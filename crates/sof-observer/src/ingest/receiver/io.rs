use super::*;

pub(super) struct UdpReceive {
    pub(super) len: usize,
    pub(super) source: SocketAddr,
    pub(super) rxq_ovfl_counter: Option<u64>,
}

pub(super) fn recv_udp_packet(
    socket: &std::net::UdpSocket,
    buffer: &mut [u8],
    track_rxq_ovfl: bool,
) -> std::io::Result<UdpReceive> {
    #[cfg(target_os = "linux")]
    if track_rxq_ovfl {
        let mut io_vectors = [IoSliceMut::new(buffer)];
        let mut cmsg_space = nix::cmsg_space!([u32; 1]);
        let message = recvmsg::<SockaddrStorage>(
            socket.as_raw_fd(),
            &mut io_vectors,
            Some(&mut cmsg_space),
            MsgFlags::empty(),
        )
        .map_err(nix_errno_to_io_error)?;
        let Some(source_storage) = message.address.as_ref() else {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "udp recvmsg missing source address",
            ));
        };
        let Some(source) = sockaddr_storage_to_socket_addr(source_storage) else {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "udp recvmsg source address is not inet/inet6",
            ));
        };
        let mut rxq_ovfl_counter: Option<u64> = None;
        if let Ok(control_messages) = message.cmsgs() {
            for control_message in control_messages {
                if let ControlMessageOwned::RxqOvfl(drop_counter) = control_message {
                    rxq_ovfl_counter = Some(u64::from(drop_counter));
                    break;
                }
            }
        }
        return Ok(UdpReceive {
            len: message.bytes,
            source,
            rxq_ovfl_counter,
        });
    }

    let (len, source) = socket.recv_from(buffer)?;
    Ok(UdpReceive {
        len,
        source,
        rxq_ovfl_counter: None,
    })
}

#[cfg(target_os = "linux")]
fn nix_errno_to_io_error(error: nix::errno::Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(error as i32)
}

#[cfg(target_os = "linux")]
fn sockaddr_storage_to_socket_addr(storage: &SockaddrStorage) -> Option<SocketAddr> {
    storage
        .as_sockaddr_in()
        .map(|address| SocketAddr::from(std::net::SocketAddrV4::from(*address)))
        .or_else(|| {
            storage
                .as_sockaddr_in6()
                .map(|address| SocketAddr::from(std::net::SocketAddrV6::from(*address)))
        })
}

pub(super) fn flush_batch(
    tx: &mpsc::Sender<RawPacketBatch>,
    batch: &mut RawPacketBatch,
    telemetry: Option<&ReceiverTelemetry>,
) {
    if batch.is_empty() {
        return;
    }
    let packet_count = batch.len();
    let outbound = std::mem::take(batch);
    match tx.try_send(outbound) {
        Ok(()) => {
            if let Some(telemetry) = telemetry {
                telemetry.record_sent_batch(packet_count);
            }
        }
        Err(mpsc::error::TrySendError::Full(_)) | Err(mpsc::error::TrySendError::Closed(_)) => {
            if let Some(telemetry) = telemetry {
                telemetry.record_dropped_batch(packet_count);
            }
        }
    }
}

pub(super) fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(u128::from(u64::MAX)) as u64
        })
}

pub(super) fn tune_udp_socket(socket: &std::net::UdpSocket) {
    let Some(rcvbuf_bytes) = read_udp_rcvbuf_bytes() else {
        return;
    };
    let sockref = SockRef::from(socket);
    if let Err(error) = sockref.set_recv_buffer_size(rcvbuf_bytes) {
        tracing::warn!(
            requested = rcvbuf_bytes,
            error = %error,
            "failed to set UDP receive buffer size"
        );
        return;
    }
    if let Ok(actual) = sockref.recv_buffer_size() {
        tracing::debug!(
            requested = rcvbuf_bytes,
            actual,
            "configured UDP receive buffer size"
        );
    }
}

pub(super) fn maybe_pin_receiver_thread(socket: &std::net::UdpSocket) {
    let local_port = socket
        .local_addr()
        .map(|address| address.port())
        .unwrap_or(0);
    let Some(core_index) = read_udp_receiver_core(local_port) else {
        return;
    };
    let Some(core_ids) = core_affinity::get_core_ids() else {
        tracing::warn!(
            core_index,
            "failed to query CPU core ids for UDP receiver pinning"
        );
        return;
    };
    let Some(core_slot) = core_index.checked_rem(core_ids.len()) else {
        tracing::warn!(
            core_index,
            "UDP receiver core index modulo failed for selected core set"
        );
        return;
    };
    let Some(core_id) = core_ids.get(core_slot).copied() else {
        tracing::warn!(
            core_index,
            "UDP receiver core index resolved to empty core set"
        );
        return;
    };
    if core_affinity::set_for_current(core_id) {
        tracing::info!(
            local_port,
            core_index,
            assigned_core = core_id.id,
            "pinned UDP receiver thread to CPU core"
        );
    } else {
        tracing::warn!(
            local_port,
            core_index,
            assigned_core = core_id.id,
            "failed to pin UDP receiver thread to CPU core"
        );
    }
}
