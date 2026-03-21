//! SOF kernel-bypass ingress example with runtime telemetry and plugin-side metrics.
#![cfg_attr(
    not(any(feature = "kernel-bypass", feature = "gossip-bootstrap")),
    allow(unused)
)]

#[cfg(feature = "kernel-bypass")]
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
#[cfg(feature = "gossip-bootstrap")]
use sof::runtime::RuntimeSetup;
use sof::{
    event::TxKind,
    framework::{
        ObserverPlugin, PluginConfig, PluginDispatchMode, PluginHost, PluginShutdownContext,
        PluginStartupContext, PluginStartupError, RawPacketEvent, ShredEvent,
    },
};
#[cfg(feature = "kernel-bypass")]
use sof::{
    ingest::{RawPacket, RawPacketIngress},
    protocol::shred_wire::{SIZE_OF_DATA_SHRED_PAYLOAD, VARIANT_MERKLE_DATA},
    runtime::{KernelBypassIngressSender, create_kernel_bypass_ingress_queue},
    shred::wire::SIZE_OF_DATA_SHRED_HEADERS,
};
/// Example configuration constant.
pub(crate) const DEFAULT_DURATION_SECS: u64 = 180;
/// Example configuration constant.
pub(crate) const DEFAULT_BATCH_SIZE: usize = 8;
/// Example configuration constant.
pub(crate) const DEFAULT_BATCH_INTERVAL_MS: u64 = 20;
#[cfg(feature = "kernel-bypass")]
/// Example configuration constant.
pub(crate) const SHRED_PAYLOAD_BYTES: usize = 128;
/// Example configuration constant.
pub(crate) const DEFAULT_RUNTIME_SHUTDOWN_TIMEOUT_SECS: u64 = 120;
/// Example configuration constant.
pub(crate) const DEFAULT_PLUGIN_DRAIN_TIMEOUT_SECS: u64 = 10;
#[cfg(feature = "kernel-bypass")]
/// Example configuration constant.
pub(crate) const SHRED_VERSION: u16 = 1;
/// Example configuration constant.
pub(crate) const SOURCE_ENV: &str = "SOF_KERNEL_BYPASS_EXAMPLE_SOURCE";

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
/// Example-local state or configuration type.
pub(crate) struct RawIngressSnapshot {
    /// Example-local field.
    pub(crate) packets: u64,
    /// Example-local field.
    pub(crate) bytes: u64,
    /// Example-local field.
    pub(crate) shreds: u64,
    /// Example-local field.
    pub(crate) data_shreds: u64,
    /// Example-local field.
    pub(crate) code_shreds: u64,
    /// Example-local field.
    pub(crate) tx_total: u64,
    /// Example-local field.
    pub(crate) tx_vote_only: u64,
    /// Example-local field.
    pub(crate) tx_mixed: u64,
    /// Example-local field.
    pub(crate) tx_non_vote: u64,
    /// Example-local field.
    pub(crate) source_8899_packets: u64,
    /// Example-local field.
    pub(crate) source_8900_packets: u64,
    /// Example-local field.
    pub(crate) source_other_packets: u64,
}

#[derive(Default)]
/// Example-local state or configuration type.
pub(crate) struct RawIngressMetricsPlugin {
    /// Example-local field.
    pub(crate) packets: AtomicU64,
    /// Example-local field.
    pub(crate) bytes: AtomicU64,
    /// Example-local field.
    pub(crate) shreds: AtomicU64,
    /// Example-local field.
    pub(crate) data_shreds: AtomicU64,
    /// Example-local field.
    pub(crate) code_shreds: AtomicU64,
    /// Example-local field.
    pub(crate) tx_total: AtomicU64,
    /// Example-local field.
    pub(crate) tx_vote_only: AtomicU64,
    /// Example-local field.
    pub(crate) tx_mixed: AtomicU64,
    /// Example-local field.
    pub(crate) tx_non_vote: AtomicU64,
    /// Example-local field.
    pub(crate) source_8899_packets: AtomicU64,
    /// Example-local field.
    pub(crate) source_8900_packets: AtomicU64,
    /// Example-local field.
    pub(crate) source_other_packets: AtomicU64,
}

