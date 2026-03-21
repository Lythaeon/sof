use super::*;
use crate::ingest::config::{
    read_udp_busy_poll_budget, read_udp_busy_poll_us, read_udp_prefer_busy_poll,
};

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
    tx: &crate::ingest::RawPacketBatchSender,
    batch: &mut RawPacketBatch,
    telemetry: Option<&ReceiverTelemetry>,
) {
    if batch.is_empty() {
        return;
    }
    let packet_count = batch.len();
    let outbound = std::mem::take(batch);
    let drop_on_full = crate::ingest::config::read_udp_drop_on_channel_full();
    if tx.send_batch(outbound, drop_on_full) {
        if let Some(telemetry) = telemetry {
            telemetry.record_sent_batch(packet_count);
        }
    } else if let Some(telemetry) = telemetry {
        telemetry.record_dropped_batch(packet_count);
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
        tune_udp_busy_poll(socket);
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
    tune_udp_busy_poll(socket);
}

#[cfg(target_os = "linux")]
fn tune_udp_busy_poll(socket: &std::net::UdpSocket) {
    const SO_BUSY_POLL: libc::c_int = 46;
    const SO_PREFER_BUSY_POLL: libc::c_int = 69;
    const SO_BUSY_POLL_BUDGET: libc::c_int = 70;

    let busy_poll_us = read_udp_busy_poll_us();
    let prefer_busy_poll = read_udp_prefer_busy_poll();
    let busy_poll_budget = read_udp_busy_poll_budget();
    if busy_poll_us.is_none() && !prefer_busy_poll && busy_poll_budget.is_none() {
        return;
    }

    if let Some(timeout_us) = busy_poll_us {
        set_udp_socket_int_sockopt(
            socket,
            SO_BUSY_POLL,
            timeout_us as libc::c_int,
            "SO_BUSY_POLL",
        );
    }
    if prefer_busy_poll {
        set_udp_socket_int_sockopt(socket, SO_PREFER_BUSY_POLL, 1, "SO_PREFER_BUSY_POLL");
    }
    if let Some(packet_budget) = busy_poll_budget {
        set_udp_socket_int_sockopt(
            socket,
            SO_BUSY_POLL_BUDGET,
            packet_budget as libc::c_int,
            "SO_BUSY_POLL_BUDGET",
        );
    }

    tracing::info!(
        local_addr = ?socket.local_addr().ok(),
        busy_poll_us,
        prefer_busy_poll,
        busy_poll_budget,
        "configured UDP busy-poll socket options"
    );
}

#[cfg(not(target_os = "linux"))]
fn tune_udp_busy_poll(_socket: &std::net::UdpSocket) {}

#[cfg(target_os = "linux")]
fn set_udp_socket_int_sockopt(
    socket: &std::net::UdpSocket,
    option_name: libc::c_int,
    option_value: libc::c_int,
    option_label: &str,
) {
    // SAFETY: `socket.as_raw_fd()` is a live UDP socket, `option_value` points to a valid
    // `c_int`, and the provided length matches that value's size.
    let result = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            option_name,
            &option_value as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if result == 0 {
        return;
    }
    let error = std::io::Error::last_os_error();
    tracing::warn!(
        option = option_label,
        value = option_value,
        error = %error,
        local_addr = ?socket.local_addr().ok(),
        "failed to configure UDP socket option"
    );
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
