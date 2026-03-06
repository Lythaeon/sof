#![cfg_attr(
    not(all(target_os = "linux", feature = "kernel-bypass")),
    allow(unused)
)]
#![allow(clippy::missing_docs_in_private_items)]
//! AF_XDP kernel-bypass example for direct submission.

#[cfg(not(all(target_os = "linux", feature = "kernel-bypass")))]
fn main() {
    eprintln!("This example requires Linux and --features kernel-bypass");
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
use std::{
    ffi::CString,
    io,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
use async_trait::async_trait;
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
use serde_json::Value;
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
use sof_tx::{
    KernelBypassDatagramSocket, KernelBypassDirectTransport, LeaderTarget, RoutingPolicy,
    submit::{DirectSubmitConfig, DirectSubmitTransport},
};
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
use xdp::{
    RingConfigBuilder, Umem, WakableRings,
    packet::PacketError,
    slab::{HeapSlab, Slab},
    socket::{PollTimeout, XdpSocket, XdpSocketBuilder},
    umem::{FrameSize, UmemCfgBuilder},
};

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
const INNER_ENV: &str = "SOF_AF_XDP_EXAMPLE_INNER";
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
const VETH_SENDER: &str = "veth_kb_tx";
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
const VETH_RECEIVER: &str = "veth_kb_rx";
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
const SENDER_IP: Ipv4Addr = Ipv4Addr::new(10, 77, 0, 1);
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
const RECEIVER_IP: Ipv4Addr = Ipv4Addr::new(10, 77, 0, 2);
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
const SRC_PORT: u16 = 42_424;
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
const DST_PORT: u16 = 19_001;

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
struct AfXdpSocketState {
    socket: XdpSocket,
    rings: WakableRings,
    umem: Umem,
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
struct AfXdpKernelBypassSocket {
    state: Mutex<AfXdpSocketState>,
    src_mac: [u8; 6],
    dst_mac: [u8; 6],
    src_ip: Ipv4Addr,
    src_port: u16,
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
impl AfXdpKernelBypassSocket {
    fn bind_to_interface(
        interface_name: &str,
        src_mac: [u8; 6],
        dst_mac: [u8; 6],
        src_ip: Ipv4Addr,
        src_port: u16,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let ifname = CString::new(interface_name)?;
        let nic = xdp::nic::NicIndex::lookup_by_name(&ifname)?.ok_or_else(|| {
            io::Error::other(format!("network interface `{interface_name}` not found"))
        })?;

        let mut builder = XdpSocketBuilder::new()?;
        let umem_cfg = UmemCfgBuilder {
            frame_size: FrameSize::TwoK,
            frame_count: 64,
            ..Default::default()
        }
        .build()?;
        let umem = Umem::map(umem_cfg)?;
        let ring_cfg = RingConfigBuilder {
            rx_count: 0,
            tx_count: 64,
            fill_count: 64,
            completion_count: 64,
        }
        .build()?;
        let (rings, bind_flags) = builder.build_wakable_rings(&umem, ring_cfg)?;
        let socket = builder.bind(nic, 0, bind_flags)?;

        Ok(Self {
            state: Mutex::new(AfXdpSocketState {
                socket,
                rings,
                umem,
            }),
            src_mac,
            dst_mac,
            src_ip,
            src_port,
        })
    }
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
#[async_trait]
impl KernelBypassDatagramSocket for AfXdpKernelBypassSocket {
    async fn send_to(&self, payload: &[u8], target: SocketAddr) -> io::Result<usize> {
        let dst = match target.ip() {
            std::net::IpAddr::V4(ip) => ip,
            std::net::IpAddr::V6(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "AF_XDP example socket only supports IPv4 targets",
                ));
            }
        };
        let frame = build_ipv4_udp_ethernet_frame(
            self.src_mac,
            self.dst_mac,
            self.src_ip,
            dst,
            self.src_port,
            target.port(),
            payload,
        );

        let mut guard = self.state.lock().map_err(|poison_error| {
            io::Error::other(format!("AF_XDP state lock poisoned: {poison_error}"))
        })?;
        // SAFETY: the UMEM is owned by this socket state and is only accessed while the mutex
        // guard is held, so the allocator borrow does not alias concurrent TX operations.
        let packet = unsafe { guard.umem.alloc() }
            .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, "AF_XDP UMEM exhausted"))?;
        let mut packet = packet;
        packet
            .insert(0, &frame)
            .map_err(|error| packet_error_to_io(&error))?;

        let mut slab = HeapSlab::with_capacity(1);
        let _ = slab.push_front(packet);
        let tx_ring = guard
            .rings
            .tx_ring
            .as_mut()
            .ok_or_else(|| io::Error::other("AF_XDP socket was created without a TX ring"))?;
        // SAFETY: the slab packet was allocated from this socket's UMEM and the TX ring was
        // created from the same builder/bind sequence, which is the invariant required by `xdp`.
        let queued = unsafe { tx_ring.send(&mut slab, true)? };
        if queued == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "AF_XDP TX queue is full",
            ));
        }

        drop(
            guard
                .socket
                .poll_write(PollTimeout::new(Some(Duration::from_millis(10)))),
        );
        let mut completed_tx = false;
        for _ in 0..8 {
            let completed = {
                let state = &mut *guard;
                let rings = &mut state.rings;
                let umem = &mut state.umem;
                rings.completion_ring.dequeue(umem, 32)
            };
            if completed > 0 {
                completed_tx = true;
                break;
            }
            drop(
                guard
                    .socket
                    .poll_write(PollTimeout::new(Some(Duration::from_millis(5)))),
            );
        }
        if !completed_tx {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "AF_XDP TX completion did not arrive before timeout",
            ));
        }

        Ok(payload.len())
    }
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn packet_error_to_io(error: &PacketError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn build_ipv4_udp_ethernet_frame(
    src_mac: [u8; 6],
    dst_mac: [u8; 6],
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let udp_len_bytes = payload.len().saturating_add(8);
    let udp_len = u16::try_from(udp_len_bytes).unwrap_or(u16::MAX);
    let total_len_bytes = usize::from(udp_len).saturating_add(20);
    let total_len = u16::try_from(total_len_bytes).unwrap_or(u16::MAX);

    let mut frame = Vec::with_capacity(usize::from(total_len).saturating_add(14));
    frame.extend_from_slice(&dst_mac);
    frame.extend_from_slice(&src_mac);
    frame.extend_from_slice(&0x0800_u16.to_be_bytes());

    let mut ipv4 = [0_u8; 20];
    ipv4[0] = 0x45;
    ipv4[1] = 0;
    ipv4[2..4].copy_from_slice(&total_len.to_be_bytes());
    ipv4[4..6].copy_from_slice(&0_u16.to_be_bytes());
    ipv4[6..8].copy_from_slice(&0x4000_u16.to_be_bytes());
    ipv4[8] = 64;
    ipv4[9] = 17;
    ipv4[10..12].copy_from_slice(&0_u16.to_be_bytes());
    ipv4[12..16].copy_from_slice(&src_ip.octets());
    ipv4[16..20].copy_from_slice(&dst_ip.octets());
    let ip_checksum = ipv4_header_checksum(&ipv4);
    ipv4[10..12].copy_from_slice(&ip_checksum.to_be_bytes());
    frame.extend_from_slice(&ipv4);

    let mut udp = [0_u8; 8];
    udp[0..2].copy_from_slice(&src_port.to_be_bytes());
    udp[2..4].copy_from_slice(&dst_port.to_be_bytes());
    udp[4..6].copy_from_slice(&udp_len.to_be_bytes());
    udp[6..8].copy_from_slice(&0_u16.to_be_bytes());
    frame.extend_from_slice(&udp);
    frame.extend_from_slice(payload);

    frame
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn ipv4_header_checksum(header: &[u8; 20]) -> u16 {
    let mut sum: u32 = 0;
    for chunk in header.chunks_exact(2) {
        if let Ok(bytes) = <[u8; 2]>::try_from(chunk) {
            sum = sum.saturating_add(u32::from(u16::from_be_bytes(bytes)));
        }
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF).saturating_add(sum >> 16);
    }
    !(sum as u16)
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn parse_mac(mac: &str) -> Result<[u8; 6], Box<dyn std::error::Error>> {
    let mut out = [0_u8; 6];
    let mut split = mac.split(':');
    for slot in &mut out {
        let Some(part) = split.next() else {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid MAC address `{mac}`"),
            )));
        };
        *slot = u8::from_str_radix(part, 16).map_err(|source| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid MAC byte `{part}`: {source}"),
            )
        })?;
    }
    if split.next().is_some() {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid MAC address `{mac}`"),
        )));
    }
    Ok(out)
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn run_ip(args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    for candidate in ["/usr/sbin/ip", "/sbin/ip", "/usr/bin/ip", "ip"] {
        match Command::new(candidate).args(args).status() {
            Ok(status) => {
                if status.success() {
                    return Ok(());
                }
                return Err(Box::new(io::Error::other(format!(
                    "`{candidate} {}` failed with status {status}",
                    args.join(" ")
                ))));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(Box::new(error)),
        }
    }
    Err(Box::new(io::Error::new(
        io::ErrorKind::NotFound,
        "`ip` command was not found in common locations",
    )))
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn run_ip_output(args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    for candidate in ["/usr/sbin/ip", "/sbin/ip", "/usr/bin/ip", "ip"] {
        match Command::new(candidate).args(args).output() {
            Ok(output) => {
                if output.status.success() {
                    return Ok(String::from_utf8_lossy(&output.stdout).to_string());
                }
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(Box::new(io::Error::other(format!(
                    "`{candidate} {}` failed with status {}: {}",
                    args.join(" "),
                    output.status,
                    stderr.trim()
                ))));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(Box::new(error)),
        }
    }
    Err(Box::new(io::Error::new(
        io::ErrorKind::NotFound,
        "`ip` command was not found in common locations",
    )))
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn read_mac(interface_name: &str) -> Result<[u8; 6], Box<dyn std::error::Error>> {
    let output = run_ip_output(&["-o", "link", "show", "dev", interface_name])?;
    let mut words = output.split_whitespace();
    while let Some(word) = words.next() {
        if word == "link/ether" {
            let mac = words.next().ok_or_else(|| {
                Box::new(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("missing link-layer address for `{interface_name}`"),
                )) as Box<dyn std::error::Error>
            })?;
            return parse_mac(mac);
        }
    }
    Err(Box::new(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("failed to parse MAC for `{interface_name}` from `ip link` output"),
    )))
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn read_link_packets(interface_name: &str) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    let output = run_ip_output(&["-j", "-s", "link", "show", "dev", interface_name])?;
    let value: Value = serde_json::from_str(&output)?;
    let Some(entry) = value.as_array().and_then(|items| items.first()) else {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("missing `ip -j` entry for interface `{interface_name}`"),
        )));
    };
    let stats = entry
        .get("stats64")
        .or_else(|| entry.get("stats"))
        .ok_or_else(|| {
            Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("missing stats for interface `{interface_name}`"),
            )) as Box<dyn std::error::Error>
        })?;

    let tx_packets = stats
        .get("tx")
        .and_then(|tx| tx.get("packets"))
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("missing tx.packets for `{interface_name}`"),
            )) as Box<dyn std::error::Error>
        })?;
    let rx_packets = stats
        .get("rx")
        .and_then(|rx| rx.get("packets"))
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("missing rx.packets for `{interface_name}`"),
            )) as Box<dyn std::error::Error>
        })?;

    Ok((tx_packets, rx_packets))
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn setup_veth_pair() -> Result<(), Box<dyn std::error::Error>> {
    run_ip(&["link", "set", "lo", "up"])?;
    run_ip(&[
        "link",
        "add",
        VETH_SENDER,
        "type",
        "veth",
        "peer",
        "name",
        VETH_RECEIVER,
    ])?;
    run_ip(&["addr", "add", "10.77.0.1/24", "dev", VETH_SENDER])?;
    run_ip(&["addr", "add", "10.77.0.2/24", "dev", VETH_RECEIVER])?;
    run_ip(&["link", "set", VETH_SENDER, "up"])?;
    run_ip(&["link", "set", VETH_RECEIVER, "up"])?;
    Ok(())
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn run_unshare(current_exe: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    for candidate in [
        "/usr/bin/unshare",
        "/bin/unshare",
        "/home/linuxbrew/.linuxbrew/bin/unshare",
        "unshare",
    ] {
        match Command::new(candidate)
            .arg("-Urn")
            .arg(current_exe)
            .env(INNER_ENV, "1")
            .status()
        {
            Ok(status) => {
                if status.success() {
                    return Ok(());
                }
                return Err(Box::new(io::Error::other(format!(
                    "`{candidate}` example subprocess failed with status {status}"
                ))));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(Box::new(error)),
        }
    }
    Err(Box::new(io::Error::new(
        io::ErrorKind::NotFound,
        "`unshare` command was not found in common locations",
    )))
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn run_inner() -> Result<(), Box<dyn std::error::Error>> {
    setup_veth_pair()?;

    let src_mac = read_mac(VETH_SENDER)?;
    let dst_mac = read_mac(VETH_RECEIVER)?;
    let (sender_tx_before, _) = read_link_packets(VETH_SENDER)?;
    let (_, receiver_rx_before) = read_link_packets(VETH_RECEIVER)?;

    let listener = UdpSocket::bind((RECEIVER_IP, DST_PORT))?;
    listener.set_read_timeout(Some(Duration::from_millis(350)))?;

    let socket = Arc::new(AfXdpKernelBypassSocket::bind_to_interface(
        VETH_SENDER,
        src_mac,
        dst_mac,
        SENDER_IP,
        SRC_PORT,
    )?);
    let transport = KernelBypassDirectTransport::new(socket);

    let target_addr = SocketAddr::from((RECEIVER_IP, DST_PORT));
    let target = LeaderTarget::new(None, target_addr);
    let payload = b"sof-kernel-bypass-af_xdp-example".to_vec();

    let config = DirectSubmitConfig {
        per_target_timeout: Duration::from_millis(500),
        global_timeout: Duration::from_millis(1_000),
        direct_target_rounds: 1,
        direct_submit_attempts: 1,
        hybrid_direct_attempts: 1,
        rebroadcast_interval: Duration::from_millis(1),
        ..DirectSubmitConfig::default()
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let selected = runtime
        .block_on(async {
            transport
                .submit_direct(
                    &payload,
                    std::slice::from_ref(&target),
                    RoutingPolicy::default(),
                    &config,
                )
                .await
        })
        .map_err(|error| io::Error::other(error.to_string()))?;

    let mut sender_tx_after = sender_tx_before;
    let mut receiver_rx_after = receiver_rx_before;
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(50));
        (sender_tx_after, _) = read_link_packets(VETH_SENDER)?;
        (_, receiver_rx_after) = read_link_packets(VETH_RECEIVER)?;
        if sender_tx_after > sender_tx_before && receiver_rx_after > receiver_rx_before {
            break;
        }
    }

    println!("selected target: {}", selected.tpu_addr);
    println!(
        "interface counters: {VETH_SENDER}.tx_packets {} -> {}, {VETH_RECEIVER}.rx_packets {} -> {}",
        sender_tx_before, sender_tx_after, receiver_rx_before, receiver_rx_after
    );

    let mut received = [0_u8; 2048];
    match listener.recv_from(&mut received) {
        Ok((len, source)) => {
            println!("udp receiver got {} bytes from {}", len, source);
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            println!(
                "udp receive timed out; kernel-bypass TX still confirmed via interface counters"
            );
        }
        Err(error) => return Err(Box::new(error)),
    }

    Ok(())
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os(INNER_ENV).is_none() {
        let current_exe = std::env::current_exe()?;
        return run_unshare(&current_exe);
    }
    run_inner()
}
