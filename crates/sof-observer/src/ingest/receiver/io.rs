use super::*;
use crate::ingest::config::{
    read_udp_busy_poll_budget, read_udp_busy_poll_us, read_udp_prefer_busy_poll,
};

pub(super) struct UdpReceive {
    pub(super) len: usize,
    pub(super) source: SocketAddr,
    pub(super) rxq_ovfl_counter: Option<u64>,
}

#[cfg(target_os = "linux")]
pub(super) struct UdpBatchScratch {
    buffers: Vec<[u8; 2048]>,
    io_vectors: Vec<libc::iovec>,
    addrs: Vec<libc::sockaddr_storage>,
    headers: Vec<libc::mmsghdr>,
}

#[cfg(target_os = "linux")]
impl UdpBatchScratch {
    pub(super) fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        // SAFETY: The libc socket structs are plain old data and immediately
        // initialized before each syscall use.
        let io_vectors = vec![unsafe { std::mem::zeroed() }; capacity];
        let addrs = vec![unsafe { std::mem::zeroed() }; capacity];
        let headers = vec![unsafe { std::mem::zeroed() }; capacity];
        Self {
            buffers: vec![[0_u8; 2048]; capacity],
            io_vectors,
            addrs,
            headers,
        }
    }
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
pub(super) fn recv_udp_batch(
    socket: &std::net::UdpSocket,
    scratch: &mut UdpBatchScratch,
    batch: &mut RawPacketBatch,
) -> std::io::Result<usize> {
    let capacity = scratch.buffers.len();
    for index in 0..capacity {
        scratch.io_vectors[index] = libc::iovec {
            iov_base: scratch.buffers[index].as_mut_ptr().cast(),
            iov_len: scratch.buffers[index].len(),
        };
        scratch.headers[index] = libc::mmsghdr {
            msg_hdr: libc::msghdr {
                msg_name: (&mut scratch.addrs[index]) as *mut libc::sockaddr_storage
                    as *mut libc::c_void,
                msg_namelen: std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t,
                msg_iov: (&mut scratch.io_vectors[index]) as *mut libc::iovec,
                msg_iovlen: 1,
                msg_control: std::ptr::null_mut(),
                msg_controllen: 0,
                msg_flags: 0,
            },
            msg_len: 0,
        };
    }

    // SAFETY: All message headers, names, and iovecs point to valid writable
    // memory for the duration of the syscall, and the socket fd remains live.
    let received = unsafe {
        libc::recvmmsg(
            socket.as_raw_fd(),
            scratch.headers.as_mut_ptr(),
            capacity.min(u32::MAX as usize) as u32,
            libc::MSG_WAITFORONE,
            std::ptr::null_mut(),
        )
    };
    if received < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let received = usize::try_from(received).unwrap_or(0);
    if received == 0 {
        return Ok(0);
    }

    batch.clear();
    batch.reserve(received);
    for index in 0..received {
        let len = usize::try_from(scratch.headers[index].msg_len).unwrap_or(0);
        let source =
            sockaddr_storage_to_socket_addr_libc(&scratch.addrs[index]).ok_or_else(|| {
                std::io::Error::new(
                    ErrorKind::InvalidData,
                    "udp recvmmsg source address is not inet/inet6",
                )
            })?;
        let bytes = scratch.buffers[index].get(..len).ok_or_else(|| {
            std::io::Error::new(
                ErrorKind::InvalidData,
                "udp recvmmsg returned packet larger than receive buffer",
            )
        })?;
        batch.push(RawPacket {
            source,
            ingress: RawPacketIngress::Udp,
            bytes: Arc::from(bytes),
        });
    }
    Ok(received)
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

#[cfg(target_os = "linux")]
fn sockaddr_storage_to_socket_addr_libc(storage: &libc::sockaddr_storage) -> Option<SocketAddr> {
    match i32::from(storage.ss_family) {
        libc::AF_INET => {
            // SAFETY: `ss_family` confirmed AF_INET, so reinterpret as sockaddr_in.
            let address = unsafe { &*(storage as *const _ as *const libc::sockaddr_in) };
            Some(SocketAddr::from(std::net::SocketAddrV4::new(
                std::net::Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes()),
                u16::from_be(address.sin_port),
            )))
        }
        libc::AF_INET6 => {
            // SAFETY: `ss_family` confirmed AF_INET6, so reinterpret as sockaddr_in6.
            let address = unsafe { &*(storage as *const _ as *const libc::sockaddr_in6) };
            Some(SocketAddr::from(std::net::SocketAddrV6::new(
                std::net::Ipv6Addr::from(address.sin6_addr.s6_addr),
                u16::from_be(address.sin6_port),
                address.sin6_flowinfo,
                address.sin6_scope_id,
            )))
        }
        _ => None,
    }
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn send_burst(
        sender: &std::net::UdpSocket,
        destination: SocketAddr,
        packet_count: usize,
    ) -> std::io::Result<()> {
        let payload = [7_u8; 256];
        for _ in 0..packet_count {
            sender.send_to(&payload, destination)?;
        }
        Ok(())
    }

    fn receive_legacy_burst(
        receiver: &std::net::UdpSocket,
        packet_count: usize,
    ) -> std::io::Result<usize> {
        let mut buffer = vec![0_u8; 2048];
        let mut received = 0_usize;
        while received < packet_count {
            let _packet = recv_udp_packet(receiver, &mut buffer, false)?;
            received += 1;
        }
        Ok(received)
    }

    #[test]
    fn recvmmsg_batch_matches_legacy_receive_count() {
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind receiver");
        let sender = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind sender");
        receiver
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set read timeout");
        let destination = receiver.local_addr().expect("receiver addr");
        let packet_count = 16;

        send_burst(&sender, destination, packet_count).expect("send burst");
        let mut scratch = UdpBatchScratch::new(packet_count);
        let mut batch = Vec::with_capacity(packet_count);
        let batch_received =
            recv_udp_batch(&receiver, &mut scratch, &mut batch).expect("recvmmsg batch");
        assert_eq!(batch_received, packet_count);
        assert_eq!(batch.len(), packet_count);

        send_burst(&sender, destination, packet_count).expect("send burst");
        let legacy_received = receive_legacy_burst(&receiver, packet_count).expect("legacy burst");
        assert_eq!(legacy_received, packet_count);
    }

    #[test]
    #[ignore = "profiling fixture for UDP receiver ingress"]
    fn udp_receiver_recvmmsg_profile_fixture() {
        let iterations = std::env::var("SOF_UDP_RECEIVER_PROFILE_ITERS")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(1_000);
        let packet_count = std::env::var("SOF_UDP_RECEIVER_PROFILE_BURST")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(64);

        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind receiver");
        let sender = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind sender");
        receiver
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set read timeout");
        let destination = receiver.local_addr().expect("receiver addr");
        let mut scratch = UdpBatchScratch::new(packet_count);
        let mut batch = Vec::with_capacity(packet_count);

        let legacy_started_at = Instant::now();
        for _ in 0..iterations {
            send_burst(&sender, destination, packet_count).expect("send legacy burst");
            let received = receive_legacy_burst(&receiver, packet_count).expect("receive legacy");
            assert_eq!(received, packet_count);
        }
        let legacy_elapsed = legacy_started_at.elapsed();

        let batch_started_at = Instant::now();
        for _ in 0..iterations {
            send_burst(&sender, destination, packet_count).expect("send batch burst");
            let received =
                recv_udp_batch(&receiver, &mut scratch, &mut batch).expect("receive batch");
            assert_eq!(received, packet_count);
            assert_eq!(batch.len(), packet_count);
        }
        let batch_elapsed = batch_started_at.elapsed();

        println!(
            "udp_receiver_recvmmsg_profile_fixture iterations={} burst={} legacy_us={} recvmmsg_us={}",
            iterations,
            packet_count,
            legacy_elapsed.as_micros(),
            batch_elapsed.as_micros()
        );
    }
}