impl RawIngressMetricsPlugin {
    /// Example helper used by this binary.
    pub(crate) fn snapshot(&self) -> RawIngressSnapshot {
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

#[async_trait]
impl ObserverPlugin for RawIngressMetricsPlugin {
    fn name(&self) -> &'static str {
        "kernel-bypass-ingress-metrics-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new()
            .with_raw_packet()
            .with_shred()
            .with_transaction()
    }

    async fn on_startup(&self, ctx: PluginStartupContext) -> Result<(), PluginStartupError> {
        tracing::info!(plugin = ctx.plugin_name, "plugin startup completed");
        Ok(())
    }

    async fn on_shutdown(&self, ctx: PluginShutdownContext) {
        tracing::info!(plugin = ctx.plugin_name, "plugin shutdown completed");
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

    async fn on_transaction(&self, event: &sof::framework::TransactionEvent) {
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

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
/// Example-local state or configuration type.
pub(crate) struct ProducerStats {
    /// Example-local field.
    pub(crate) packets: u64,
    /// Example-local field.
    pub(crate) batches: u64,
    /// Example-local field.
    pub(crate) bytes: u64,
    /// Example-local field.
    pub(crate) elapsed_ms: u64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
/// Example-local enum used by this example binary.
pub(crate) enum IngressSource {
    #[cfg(feature = "kernel-bypass")]
    /// Example-local variant.
    SyntheticKernelBypass,
    /// Example-local variant.
    LiveGossip,
}

#[cfg(feature = "kernel-bypass")]
/// Example helper used by this binary.
pub(crate) fn build_raw_packet(sequence: u64, source_port: u16) -> RawPacket {
    let slot = (sequence / 128).saturating_add(1);
    let index = u32::try_from(sequence % 128).unwrap_or(0);
    let fec_set_index = index;
    let declared_size =
        u16::try_from(SIZE_OF_DATA_SHRED_HEADERS.saturating_add(SHRED_PAYLOAD_BYTES))
            .unwrap_or(u16::MAX);
    let mut bytes = vec![0_u8; SIZE_OF_DATA_SHRED_PAYLOAD];

    // Distinct signature prefix to avoid dedupe collisions in synthetic traffic.
    write_bytes(&mut bytes, 0, &slot.to_le_bytes());
    write_bytes(&mut bytes, 8, &index.to_le_bytes());
    write_bytes(&mut bytes, 12, &fec_set_index.to_le_bytes());

    write_byte(&mut bytes, 64, VARIANT_MERKLE_DATA);
    write_bytes(&mut bytes, 65, &slot.to_le_bytes());
    write_bytes(&mut bytes, 73, &index.to_le_bytes());
    write_bytes(&mut bytes, 77, &SHRED_VERSION.to_le_bytes());
    write_bytes(&mut bytes, 79, &fec_set_index.to_le_bytes());
    write_bytes(&mut bytes, 83, &0_u16.to_le_bytes());
    write_byte(&mut bytes, 85, 0); // no DATA_COMPLETE/LAST_IN_SLOT: keep test traffic lightweight.
    write_bytes(&mut bytes, 86, &declared_size.to_le_bytes());
    let payload_end = SIZE_OF_DATA_SHRED_HEADERS.saturating_add(SHRED_PAYLOAD_BYTES);
    if let Some(payload) = bytes.get_mut(88..payload_end) {
        payload.fill(0xAB);
    }

    RawPacket {
        source: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), source_port),
        ingress: RawPacketIngress::Udp,
        bytes: bytes.into(),
    }
}

#[cfg(feature = "kernel-bypass")]
/// Example helper used by this binary.
pub(crate) fn produce_kernel_bypass_ingress(
    tx: &KernelBypassIngressSender,
    run_for: Duration,
    batch_size: usize,
    batch_interval: Duration,
) -> ProducerStats {
    let started_at = Instant::now();
    let mut sequence = 0_u64;
    let mut stats = ProducerStats::default();
    while started_at.elapsed() < run_for {
        let mut batch = Vec::with_capacity(batch_size);
        for _ in 0..batch_size {
            let source_port = match sequence % 3 {
                0 => 8_899,
                1 => 8_900,
                _ => 9_001,
            };
            let packet = build_raw_packet(sequence, source_port);
            stats.packets = stats.packets.saturating_add(1);
            stats.bytes = stats
                .bytes
                .saturating_add(u64::try_from(packet.bytes.len()).unwrap_or(u64::MAX));
            batch.push(packet);
            sequence = sequence.saturating_add(1);
        }
        if !tx.send_batch(batch, false) {
            break;
        }
        stats.batches = stats.batches.saturating_add(1);
        std::thread::sleep(batch_interval);
    }
    stats.elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    stats
}

#[cfg(feature = "kernel-bypass")]
/// Example helper used by this binary.
pub(crate) fn write_bytes(buf: &mut [u8], offset: usize, src: &[u8]) {
    let end = offset.saturating_add(src.len());
    if let Some(dst) = buf.get_mut(offset..end) {
        dst.copy_from_slice(src);
    }
}

#[cfg(feature = "kernel-bypass")]
/// Example helper used by this binary.
pub(crate) fn write_byte(buf: &mut [u8], offset: usize, value: u8) {
    if let Some(slot) = buf.get_mut(offset) {
        *slot = value;
    }
}

/// Example helper used by this binary.
pub(crate) async fn wait_for_plugin_packets(
    plugin: &RawIngressMetricsPlugin,
    expected_packets: u64,
    timeout: Duration,
) -> RawIngressSnapshot {
    let started_at = Instant::now();
    loop {
        let snapshot = plugin.snapshot();
        if snapshot.packets >= expected_packets {
            return snapshot;
        }
        if started_at.elapsed() >= timeout {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Example helper used by this binary.
pub(crate) fn read_env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

/// Example helper used by this binary.
pub(crate) fn read_env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

/// Example helper used by this binary.
pub(crate) fn read_ingress_source() -> Result<IngressSource, Box<dyn std::error::Error>> {
    let default_source = if cfg!(feature = "kernel-bypass") {
        "synthetic"
    } else {
        "gossip"
    };
    let value = std::env::var(SOURCE_ENV).unwrap_or_else(|_| default_source.to_owned());
    match value.trim().to_ascii_lowercase().as_str() {
        "synthetic" | "kernel-bypass" | "kernel_bypass" => {
            #[cfg(feature = "kernel-bypass")]
            {
                Ok(IngressSource::SyntheticKernelBypass)
            }
            #[cfg(not(feature = "kernel-bypass"))]
            {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "invalid {SOURCE_ENV} value `synthetic`; this build requires `--features kernel-bypass` for synthetic mode"
                    ),
                )
                .into())
            }
        }
        "gossip" | "live" | "mainnet" => Ok(IngressSource::LiveGossip),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid {SOURCE_ENV} value `{other}`; expected `synthetic` or `gossip`"),
        )
        .into()),
    }
}

/// Example helper used by this binary.
pub(crate) fn print_summary(
    source: IngressSource,
    producer_stats: ProducerStats,
    plugin_snapshot: RawIngressSnapshot,
    dropped_events: u64,
) {
    let plugin_mib_per_sec = format_mib_per_sec(plugin_snapshot.bytes, producer_stats.elapsed_ms);
    println!("ingress metrics summary");
    println!(
        "kernel_bypass_feature_enabled={}",
        cfg!(feature = "kernel-bypass")
    );
    println!("source={source:?}");
    println!(
        "producer: packets={} batches={} bytes={} elapsed_ms={}",
        producer_stats.packets,
        producer_stats.batches,
        producer_stats.bytes,
        producer_stats.elapsed_ms
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

/// Example helper used by this binary.
pub(crate) fn format_mib_per_sec(bytes: u64, elapsed_ms: u64) -> String {
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

#[cfg(feature = "kernel-bypass")]
/// Example helper used by this binary.
pub(crate) async fn run_synthetic_mode(
    plugin: Arc<RawIngressMetricsPlugin>,
    plugin_host: PluginHost,
    run_for: Duration,
    batch_size: usize,
    batch_interval: Duration,
    runtime_shutdown_timeout_secs: u64,
    plugin_drain_timeout_secs: u64,
) -> Result<(ProducerStats, RawIngressSnapshot, u64), Box<dyn std::error::Error>> {
    let plugin_host_metrics = plugin_host.clone();
    let (tx, rx) = create_kernel_bypass_ingress_queue();
    let runtime_task = tokio::spawn(async move {
        sof::runtime::run_async_with_plugin_host_and_kernel_bypass_ingress(plugin_host, rx).await
    });

    let producer_task = tokio::task::spawn_blocking(move || {
        produce_kernel_bypass_ingress(&tx, run_for, batch_size, batch_interval)
    });
    let producer_stats = producer_task
        .await
        .map_err(|error| io::Error::other(format!("producer task join failed: {error}")))?;

    tokio::time::timeout(
        Duration::from_secs(runtime_shutdown_timeout_secs),
        runtime_task,
    )
    .await
    .map_err(|timeout_error| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("runtime task timed out: {timeout_error}"),
        )
    })?
    .map_err(|error| io::Error::other(format!("runtime task join failed: {error}")))?
    .map_err(|error| io::Error::other(format!("runtime returned error: {error}")))?;

    let plugin_snapshot = wait_for_plugin_packets(
        plugin.as_ref(),
        producer_stats.packets,
        Duration::from_secs(plugin_drain_timeout_secs),
    )
    .await;
    let dropped_events = plugin_host_metrics.dropped_event_count();
    Ok((producer_stats, plugin_snapshot, dropped_events))
}

#[cfg(feature = "gossip-bootstrap")]
/// Example helper used by this binary.
pub(crate) async fn run_gossip_mode(
    plugin: Arc<RawIngressMetricsPlugin>,
    plugin_host: PluginHost,
    run_for: Duration,
    plugin_drain_timeout_secs: u64,
) -> Result<(ProducerStats, RawIngressSnapshot, u64), Box<dyn std::error::Error>> {
    let plugin_host_metrics = plugin_host.clone();
    let setup = RuntimeSetup::new()
        .with_startup_step_logs(true)
        .with_env("SOF_PORT_RANGE", "12000-12100")
        .with_env("SOF_GOSSIP_PORT", "8001")
        .with_env("SOF_TVU_SOCKETS", "16")
        .with_env("SOF_UDP_RCVBUF", "134217728")
        .with_env("SOF_INGEST_QUEUE_MODE", "lockfree")
        .with_env("SOF_INGEST_QUEUE_CAPACITY", "262144")
        .with_env("SOF_UDP_DROP_ON_CHANNEL_FULL", "false")
        .with_env("SOF_UDP_TRACK_RXQ_OVFL", "true")
        .with_env("SOF_UDP_RELAY_ENABLED", "false")
        .with_env("SOF_REPAIR_ENABLED", "false");
    let runtime_task = tokio::spawn(async move {
        sof::runtime::run_async_with_plugin_host_and_setup(plugin_host, &setup).await
    });

    let started_at = Instant::now();
    tokio::time::sleep(run_for).await;
    runtime_task.abort();
    match runtime_task.await {
        Err(error) if error.is_cancelled() => {}
        Err(error) => {
            return Err(io::Error::other(format!("gossip runtime join failed: {error}")).into());
        }
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return Err(io::Error::other(format!("gossip runtime returned error: {error}")).into());
        }
    }

    let plugin_snapshot = wait_for_plugin_packets(
        plugin.as_ref(),
        plugin.snapshot().packets,
        Duration::from_secs(plugin_drain_timeout_secs),
    )
    .await;
    let dropped_events = plugin_host_metrics.dropped_event_count();
    let producer_stats = ProducerStats {
        packets: 0,
        batches: 0,
        bytes: 0,
        elapsed_ms: u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    Ok((producer_stats, plugin_snapshot, dropped_events))
}

#[cfg(not(any(feature = "kernel-bypass", feature = "gossip-bootstrap")))]
fn main() {
    eprintln!(
        "This example requires `--features kernel-bypass` for synthetic ingress or `--features gossip-bootstrap` for live gossip"
    );
}

#[cfg(any(feature = "kernel-bypass", feature = "gossip-bootstrap"))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let duration_secs = read_env_u64(
        "SOF_KERNEL_BYPASS_EXAMPLE_DURATION_SECS",
        DEFAULT_DURATION_SECS,
    );
    let batch_size = read_env_usize("SOF_KERNEL_BYPASS_EXAMPLE_BATCH_SIZE", DEFAULT_BATCH_SIZE);
    let batch_interval_ms = read_env_u64(
        "SOF_KERNEL_BYPASS_EXAMPLE_BATCH_INTERVAL_MS",
        DEFAULT_BATCH_INTERVAL_MS,
    );
    let runtime_shutdown_timeout_secs = read_env_u64(
        "SOF_KERNEL_BYPASS_EXAMPLE_RUNTIME_SHUTDOWN_TIMEOUT_SECS",
        DEFAULT_RUNTIME_SHUTDOWN_TIMEOUT_SECS,
    );
    let plugin_drain_timeout_secs = read_env_u64(
        "SOF_KERNEL_BYPASS_EXAMPLE_PLUGIN_DRAIN_TIMEOUT_SECS",
        DEFAULT_PLUGIN_DRAIN_TIMEOUT_SECS,
    );
    let source = read_ingress_source()?;
    let run_for = Duration::from_secs(duration_secs);
    #[cfg(feature = "kernel-bypass")]
    let batch_interval = Duration::from_millis(batch_interval_ms);

