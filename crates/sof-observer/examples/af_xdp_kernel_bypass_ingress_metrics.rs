#![cfg_attr(
    not(all(target_os = "linux", feature = "kernel-bypass")),
    allow(unused)
)]
#![allow(clippy::missing_docs_in_private_items)]
//! AF_XDP external-ingress metrics example for SOF kernel-bypass runtime mode.

#[cfg(not(all(target_os = "linux", feature = "kernel-bypass")))]
fn main() {
    eprintln!("This example requires Linux and --features kernel-bypass");
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
use std::{
    ffi::CString,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
use async_trait::async_trait;
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
use sof::{
    event::TxKind,
    framework::{ObserverPlugin, PluginDispatchMode, PluginHost, RawPacketEvent, ShredEvent},
    ingest::{RawPacket, RawPacketIngress},
    runtime::KernelBypassIngressSender,
};
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
use xdp::{
    RingConfigBuilder, Umem,
    slab::{HeapSlab, Slab},
    socket::{PollTimeout, XdpSocket, XdpSocketBuilder},
    umem::{FrameSize, UmemCfgBuilder},
};

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
type ExampleError = Box<dyn std::error::Error + Send + Sync>;

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
const DEFAULT_DURATION_SECS: u64 = 180;
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
const DEFAULT_INTERFACE: &str = "enp17s0";
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
const DEFAULT_QUEUE_ID: u32 = 0;
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
const DEFAULT_BATCH_SIZE: usize = 64;
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
const DEFAULT_UMEM_FRAME_COUNT: u32 = 4_096;
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
const DEFAULT_RING_DEPTH: u32 = 2_048;
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
const DEFAULT_POLL_TIMEOUT_MS: u64 = 100;

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
struct RawIngressSnapshot {
    packets: u64,
    bytes: u64,
    shreds: u64,
    data_shreds: u64,
    code_shreds: u64,
    tx_total: u64,
    tx_vote_only: u64,
    tx_mixed: u64,
    tx_non_vote: u64,
    source_8899_packets: u64,
    source_8900_packets: u64,
    source_other_packets: u64,
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
#[derive(Default)]
struct RawIngressMetricsPlugin {
    packets: AtomicU64,
    bytes: AtomicU64,
    shreds: AtomicU64,
    data_shreds: AtomicU64,
    code_shreds: AtomicU64,
    tx_total: AtomicU64,
    tx_vote_only: AtomicU64,
    tx_mixed: AtomicU64,
    tx_non_vote: AtomicU64,
    source_8899_packets: AtomicU64,
    source_8900_packets: AtomicU64,
    source_other_packets: AtomicU64,
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
impl RawIngressMetricsPlugin {
    fn snapshot(&self) -> RawIngressSnapshot {
        RawIngressSnapshot {
            packets: self.packets.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            shreds: self.shreds.load(Ordering::Relaxed),
            data_shreds: self.data_shreds.load(Ordering::Relaxed),
            code_shreds: self.code_shreds.load(Ordering::Relaxed),
            tx_total: self.tx_total.load(Ordering::Relaxed),
            tx_vote_only: self.tx_vote_only.load(Ordering::Relaxed),
            tx_mixed: self.tx_mixed.load(Ordering::Relaxed),
            tx_non_vote: self.tx_non_vote.load(Ordering::Relaxed),
            source_8899_packets: self.source_8899_packets.load(Ordering::Relaxed),
            source_8900_packets: self.source_8900_packets.load(Ordering::Relaxed),
            source_other_packets: self.source_other_packets.load(Ordering::Relaxed),
        }
    }
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
#[async_trait]
impl ObserverPlugin for RawIngressMetricsPlugin {
    fn name(&self) -> &'static str {
        "af-xdp-kernel-bypass-ingress-metrics-plugin"
    }

    fn wants_raw_packet(&self) -> bool {
        true
    }

    fn wants_shred(&self) -> bool {
        true
    }

    fn wants_transaction(&self) -> bool {
        true
    }

    async fn on_raw_packet(&self, event: RawPacketEvent) {
        self.packets.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(
            u64::try_from(event.bytes.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        match event.source.port() {
            8_899 => {
                self.source_8899_packets.fetch_add(1, Ordering::Relaxed);
            }
            8_900 => {
                self.source_8900_packets.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                self.source_other_packets.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    async fn on_shred(&self, event: ShredEvent) {
        self.shreds.fetch_add(1, Ordering::Relaxed);
        match event.parsed.as_ref() {
            sof::shred::wire::ParsedShredHeader::Data(_) => {
                self.data_shreds.fetch_add(1, Ordering::Relaxed);
            }
            sof::shred::wire::ParsedShredHeader::Code(_) => {
                self.code_shreds.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    async fn on_transaction(&self, event: sof::framework::TransactionEvent) {
        self.tx_total.fetch_add(1, Ordering::Relaxed);
        match event.kind {
            TxKind::VoteOnly => {
                self.tx_vote_only.fetch_add(1, Ordering::Relaxed);
            }
            TxKind::Mixed => {
                self.tx_mixed.fetch_add(1, Ordering::Relaxed);
            }
            TxKind::NonVote => {
                self.tx_non_vote.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
struct AfXdpProducerStats {
    frames_seen: u64,
    udp_frames_forwarded: u64,
    filtered_frames: u64,
    parse_error_frames: u64,
    batches_sent: u64,
    bytes_forwarded: u64,
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct PortFilter {
    range_start: u16,
    range_end: u16,
    extra_port: u16,
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
impl PortFilter {
    const fn default_sol() -> Self {
        Self {
            range_start: 12_000,
            range_end: 12_100,
            extra_port: 8_001,
        }
    }

    const fn allows(self, port: u16) -> bool {
        (port >= self.range_start && port <= self.range_end) || port == self.extra_port
    }
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum FrameParseOutcome {
    ParseError,
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
#[derive(Debug, Clone, Eq, PartialEq)]
struct AfXdpConfig {
    interface: String,
    queue_id: u32,
    batch_size: usize,
    umem_frame_count: u32,
    ring_depth: u32,
    poll_timeout: Duration,
    filter: PortFilter,
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
struct AfXdpSocketState {
    socket: XdpSocket,
    rings: xdp::WakableRings,
    umem: Umem,
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn read_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn read_env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn read_env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn read_af_xdp_config() -> AfXdpConfig {
    AfXdpConfig {
        interface: std::env::var("SOF_AF_XDP_IFACE")
            .unwrap_or_else(|_| DEFAULT_INTERFACE.to_owned()),
        queue_id: read_env_u32("SOF_AF_XDP_QUEUE_ID", DEFAULT_QUEUE_ID),
        batch_size: read_env_usize("SOF_AF_XDP_BATCH_SIZE", DEFAULT_BATCH_SIZE),
        umem_frame_count: read_env_u32("SOF_AF_XDP_UMEM_FRAMES", DEFAULT_UMEM_FRAME_COUNT),
        ring_depth: read_env_u32("SOF_AF_XDP_RING_DEPTH", DEFAULT_RING_DEPTH),
        poll_timeout: Duration::from_millis(read_env_u64(
            "SOF_AF_XDP_POLL_TIMEOUT_MS",
            DEFAULT_POLL_TIMEOUT_MS,
        )),
        filter: PortFilter::default_sol(),
    }
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn build_af_xdp_socket(config: &AfXdpConfig) -> Result<AfXdpSocketState, ExampleError> {
    let ifname = CString::new(config.interface.clone())?;
    let nic = xdp::nic::NicIndex::lookup_by_name(&ifname)?.ok_or_else(|| {
        io::Error::other(format!(
            "network interface `{}` not found",
            config.interface
        ))
    })?;

    let mut builder = XdpSocketBuilder::new()?;
    let umem_cfg = UmemCfgBuilder {
        frame_size: FrameSize::TwoK,
        frame_count: config.umem_frame_count,
        ..Default::default()
    }
    .build()?;
    let mut umem = Umem::map(umem_cfg)?;
    let ring_cfg = RingConfigBuilder {
        rx_count: config.ring_depth,
        tx_count: 0,
        fill_count: config.ring_depth,
        completion_count: config.ring_depth,
    }
    .build()?;
    let (mut rings, bind_flags) = builder.build_wakable_rings(&umem, ring_cfg)?;
    let socket = builder.bind(nic, config.queue_id, bind_flags)?;

    // SAFETY: `umem` and `fill_ring` were created together by `build_wakable_rings`,
    // and the requested descriptor count is bounded by the configured ring depth.
    let fill_queued = unsafe {
        rings.fill_ring.enqueue(
            &mut umem,
            usize::try_from(config.ring_depth).unwrap_or(0),
            true,
        )?
    };
    tracing::info!(
        interface = %config.interface,
        queue_id = config.queue_id,
        ring_depth = config.ring_depth,
        fill_queued,
        "AF_XDP socket initialized"
    );

    Ok(AfXdpSocketState {
        socket,
        rings,
        umem,
    })
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn parse_udp_payload_to_raw_packet(
    frame: &[u8],
    filter: PortFilter,
) -> Result<Option<RawPacket>, FrameParseOutcome> {
    const ETH_LEN: usize = 14;
    const ETH_TYPE_OFFSET: usize = 12;
    const ETH_P_IPV4: u16 = 0x0800;
    const IP_PROTO_UDP: u8 = 17;
    if frame.len() < ETH_LEN + 20 {
        return Err(FrameParseOutcome::ParseError);
    }

    if read_u16_be(frame, ETH_TYPE_OFFSET)? != ETH_P_IPV4 {
        return Ok(None);
    }

    let ip_offset = ETH_LEN;
    let version_ihl = read_u8(frame, ip_offset)?;
    if version_ihl >> 4 != 4 {
        return Ok(None);
    }
    let ihl = usize::from(version_ihl & 0x0F).checked_mul(4).unwrap_or(0);
    let udp_header_end = checked_add(checked_add(ip_offset, ihl)?, 8)?;
    if ihl < 20 || frame.len() < udp_header_end {
        return Err(FrameParseOutcome::ParseError);
    }
    if read_u8(frame, checked_add(ip_offset, 9)?)? != IP_PROTO_UDP {
        return Ok(None);
    }
    let frag_field = read_u16_be(frame, checked_add(ip_offset, 6)?)?;
    if (frag_field & 0x1FFF) != 0 {
        return Ok(None);
    }

    let udp_offset = checked_add(ip_offset, ihl)?;
    let src_port = read_u16_be(frame, udp_offset)?;
    let dst_port = read_u16_be(frame, checked_add(udp_offset, 2)?)?;
    if !filter.allows(dst_port) {
        return Ok(None);
    }
    let udp_len = usize::from(read_u16_be(frame, checked_add(udp_offset, 4)?)?);
    if udp_len < 8 {
        return Err(FrameParseOutcome::ParseError);
    }
    let payload_start = checked_add(udp_offset, 8)?;
    let payload_end = checked_add(payload_start, udp_len.saturating_sub(8))?;
    if payload_end > frame.len() {
        return Err(FrameParseOutcome::ParseError);
    }

    let source = SocketAddr::new(
        IpAddr::V4(read_ipv4_addr(frame, checked_add(ip_offset, 12)?)?),
        src_port,
    );
    Ok(Some(RawPacket {
        source,
        ingress: RawPacketIngress::Udp,
        bytes: frame
            .get(payload_start..payload_end)
            .ok_or(FrameParseOutcome::ParseError)?
            .to_vec(),
    }))
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn checked_add(lhs: usize, rhs: usize) -> Result<usize, FrameParseOutcome> {
    lhs.checked_add(rhs).ok_or(FrameParseOutcome::ParseError)
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn read_u8(frame: &[u8], offset: usize) -> Result<u8, FrameParseOutcome> {
    frame
        .get(offset)
        .copied()
        .ok_or(FrameParseOutcome::ParseError)
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn read_u16_be(frame: &[u8], offset: usize) -> Result<u16, FrameParseOutcome> {
    let end = checked_add(offset, 2)?;
    let bytes: [u8; 2] = frame
        .get(offset..end)
        .ok_or(FrameParseOutcome::ParseError)?
        .try_into()
        .map_err(|_slice_error| FrameParseOutcome::ParseError)?;
    Ok(u16::from_be_bytes(bytes))
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn read_ipv4_addr(frame: &[u8], offset: usize) -> Result<Ipv4Addr, FrameParseOutcome> {
    let end = checked_add(offset, 4)?;
    let octets: [u8; 4] = frame
        .get(offset..end)
        .ok_or(FrameParseOutcome::ParseError)?
        .try_into()
        .map_err(|_slice_error| FrameParseOutcome::ParseError)?;
    Ok(Ipv4Addr::from(octets))
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn run_af_xdp_producer(
    tx: &KernelBypassIngressSender,
    config: &AfXdpConfig,
    run_for: Duration,
) -> Result<AfXdpProducerStats, ExampleError> {
    let mut state = build_af_xdp_socket(config)?;
    let mut slab = HeapSlab::with_capacity(usize::try_from(config.ring_depth).unwrap_or(0));
    let mut batch = Vec::with_capacity(config.batch_size);
    let started_at = Instant::now();
    let mut stats = AfXdpProducerStats::default();

    while started_at.elapsed() < run_for {
        drop(
            state
                .socket
                .poll_read(PollTimeout::new(Some(config.poll_timeout))),
        );

        let Some(rx_ring) = state.rings.rx_ring.as_mut() else {
            return Err(io::Error::other("AF_XDP socket has no RX ring configured").into());
        };
        // SAFETY: `rx_ring` is paired with `state.umem` from the same socket setup,
        // and `slab` is the destination scratch storage expected by the xdp crate.
        let received = unsafe { rx_ring.recv(&state.umem, &mut slab) };
        if received == 0 {
            continue;
        }

        for _ in 0..received {
            let Some(frame) = slab.pop_back() else {
                break;
            };
            stats.frames_seen = stats.frames_seen.saturating_add(1);
            match parse_udp_payload_to_raw_packet(&frame, config.filter) {
                Ok(Some(raw_packet)) => {
                    stats.udp_frames_forwarded = stats.udp_frames_forwarded.saturating_add(1);
                    stats.bytes_forwarded = stats
                        .bytes_forwarded
                        .saturating_add(u64::try_from(raw_packet.bytes.len()).unwrap_or(u64::MAX));
                    batch.push(raw_packet);
                    if batch.len() >= config.batch_size {
                        if !tx.send_batch(std::mem::take(&mut batch), false) {
                            state.umem.free_packet(frame);
                            return Ok(stats);
                        }
                        stats.batches_sent = stats.batches_sent.saturating_add(1);
                    }
                }
                Ok(None) => {
                    stats.filtered_frames = stats.filtered_frames.saturating_add(1);
                }
                Err(FrameParseOutcome::ParseError) => {
                    stats.parse_error_frames = stats.parse_error_frames.saturating_add(1);
                }
            }
            state.umem.free_packet(frame);
        }

        // SAFETY: Returned RX buffers are recycled into the matching fill ring for the
        // same `umem`, bounded by the number of descriptors just received.
        unsafe {
            state
                .rings
                .fill_ring
                .enqueue(&mut state.umem, received, true)?
        };
    }

    if !batch.is_empty() && tx.send_batch(batch, false) {
        stats.batches_sent = stats.batches_sent.saturating_add(1);
    }

    Ok(stats)
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn print_summary(
    duration: Duration,
    config: &AfXdpConfig,
    producer_stats: AfXdpProducerStats,
    plugin_snapshot: RawIngressSnapshot,
    dropped_events: u64,
) {
    let elapsed_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    let plugin_mib_per_sec = format_mib_per_sec(plugin_snapshot.bytes, elapsed_ms);

    println!("af_xdp_kernel_bypass summary");
    println!(
        "af_xdp: interface={} queue_id={} ring_depth={} batch_size={} frames_seen={} udp_frames_forwarded={} filtered_frames={} parse_error_frames={} batches_sent={} bytes_forwarded={}",
        config.interface,
        config.queue_id,
        config.ring_depth,
        config.batch_size,
        producer_stats.frames_seen,
        producer_stats.udp_frames_forwarded,
        producer_stats.filtered_frames,
        producer_stats.parse_error_frames,
        producer_stats.batches_sent,
        producer_stats.bytes_forwarded
    );
    println!(
        "plugin: packets={} bytes={} shreds={} data_shreds={} code_shreds={} tx_total={} tx_vote_only={} tx_mixed={} tx_non_vote={} source_8899={} source_8900={} source_other={}",
        plugin_snapshot.packets,
        plugin_snapshot.bytes,
        plugin_snapshot.shreds,
        plugin_snapshot.data_shreds,
        plugin_snapshot.code_shreds,
        plugin_snapshot.tx_total,
        plugin_snapshot.tx_vote_only,
        plugin_snapshot.tx_mixed,
        plugin_snapshot.tx_non_vote,
        plugin_snapshot.source_8899_packets,
        plugin_snapshot.source_8900_packets,
        plugin_snapshot.source_other_packets
    );
    println!("plugin_throughput_mib_per_sec={plugin_mib_per_sec}");
    println!("plugin_dispatch_dropped_events={dropped_events}");
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
fn format_mib_per_sec(bytes: u64, elapsed_ms: u64) -> String {
    if elapsed_ms == 0 {
        return "0.000".to_owned();
    }

    let numerator = u128::from(bytes).saturating_mul(1_000_000);
    let denominator = u128::from(elapsed_ms).saturating_mul(1_048_576);
    let scaled = numerator.checked_div(denominator).unwrap_or(0);
    let whole = scaled / 1_000;
    let fractional = scaled % 1_000;

    format!("{whole}.{fractional:03}")
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
#[tokio::main]
async fn main() -> Result<(), ExampleError> {
    let duration = Duration::from_secs(read_env_u64(
        "SOF_AF_XDP_EXAMPLE_DURATION_SECS",
        DEFAULT_DURATION_SECS,
    ));
    let config = read_af_xdp_config();

    tracing::info!(
        duration_secs = duration.as_secs(),
        interface = %config.interface,
        queue_id = config.queue_id,
        ring_depth = config.ring_depth,
        "starting AF_XDP kernel-bypass ingress metrics example"
    );

    let plugin = Arc::new(RawIngressMetricsPlugin::default());
    let plugin_host = PluginHost::builder()
        .with_event_queue_capacity(262_144)
        .with_dispatch_mode(PluginDispatchMode::BoundedConcurrent(16))
        .add_shared_plugin(plugin.clone())
        .build();
    let plugin_host_metrics = plugin_host.clone();

    let (tx, rx) = sof::runtime::create_kernel_bypass_ingress_queue();
    let runtime_task = tokio::spawn(async move {
        sof::runtime::run_async_with_plugin_host_and_kernel_bypass_ingress(plugin_host, rx).await
    });

    let producer_config = config.clone();
    let producer_task =
        tokio::task::spawn_blocking(move || run_af_xdp_producer(&tx, &producer_config, duration));
    let producer_stats = producer_task.await.map_err(|error| {
        io::Error::other(format!("AF_XDP producer task join failed: {error}"))
    })??;

    let runtime_result = tokio::time::timeout(Duration::from_secs(30), runtime_task)
        .await
        .map_err(|timeout_error| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("runtime task timed out: {timeout_error}"),
            )
        })?;
    match runtime_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return Err(io::Error::other(format!("runtime returned error: {error}")).into());
        }
        Err(error) => {
            return Err(io::Error::other(format!("runtime task join failed: {error}")).into());
        }
    }

    let plugin_snapshot = plugin.snapshot();
    let dropped_events = plugin_host_metrics.dropped_event_count();
    print_summary(
        duration,
        &config,
        producer_stats,
        plugin_snapshot,
        dropped_events,
    );

    if plugin_snapshot.packets == 0 {
        return Err(io::Error::other(
            "AF_XDP ingress observed zero packets; ensure CAP_NET_ADMIN/CAP_BPF and an XDP redirect program are configured",
        )
        .into());
    }

    Ok(())
}
