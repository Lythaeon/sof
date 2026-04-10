#![allow(clippy::indexing_slicing)]
#![allow(clippy::shadow_unrelated)]
#![allow(clippy::arithmetic_side_effects)]

use super::*;
use crate::ingest::RawPacketBatchSender;
#[cfg(test)]
use crate::ingest::config::read_udp_drop_on_channel_full;
use crate::ingest::config::{
    read_udp_busy_poll_budget, read_udp_busy_poll_us, read_udp_prefer_busy_poll,
};
#[cfg(all(target_os = "linux", test))]
use nix::poll::PollFlags;
#[cfg(target_os = "linux")]
use nix::poll::{PollFd, ppoll};
#[cfg(target_os = "linux")]
use nix::sys::time::TimeSpec;

pub(super) struct UdpReceive {
    pub(super) len: usize,
    pub(super) source: SocketAddr,
    pub(super) rxq_ovfl_counter: Option<u64>,
}

#[cfg(target_os = "linux")]
pub(super) struct UdpBatchScratch {
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
        let mut io_vectors = vec![unsafe { std::mem::zeroed() }; capacity];
        // SAFETY: The libc socket structs are plain old data and immediately
        // initialized before each syscall use.
        let mut addrs = vec![unsafe { std::mem::zeroed() }; capacity];
        // SAFETY: The libc socket structs are plain old data and immediately
        // initialized before each syscall use.
        let mut headers = vec![unsafe { std::mem::zeroed() }; capacity];
        let name_len =
            libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_storage>()).unwrap_or(0);
        for index in 0..capacity {
            headers[index] = libc::mmsghdr {
                msg_hdr: libc::msghdr {
                    msg_name: (&mut addrs[index]) as *mut libc::sockaddr_storage
                        as *mut libc::c_void,
                    msg_namelen: name_len,
                    msg_iov: (&mut io_vectors[index]) as *mut libc::iovec,
                    msg_iovlen: 1,
                    msg_control: std::ptr::null_mut(),
                    msg_controllen: 0,
                    msg_flags: 0,
                },
                msg_len: 0,
            };
        }
        Self {
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

#[cfg(test)]
#[cfg(target_os = "linux")]
pub(super) fn recv_udp_batch(
    socket: &std::net::UdpSocket,
    scratch: &mut UdpBatchScratch,
    batch: &mut RawPacketBatch,
) -> std::io::Result<usize> {
    batch.clear();
    recv_udp_batch_append(socket, scratch, batch, scratch.headers.len())
}

#[cfg(all(test, target_os = "linux"))]
fn recv_udp_batch_baseline(
    socket: &std::net::UdpSocket,
    scratch: &mut UdpBatchScratch,
    batch: &mut RawPacketBatch,
) -> std::io::Result<usize> {
    batch.clear();
    recv_udp_batch_append_baseline(socket, scratch, batch, scratch.headers.len())
}

#[cfg(target_os = "linux")]
pub(super) fn recv_udp_batch_coalesced(
    socket: &std::net::UdpSocket,
    scratch: &mut UdpBatchScratch,
    batch: &mut RawPacketBatch,
    idle_wait: Duration,
    batch_max_wait: Duration,
    poll_fd: &mut [PollFd<'_>],
) -> std::io::Result<usize> {
    batch.clear();
    let mut total_received = 0_usize;
    let deadline = Instant::now() + batch_max_wait;

    loop {
        let remaining_capacity = scratch.headers.len().saturating_sub(batch.len());
        if remaining_capacity == 0 {
            break;
        }
        match recv_udp_batch_append(socket, scratch, batch, remaining_capacity) {
            Ok(received) => {
                total_received = total_received.saturating_add(received);
                if batch.len() >= scratch.headers.len() {
                    break;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                if !wait_udp_readable(poll_fd, remaining)? {
                    break;
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                let wait = if total_received == 0 {
                    idle_wait
                } else {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    remaining
                };
                if !wait_udp_readable(poll_fd, wait)? {
                    if total_received == 0 {
                        return Err(std::io::Error::from(ErrorKind::WouldBlock));
                    }
                    break;
                }
            }
            Err(error) => return Err(error),
        }
    }

    Ok(total_received)
}

#[cfg(target_os = "linux")]
fn recv_udp_batch_append(
    socket: &std::net::UdpSocket,
    scratch: &mut UdpBatchScratch,
    batch: &mut RawPacketBatch,
    max_packets: usize,
) -> std::io::Result<usize> {
    let capacity = scratch.headers.len();
    let count = capacity.min(max_packets);
    if count == 0 {
        return Ok(0);
    }
    let start_index = batch.ensure_receive_slots(count);
    for index in 0..count {
        let buffer_index = start_index.saturating_add(index);
        let Some(buffer) = batch.receive_buffer_mut(buffer_index) else {
            return Err(std::io::Error::other(
                "raw packet batch receive buffer missing",
            ));
        };
        scratch.io_vectors[index] = libc::iovec {
            iov_base: buffer.as_mut_ptr().cast(),
            iov_len: buffer.len(),
        };
        scratch.headers[index].msg_hdr.msg_namelen =
            libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_storage>()).unwrap_or(0);
        scratch.headers[index].msg_hdr.msg_flags = 0;
        scratch.headers[index].msg_len = 0;
    }

    // SAFETY: All message headers, names, and iovecs point to valid writable
    // memory for the duration of the syscall, and the socket fd remains live.
    let received = unsafe {
        libc::recvmmsg(
            socket.as_raw_fd(),
            scratch.headers.as_mut_ptr(),
            count.min(u32::MAX as usize) as u32,
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

    batch.reserve(received);
    for index in 0..received {
        let len = usize::try_from(scratch.headers[index].msg_len).unwrap_or(0);
        let buffer_index = start_index.saturating_add(index);
        let source = sockaddr_storage_to_socket_addr_libc(
            &scratch.addrs[index],
            scratch.headers[index].msg_hdr.msg_namelen,
        )
        .ok_or_else(|| {
            std::io::Error::new(
                ErrorKind::InvalidData,
                "udp recvmmsg source address is not inet/inet6",
            )
        })?;
        batch
            .push_received_metadata(source, RawPacketIngress::Udp, buffer_index, len)
            .map_err(|error| match error {
                UdpReceiverError::InvalidPacketLength { len, capacity } => std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "udp recvmmsg returned packet length {len} beyond buffer capacity {capacity}"
                    ),
                ),
                UdpReceiverError::Receive { source: io_error } => io_error,
                UdpReceiverError::BindSocket { .. }
                | UdpReceiverError::SetBlockingMode { .. }
                | UdpReceiverError::SetReadTimeout { .. } => std::io::Error::new(
                    ErrorKind::InvalidData,
                    "udp recvmmsg packet push failed",
                ),
            })?;
    }
    Ok(received)
}

#[cfg(all(test, target_os = "linux"))]
fn recv_udp_batch_append_baseline(
    socket: &std::net::UdpSocket,
    scratch: &mut UdpBatchScratch,
    batch: &mut RawPacketBatch,
    max_packets: usize,
) -> std::io::Result<usize> {
    let capacity = scratch.headers.len();
    let count = capacity.min(max_packets);
    if count == 0 {
        return Ok(0);
    }
    let start_index = batch.ensure_receive_slots(count);
    for index in 0..count {
        let buffer_index = start_index.saturating_add(index);
        let Some(buffer) = batch.receive_buffer_mut(buffer_index) else {
            return Err(std::io::Error::other(
                "raw packet batch receive buffer missing",
            ));
        };
        scratch.io_vectors[index] = libc::iovec {
            iov_base: buffer.as_mut_ptr().cast(),
            iov_len: buffer.len(),
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
            count.min(u32::MAX as usize) as u32,
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

    batch.reserve(received);
    for index in 0..received {
        let len = usize::try_from(scratch.headers[index].msg_len).unwrap_or(0);
        let buffer_index = start_index.saturating_add(index);
        let source = sockaddr_storage_to_socket_addr_libc(
            &scratch.addrs[index],
            scratch.headers[index].msg_hdr.msg_namelen,
        )
        .ok_or_else(|| {
            std::io::Error::new(
                ErrorKind::InvalidData,
                "udp recvmmsg source address is not inet/inet6",
            )
        })?;
        batch
            .push_received_metadata(source, RawPacketIngress::Udp, buffer_index, len)
            .map_err(|error| match error {
                UdpReceiverError::InvalidPacketLength { len, capacity } => std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "udp recvmmsg returned packet length {len} beyond buffer capacity {capacity}"
                    ),
                ),
                UdpReceiverError::Receive { source: io_error } => io_error,
                UdpReceiverError::BindSocket { .. }
                | UdpReceiverError::SetBlockingMode { .. }
                | UdpReceiverError::SetReadTimeout { .. } => std::io::Error::new(
                    ErrorKind::InvalidData,
                    "udp recvmmsg packet push failed",
                ),
            })?;
    }
    Ok(received)
}

#[cfg(target_os = "linux")]
fn wait_udp_readable(poll_fd: &mut [PollFd<'_>], timeout: Duration) -> std::io::Result<bool> {
    if timeout.is_zero() {
        return Ok(false);
    }
    Ok(ppoll(poll_fd, Some(TimeSpec::from_duration(timeout)), None)? > 0)
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
fn sockaddr_storage_to_socket_addr_libc(
    storage: &libc::sockaddr_storage,
    namelen: libc::socklen_t,
) -> Option<SocketAddr> {
    let namelen = usize::try_from(namelen).ok()?;
    match i32::from(storage.ss_family) {
        libc::AF_INET => {
            if namelen < std::mem::size_of::<libc::sockaddr_in>() {
                return None;
            }
            // SAFETY: `ss_family` confirmed AF_INET, so reinterpret as sockaddr_in.
            let address = unsafe { &*(storage as *const _ as *const libc::sockaddr_in) };
            Some(SocketAddr::from(std::net::SocketAddrV4::new(
                std::net::Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes()),
                u16::from_be(address.sin_port),
            )))
        }
        libc::AF_INET6 => {
            if namelen < std::mem::size_of::<libc::sockaddr_in6>() {
                return None;
            }
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
    tx: &RawPacketBatchSender,
    batch: &mut RawPacketBatch,
    drop_on_full: bool,
    telemetry: Option<&ReceiverTelemetry>,
) {
    flush_batch_inner(tx, batch, drop_on_full, telemetry);
}

fn flush_batch_inner(
    tx: &RawPacketBatchSender,
    batch: &mut RawPacketBatch,
    drop_on_full: bool,
    telemetry: Option<&ReceiverTelemetry>,
) {
    if batch.is_empty() {
        return;
    }
    let packet_count = batch.len();
    let outbound = batch.take_for_send();
    if tx.send_batch(outbound, drop_on_full) {
        if let Some(telemetry) = telemetry {
            telemetry.record_sent_batch(packet_count);
        }
    } else if let Some(telemetry) = telemetry {
        telemetry.record_dropped_batch(packet_count);
    }
}

#[cfg(test)]
fn flush_batch_baseline(
    tx: &RawPacketBatchSender,
    batch: &mut RawPacketBatch,
    telemetry: Option<&ReceiverTelemetry>,
) {
    let drop_on_full = read_udp_drop_on_channel_full();
    flush_batch_inner(tx, batch, drop_on_full, telemetry);
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
    use std::thread;

    use crate::ingest::create_raw_packet_batch_queue;
    use crate::runtime_env::with_runtime_env_overrides_for_test;
    use sof_support::{bench::avg_ns_per_iteration, env_support::read_positive_usize};

    #[derive(Debug)]
    struct LegacyRawPacket {
        _source: SocketAddr,
        _bytes: Arc<[u8]>,
    }

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

    fn send_staggered_burst(
        sender: std::net::UdpSocket,
        destination: SocketAddr,
        packet_count: usize,
        packets_per_chunk: usize,
        gap: Duration,
    ) -> std::thread::JoinHandle<std::io::Result<()>> {
        thread::spawn(move || {
            let payload = [9_u8; 256];
            let mut sent = 0_usize;
            while sent < packet_count {
                let chunk = packets_per_chunk.min(packet_count.saturating_sub(sent));
                for _ in 0..chunk {
                    sender.send_to(&payload, destination)?;
                }
                sent = sent.saturating_add(chunk);
                if sent < packet_count {
                    thread::sleep(gap);
                }
            }
            Ok(())
        })
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
        let mut batch = RawPacketBatch::with_capacity(packet_count);
        let batch_received =
            recv_udp_batch(&receiver, &mut scratch, &mut batch).expect("recvmmsg batch");
        assert_eq!(batch_received, packet_count);
        assert_eq!(batch.len(), packet_count);

        send_burst(&sender, destination, packet_count).expect("send burst");
        let legacy_received = receive_legacy_burst(&receiver, packet_count).expect("legacy burst");
        assert_eq!(legacy_received, packet_count);
    }

    #[test]
    fn recvmmsg_source_address_rejects_truncated_name() {
        // SAFETY: The test initializes the family tag before conversion.
        let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        storage.ss_family = libc::AF_INET as libc::sa_family_t;

        let truncated = std::mem::size_of::<libc::sockaddr_in>().saturating_sub(1);
        let namelen = libc::socklen_t::try_from(truncated).unwrap_or(0);
        assert!(sockaddr_storage_to_socket_addr_libc(&storage, namelen).is_none());
    }

    #[test]
    #[ignore = "profiling fixture for UDP receiver ingress"]
    fn udp_receiver_recvmmsg_profile_fixture() {
        let iterations = read_positive_usize("SOF_UDP_RECEIVER_PROFILE_ITERS", 1_000);
        let packet_count = read_positive_usize("SOF_UDP_RECEIVER_PROFILE_BURST", 64);

        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind receiver");
        let sender = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind sender");
        receiver
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set read timeout");
        let destination = receiver.local_addr().expect("receiver addr");
        let mut scratch = UdpBatchScratch::new(packet_count);
        let mut batch = RawPacketBatch::with_capacity(packet_count);

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
        let legacy_avg_ns = avg_ns_per_iteration(legacy_elapsed, iterations);
        let batch_avg_ns = avg_ns_per_iteration(batch_elapsed, iterations);

        println!(
            "udp_receiver_recvmmsg_profile_fixture iterations={} burst={} legacy_us={} legacy_avg_ns_per_iteration={} legacy_avg_us_per_iteration={:.3} recvmmsg_us={} recvmmsg_avg_ns_per_iteration={} recvmmsg_avg_us_per_iteration={:.3}",
            iterations,
            packet_count,
            legacy_elapsed.as_micros(),
            legacy_avg_ns,
            legacy_avg_ns as f64 / 1_000.0,
            batch_elapsed.as_micros(),
            batch_avg_ns,
            batch_avg_ns as f64 / 1_000.0
        );
    }

    #[test]
    #[ignore = "profiling fixture for UDP receiver recvmmsg setup A/B"]
    fn udp_receiver_recvmmsg_setup_profile_fixture() {
        let iterations = read_positive_usize("SOF_UDP_RECEIVER_PROFILE_ITERS", 1_000);
        let packet_count = read_positive_usize("SOF_UDP_RECEIVER_PROFILE_BURST", 64);

        let baseline_receiver = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind receiver");
        let sender = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind sender");
        baseline_receiver
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set read timeout");
        let baseline_destination = baseline_receiver.local_addr().expect("receiver addr");
        let mut baseline_scratch = UdpBatchScratch::new(packet_count);
        let mut baseline_batch = RawPacketBatch::with_capacity(packet_count);

        let baseline_started_at = Instant::now();
        for _ in 0..iterations {
            send_burst(&sender, baseline_destination, packet_count).expect("send baseline burst");
            let received = recv_udp_batch_baseline(
                &baseline_receiver,
                &mut baseline_scratch,
                &mut baseline_batch,
            )
            .expect("receive baseline batch");
            assert_eq!(received, packet_count);
            assert_eq!(baseline_batch.len(), packet_count);
        }
        let baseline_elapsed = baseline_started_at.elapsed();

        let optimized_receiver = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind receiver");
        optimized_receiver
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set read timeout");
        let optimized_destination = optimized_receiver.local_addr().expect("receiver addr");
        let mut optimized_scratch = UdpBatchScratch::new(packet_count);
        let mut optimized_batch = RawPacketBatch::with_capacity(packet_count);

        let optimized_started_at = Instant::now();
        for _ in 0..iterations {
            send_burst(&sender, optimized_destination, packet_count).expect("send optimized burst");
            let received = recv_udp_batch(
                &optimized_receiver,
                &mut optimized_scratch,
                &mut optimized_batch,
            )
            .expect("receive optimized batch");
            assert_eq!(received, packet_count);
            assert_eq!(optimized_batch.len(), packet_count);
        }
        let optimized_elapsed = optimized_started_at.elapsed();
        let baseline_avg_ns = avg_ns_per_iteration(baseline_elapsed, iterations);
        let optimized_avg_ns = avg_ns_per_iteration(optimized_elapsed, iterations);

        println!(
            "udp_receiver_recvmmsg_setup_profile_fixture iterations={} burst={} baseline_us={} optimized_us={} baseline_avg_ns_per_iteration={} optimized_avg_ns_per_iteration={} baseline_avg_us_per_iteration={:.3} optimized_avg_us_per_iteration={:.3}",
            iterations,
            packet_count,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
            baseline_avg_ns,
            optimized_avg_ns,
            baseline_avg_ns as f64 / 1_000.0,
            optimized_avg_ns as f64 / 1_000.0
        );
    }

    #[test]
    #[ignore = "profiling fixture for UDP receiver flush-path config lookup"]
    fn udp_receiver_flush_batch_profile_fixture() {
        let iterations = read_positive_usize("SOF_UDP_RECEIVER_PROFILE_ITERS", 10_000);
        let capacity = iterations.saturating_mul(2).max(1).to_string();

        with_runtime_env_overrides_for_test(
            [("SOF_INGEST_QUEUE_CAPACITY".to_owned(), capacity)],
            || {
                let source: SocketAddr = "127.0.0.1:8899".parse().expect("source addr");
                let payload = [11_u8; 256];
                let recycler = RawPacketBatch::recycler_for_tests(1);

                let (baseline_tx, _baseline_rx) = create_raw_packet_batch_queue();
                let baseline_started_at = Instant::now();
                for _ in 0..iterations {
                    let mut batch = RawPacketBatch::from_recycler_for_tests(&recycler);
                    batch
                        .push_packet(source, RawPacketIngress::Udp, &payload)
                        .expect("push packet");
                    flush_batch_baseline(&baseline_tx, &mut batch, None);
                }
                let baseline_elapsed = baseline_started_at.elapsed();

                let (optimized_tx, _optimized_rx) = create_raw_packet_batch_queue();
                let drop_on_full = read_udp_drop_on_channel_full();
                let optimized_started_at = Instant::now();
                for _ in 0..iterations {
                    let mut batch = RawPacketBatch::from_recycler_for_tests(&recycler);
                    batch
                        .push_packet(source, RawPacketIngress::Udp, &payload)
                        .expect("push packet");
                    flush_batch(&optimized_tx, &mut batch, drop_on_full, None);
                }
                let optimized_elapsed = optimized_started_at.elapsed();
                let baseline_avg_ns = avg_ns_per_iteration(baseline_elapsed, iterations);
                let optimized_avg_ns = avg_ns_per_iteration(optimized_elapsed, iterations);

                println!(
                    "udp_receiver_flush_batch_profile_fixture iterations={} baseline_us={} optimized_us={} baseline_avg_ns_per_iteration={} optimized_avg_ns_per_iteration={} baseline_avg_us_per_iteration={:.3} optimized_avg_us_per_iteration={:.3}",
                    iterations,
                    baseline_elapsed.as_micros(),
                    optimized_elapsed.as_micros(),
                    baseline_avg_ns,
                    optimized_avg_ns,
                    baseline_avg_ns as f64 / 1_000.0,
                    optimized_avg_ns as f64 / 1_000.0
                );
            },
        );
    }

    #[test]
    #[ignore = "profiling fixture for UDP receiver coalesced ingress"]
    fn udp_receiver_recvmmsg_coalesced_profile_fixture() {
        use std::os::fd::AsFd;

        let iterations = read_positive_usize("SOF_UDP_RECEIVER_PROFILE_ITERS", 1_000);
        let packet_count = read_positive_usize("SOF_UDP_RECEIVER_PROFILE_BURST", 64);
        let chunk_size = read_positive_usize("SOF_UDP_RECEIVER_PROFILE_CHUNK", 8);
        let gap_us = u64::try_from(read_positive_usize("SOF_UDP_RECEIVER_PROFILE_GAP_US", 100))
            .unwrap_or(100);
        let gap = Duration::from_micros(gap_us);
        let idle_wait = Duration::from_millis(200);
        let batch_max_wait = Duration::from_millis(2);

        let blocking_receiver = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind receiver");
        let sender = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind sender");
        blocking_receiver
            .set_read_timeout(Some(idle_wait))
            .expect("set read timeout");
        let destination = blocking_receiver.local_addr().expect("receiver addr");

        let mut scratch = UdpBatchScratch::new(packet_count);
        let mut batch = RawPacketBatch::with_capacity(packet_count);
        let blocking_started_at = Instant::now();
        for _ in 0..iterations {
            let sender_thread = send_staggered_burst(
                sender.try_clone().expect("clone sender"),
                destination,
                packet_count,
                chunk_size,
                gap,
            );
            let mut received = 0_usize;
            while received < packet_count {
                received = received.saturating_add(
                    recv_udp_batch(&blocking_receiver, &mut scratch, &mut batch)
                        .expect("receive blocking batch"),
                );
            }
            sender_thread
                .join()
                .expect("join sender")
                .expect("send staggered burst");
        }
        let blocking_elapsed = blocking_started_at.elapsed();

        let coalesced_receiver =
            std::net::UdpSocket::bind("127.0.0.1:0").expect("bind coalesced receiver");
        let destination = coalesced_receiver
            .local_addr()
            .expect("coalesced receiver addr");
        coalesced_receiver
            .set_nonblocking(true)
            .expect("set nonblocking");
        let mut scratch = UdpBatchScratch::new(packet_count);
        let mut batch = RawPacketBatch::with_capacity(packet_count);
        let mut poll_fd = [PollFd::new(coalesced_receiver.as_fd(), PollFlags::POLLIN)];
        let coalesced_started_at = Instant::now();
        for _ in 0..iterations {
            let sender_thread = send_staggered_burst(
                sender.try_clone().expect("clone sender"),
                destination,
                packet_count,
                chunk_size,
                gap,
            );
            let received = recv_udp_batch_coalesced(
                &coalesced_receiver,
                &mut scratch,
                &mut batch,
                idle_wait,
                batch_max_wait,
                &mut poll_fd,
            )
            .expect("receive coalesced batch");
            assert_eq!(received, packet_count);
            assert_eq!(batch.len(), packet_count);
            sender_thread
                .join()
                .expect("join sender")
                .expect("send staggered burst");
        }
        let coalesced_elapsed = coalesced_started_at.elapsed();
        let blocking_avg_ns = avg_ns_per_iteration(blocking_elapsed, iterations);
        let coalesced_avg_ns = avg_ns_per_iteration(coalesced_elapsed, iterations);

        println!(
            "udp_receiver_recvmmsg_coalesced_profile_fixture iterations={} burst={} chunk={} gap_us={} immediate_us={} immediate_avg_ns_per_iteration={} immediate_avg_us_per_iteration={:.3} coalesced_us={} coalesced_avg_ns_per_iteration={} coalesced_avg_us_per_iteration={:.3}",
            iterations,
            packet_count,
            chunk_size,
            gap_us,
            blocking_elapsed.as_micros(),
            blocking_avg_ns,
            blocking_avg_ns as f64 / 1_000.0,
            coalesced_elapsed.as_micros(),
            coalesced_avg_ns,
            coalesced_avg_ns as f64 / 1_000.0
        );
    }

    #[test]
    #[ignore = "profiling fixture for contiguous raw packet batch materialization"]
    fn udp_receiver_batch_materialization_profile_fixture() {
        let iterations = read_positive_usize("SOF_UDP_RECEIVER_PROFILE_ITERS", 20_000);
        let packet_count = read_positive_usize("SOF_UDP_RECEIVER_PROFILE_BURST", 64);
        let source: SocketAddr = "127.0.0.1:8899".parse().expect("source addr");
        let payloads: Vec<Vec<u8>> = (0..packet_count)
            .map(|index| vec![u8::try_from(index % 251).unwrap_or(0); 256])
            .collect();

        let legacy_started_at = Instant::now();
        for _ in 0..iterations {
            let mut batch = Vec::with_capacity(packet_count);
            for payload in &payloads {
                batch.push(LegacyRawPacket {
                    _source: source,
                    _bytes: Arc::from(payload.as_slice()),
                });
            }
            assert_eq!(batch.len(), packet_count);
        }
        let legacy_elapsed = legacy_started_at.elapsed();

        let contiguous_started_at = Instant::now();
        for _ in 0..iterations {
            let mut batch = RawPacketBatch::with_capacity(packet_count);
            for payload in &payloads {
                batch
                    .push_packet(source, RawPacketIngress::Udp, payload)
                    .expect("push packet");
            }
            assert_eq!(batch.len(), packet_count);
        }
        let contiguous_elapsed = contiguous_started_at.elapsed();

        let recycler = RawPacketBatch::recycler_for_tests(packet_count);
        let recycled_started_at = Instant::now();
        for _ in 0..iterations {
            let mut batch = RawPacketBatch::from_recycler_for_tests(&recycler);
            for payload in &payloads {
                batch
                    .push_packet(source, RawPacketIngress::Udp, payload)
                    .expect("push packet");
            }
            assert_eq!(batch.len(), packet_count);
        }
        let recycled_elapsed = recycled_started_at.elapsed();
        let legacy_avg_ns = avg_ns_per_iteration(legacy_elapsed, iterations);
        let contiguous_avg_ns = avg_ns_per_iteration(contiguous_elapsed, iterations);
        let recycled_avg_ns = avg_ns_per_iteration(recycled_elapsed, iterations);

        println!(
            "udp_receiver_batch_materialization_profile_fixture iterations={} burst={} legacy_arc_us={} legacy_arc_avg_ns_per_iteration={} legacy_arc_avg_us_per_iteration={:.3} contiguous_us={} contiguous_avg_ns_per_iteration={} contiguous_avg_us_per_iteration={:.3} recycled_us={} recycled_avg_ns_per_iteration={} recycled_avg_us_per_iteration={:.3}",
            iterations,
            packet_count,
            legacy_elapsed.as_micros(),
            legacy_avg_ns,
            legacy_avg_ns as f64 / 1_000.0,
            contiguous_elapsed.as_micros(),
            contiguous_avg_ns,
            contiguous_avg_ns as f64 / 1_000.0,
            recycled_elapsed.as_micros(),
            recycled_avg_ns,
            recycled_avg_ns as f64 / 1_000.0
        );
    }

    #[test]
    #[ignore = "profiling fixture for recycled raw packet batch materialization"]
    fn udp_receiver_batch_recycler_profile_fixture() {
        let iterations = read_positive_usize("SOF_UDP_RECEIVER_PROFILE_ITERS", 50_000);
        let packet_count = read_positive_usize("SOF_UDP_RECEIVER_PROFILE_BURST", 64);
        let source: SocketAddr = "127.0.0.1:8899".parse().expect("source addr");
        let payloads: Vec<Vec<u8>> = (0..packet_count)
            .map(|index| vec![u8::try_from(index % 251).unwrap_or(0); 256])
            .collect();
        let recycler = RawPacketBatch::recycler_for_tests(packet_count);

        let started_at = Instant::now();
        for _ in 0..iterations {
            let mut batch = RawPacketBatch::from_recycler_for_tests(&recycler);
            for payload in &payloads {
                batch
                    .push_packet(source, RawPacketIngress::Udp, payload)
                    .expect("push packet");
            }
            assert_eq!(batch.len(), packet_count);
        }
        let elapsed = started_at.elapsed();
        let avg_ns = avg_ns_per_iteration(elapsed, iterations);

        println!(
            "udp_receiver_batch_recycler_profile_fixture iterations={} burst={} recycled_us={} recycled_avg_ns_per_iteration={} recycled_avg_us_per_iteration={:.3}",
            iterations,
            packet_count,
            elapsed.as_micros(),
            avg_ns,
            avg_ns as f64 / 1_000.0
        );
    }
}