    tracing::info!(
        duration_secs,
        batch_size,
        batch_interval_ms,
        runtime_shutdown_timeout_secs,
        plugin_drain_timeout_secs,
        source = ?source,
        "starting kernel-bypass ingress metrics example"
    );

    let plugin = Arc::new(RawIngressMetricsPlugin::default());
    let plugin_host = PluginHost::builder()
        .with_event_queue_capacity(262_144)
        .with_dispatch_mode(PluginDispatchMode::BoundedConcurrent(16))
        .add_shared_plugin(plugin.clone())
        .build();
    let (producer_stats, plugin_snapshot, dropped_events) = match source {
        #[cfg(feature = "kernel-bypass")]
        IngressSource::SyntheticKernelBypass => {
            run_synthetic_mode(
                plugin.clone(),
                plugin_host,
                run_for,
                batch_size,
                batch_interval,
                runtime_shutdown_timeout_secs,
                plugin_drain_timeout_secs,
            )
            .await?
        }
        IngressSource::LiveGossip => {
            #[cfg(feature = "gossip-bootstrap")]
            {
                run_gossip_mode(
                    plugin.clone(),
                    plugin_host,
                    run_for,
                    plugin_drain_timeout_secs,
                )
                .await?
            }
            #[cfg(not(feature = "gossip-bootstrap"))]
            {
                return Err(io::Error::other(
                    "gossip source requires compiling with `--features gossip-bootstrap`",
                )
                .into());
            }
        }
    };

    print_summary(source, producer_stats, plugin_snapshot, dropped_events);

    // Gossip mode currently relies on long-lived receiver threads that outlive
    // the aborted runtime task; force process exit after printing summary.
    if matches!(source, IngressSource::LiveGossip) {
        let exit_code = if plugin_snapshot.packets == 0 { 1 } else { 0 };
        std::process::exit(exit_code);
    }

    if plugin_snapshot.packets == 0 {
        return Err(
            io::Error::other("kernel-bypass ingress produced no observable packets").into(),
        );
    }

    Ok(())
}
