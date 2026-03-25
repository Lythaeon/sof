use std::{future::Future, net::SocketAddr, path::PathBuf, pin::Pin};

use crate::framework::{
    DerivedStateHost, DerivedStateReplayBackend, DerivedStateReplayDurability, PluginHost,
    RuntimeExtensionHost,
};
use sof_gossip_tuning::{
    CpuCoreIndex, GossipChannelTuning, GossipTuningProfile, GossipTuningService, IngestQueueMode,
    QueueCapacity, ReceiverCoalesceWindow, RuntimeTuningPort, SofRuntimeTuning,
    TvuReceiveSocketCount,
};
use thiserror::Error;

type ShutdownSignal = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Public runtime error surface for packaged SOF entrypoints.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// Tokio runtime initialization failed before ingest started.
    #[error("failed to build tokio runtime: {0}")]
    BuildTokioRuntime(std::io::Error),
    /// Runtime runloop exited with an operational error.
    #[error("runtime runloop failed: {0}")]
    Runloop(String),
}

impl From<crate::app::runtime::RuntimeEntrypointError> for RuntimeError {
    fn from(value: crate::app::runtime::RuntimeEntrypointError) -> Self {
        match value {
            crate::app::runtime::RuntimeEntrypointError::BuildTokioRuntime { source } => {
                Self::BuildTokioRuntime(source)
            }
            crate::app::runtime::RuntimeEntrypointError::Runloop { reason } => {
                Self::Runloop(reason)
            }
        }
    }
}

/// Programmatic runtime setup that mirrors selected SOF environment variables.
///
/// This lets embedders configure startup in code while keeping SOF's env-based
/// configuration model for everything else.
#[derive(Clone, Debug, Default)]
pub struct RuntimeSetup {
    /// Env-like key/value overrides applied before runtime bootstrap.
    env_overrides: Vec<(String, String)>,
}

/// Typed runtime observability endpoint configuration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeObservabilityConfig {
    /// Bind address for the runtime-owned observability endpoint.
    pub bind_addr: Option<SocketAddr>,
}

/// Typed replay retention configuration for derived-state consumers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedStateReplayConfig {
    /// Replay backend used to retain envelopes.
    pub backend: DerivedStateReplayBackend,
    /// Filesystem directory used by the disk backend.
    pub replay_dir: PathBuf,
    /// Durability policy used by the disk backend.
    pub durability: DerivedStateReplayDurability,
    /// Maximum number of retained envelopes per session.
    pub max_envelopes: usize,
    /// Maximum number of retained sessions visible to the disk backend.
    pub max_sessions: usize,
}

impl Default for DerivedStateReplayConfig {
    fn default() -> Self {
        Self {
            backend: DerivedStateReplayBackend::Memory,
            replay_dir: PathBuf::from(".sof-derived-state-replay"),
            durability: DerivedStateReplayDurability::Flush,
            max_envelopes: 8_192,
            max_sessions: 4,
        }
    }
}

impl DerivedStateReplayConfig {
    /// Returns a checkpoint-only replay configuration with no retained runtime tail.
    ///
    /// This is the lowest-memory derived-state mode. Consumers rely on their own
    /// durable checkpoints and accept that retained envelope replay is unavailable.
    #[must_use]
    pub fn checkpoint_only() -> Self {
        Self {
            max_envelopes: 0,
            max_sessions: 0,
            ..Self::default()
        }
    }

    /// Returns whether the runtime-owned replay tail is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.max_envelopes > 0
    }
}

/// Typed derived-state runtime configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedStateRuntimeConfig {
    /// Periodic checkpoint-barrier cadence in milliseconds.
    pub checkpoint_interval_ms: u64,
    /// Periodic recovery-attempt cadence in milliseconds.
    pub recovery_interval_ms: u64,
    /// Replay retention settings.
    pub replay: DerivedStateReplayConfig,
}

impl Default for DerivedStateRuntimeConfig {
    fn default() -> Self {
        Self {
            checkpoint_interval_ms: 30_000,
            recovery_interval_ms: 5_000,
            replay: DerivedStateReplayConfig::default(),
        }
    }
}

impl DerivedStateRuntimeConfig {
    /// Returns the default runtime configuration used when no env overrides are present.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl RuntimeObservabilityConfig {
    /// Returns a disabled observability configuration.
    #[must_use]
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Returns an enabled observability configuration bound to one socket address.
    #[must_use]
    pub const fn with_bind_addr(bind_addr: SocketAddr) -> Self {
        Self {
            bind_addr: Some(bind_addr),
        }
    }

    /// Returns whether the runtime-owned observability endpoint is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.bind_addr.is_some()
    }
}

impl RuntimeSetup {
    /// Creates an empty setup that preserves standard env/default behavior.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            env_overrides: Vec::new(),
        }
    }

    /// Adds an explicit env-style override.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_overrides.push((key.into(), value.into()));
        self
    }

    /// Sets `RUST_LOG`.
    #[must_use]
    pub fn with_rust_log_filter(self, filter: impl Into<String>) -> Self {
        self.with_env("RUST_LOG", filter)
    }

    /// Sets `SOF_BIND`.
    #[must_use]
    pub fn with_bind_addr(self, bind_addr: SocketAddr) -> Self {
        self.with_env("SOF_BIND", bind_addr.to_string())
    }

    /// Sets `SOF_OBSERVABILITY_BIND`.
    #[must_use]
    pub fn with_observability_bind_addr(self, bind_addr: SocketAddr) -> Self {
        self.with_env("SOF_OBSERVABILITY_BIND", bind_addr.to_string())
    }

    /// Applies one typed runtime observability configuration bundle.
    #[must_use]
    pub fn with_observability_config(self, config: RuntimeObservabilityConfig) -> Self {
        match config.bind_addr {
            Some(bind_addr) => self.with_observability_bind_addr(bind_addr),
            None => self,
        }
    }

    /// Sets `SOF_GOSSIP_ENTRYPOINT` from a list of entrypoints.
    #[must_use]
    pub fn with_gossip_entrypoints<I, S>(self, gossip_entrypoints: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let serialized = gossip_entrypoints
            .into_iter()
            .map(Into::into)
            .collect::<Vec<_>>()
            .join(",");
        self.with_env("SOF_GOSSIP_ENTRYPOINT", serialized)
    }

    /// Sets `SOF_GOSSIP_VALIDATORS` from a list of validator identity pubkeys.
    #[must_use]
    pub fn with_gossip_validators<I, S>(self, gossip_validators: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let serialized = gossip_validators
            .into_iter()
            .map(Into::into)
            .collect::<Vec<_>>()
            .join(",");
        self.with_env("SOF_GOSSIP_VALIDATORS", serialized)
    }

    /// Sets `SOF_PORT_RANGE`.
    #[must_use]
    pub fn with_gossip_port_range(self, start: u16, end: u16) -> Self {
        self.with_env("SOF_PORT_RANGE", format!("{start}-{end}"))
    }

    /// Sets `SOF_SHRED_VERSION`.
    #[must_use]
    pub fn with_shred_version(self, shred_version: u16) -> Self {
        self.with_env("SOF_SHRED_VERSION", shred_version.to_string())
    }

    /// Sets `SOF_LOG_STARTUP_STEPS`.
    #[must_use]
    pub fn with_startup_step_logs(self, enabled: bool) -> Self {
        self.with_env("SOF_LOG_STARTUP_STEPS", enabled.to_string())
    }

    /// Sets `SOF_LOG_REPAIR_PEER_TRAFFIC`.
    #[must_use]
    pub fn with_repair_peer_traffic_logs(self, enabled: bool) -> Self {
        self.with_env("SOF_LOG_REPAIR_PEER_TRAFFIC", enabled.to_string())
    }

    /// Sets `SOF_LOG_REPAIR_PEER_TRAFFIC_EVERY`.
    #[must_use]
    pub fn with_repair_peer_traffic_every(self, every_n_events: u64) -> Self {
        self.with_env(
            "SOF_LOG_REPAIR_PEER_TRAFFIC_EVERY",
            every_n_events.to_string(),
        )
    }

    /// Sets `SOF_WORKER_THREADS`.
    #[must_use]
    pub fn with_worker_threads(self, worker_threads: usize) -> Self {
        self.with_env("SOF_WORKER_THREADS", worker_threads.to_string())
    }

    /// Sets `SOF_DATASET_WORKERS`.
    #[must_use]
    pub fn with_dataset_workers(self, dataset_workers: usize) -> Self {
        self.with_env("SOF_DATASET_WORKERS", dataset_workers.to_string())
    }

    /// Sets `SOF_PACKET_WORKERS`.
    #[must_use]
    pub fn with_packet_workers(self, packet_workers: usize) -> Self {
        self.with_env("SOF_PACKET_WORKERS", packet_workers.to_string())
    }

    /// Sets `SOF_PACKET_WORKER_QUEUE_CAPACITY`.
    #[must_use]
    pub fn with_packet_worker_queue_capacity(self, queue_capacity: usize) -> Self {
        self.with_env(
            "SOF_PACKET_WORKER_QUEUE_CAPACITY",
            queue_capacity.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_RECEIVER_CHANNEL_CAPACITY`.
    #[must_use]
    pub fn with_gossip_receiver_channel_capacity(self, queue_capacity: usize) -> Self {
        self.with_env(
            "SOF_GOSSIP_RECEIVER_CHANNEL_CAPACITY",
            queue_capacity.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_SOCKET_CONSUME_CHANNEL_CAPACITY`.
    #[must_use]
    pub fn with_gossip_socket_consume_channel_capacity(self, queue_capacity: usize) -> Self {
        self.with_env(
            "SOF_GOSSIP_SOCKET_CONSUME_CHANNEL_CAPACITY",
            queue_capacity.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_RESPONSE_CHANNEL_CAPACITY`.
    #[must_use]
    pub fn with_gossip_response_channel_capacity(self, queue_capacity: usize) -> Self {
        self.with_env(
            "SOF_GOSSIP_RESPONSE_CHANNEL_CAPACITY",
            queue_capacity.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_CHANNEL_CONSUME_CAPACITY`.
    #[must_use]
    pub fn with_gossip_channel_consume_capacity(self, queue_capacity: usize) -> Self {
        self.with_env(
            "SOF_GOSSIP_CHANNEL_CONSUME_CAPACITY",
            queue_capacity.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_CONSUME_THREADS`.
    #[must_use]
    pub fn with_gossip_consume_threads(self, thread_count: usize) -> Self {
        self.with_env("SOF_GOSSIP_CONSUME_THREADS", thread_count.to_string())
    }

    /// Sets `SOF_GOSSIP_LISTEN_THREADS`.
    #[must_use]
    pub fn with_gossip_listen_threads(self, thread_count: usize) -> Self {
        self.with_env("SOF_GOSSIP_LISTEN_THREADS", thread_count.to_string())
    }

    /// Sets `SOF_GOSSIP_RUN_THREADS`.
    #[must_use]
    pub fn with_gossip_run_threads(self, thread_count: usize) -> Self {
        self.with_env("SOF_GOSSIP_RUN_THREADS", thread_count.to_string())
    }

    /// Sets `SOF_GOSSIP_SOCKET_CONSUME_PARALLEL_PACKET_THRESHOLD`.
    #[must_use]
    pub fn with_gossip_socket_consume_parallel_packet_threshold(
        self,
        packet_threshold: usize,
    ) -> Self {
        self.with_env(
            "SOF_GOSSIP_SOCKET_CONSUME_PARALLEL_PACKET_THRESHOLD",
            packet_threshold.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_STATS_INTERVAL_SECS`.
    ///
    /// Use `0` to disable the gossip metrics thread.
    #[must_use]
    pub fn with_gossip_stats_interval_secs(self, interval_secs: u64) -> Self {
        self.with_env("SOF_GOSSIP_STATS_INTERVAL_SECS", interval_secs.to_string())
    }

    /// Sets `SOF_GOSSIP_SAMPLE_LOGS_ENABLED`.
    #[must_use]
    pub fn with_gossip_sample_logs_enabled(self, enabled: bool) -> Self {
        self.with_env("SOF_GOSSIP_SAMPLE_LOGS_ENABLED", enabled.to_string())
    }

    /// Sets `SOF_DATASET_MAX_TRACKED_SLOTS`.
    #[must_use]
    pub fn with_dataset_max_tracked_slots(self, max_tracked_slots: usize) -> Self {
        self.with_env(
            "SOF_DATASET_MAX_TRACKED_SLOTS",
            max_tracked_slots.to_string(),
        )
    }

    /// Sets `SOF_DATASET_RETAINED_SLOT_LAG`.
    #[must_use]
    pub fn with_dataset_retained_slot_lag(self, slot_lag: u64) -> Self {
        self.with_env("SOF_DATASET_RETAINED_SLOT_LAG", slot_lag.to_string())
    }

    /// Sets `SOF_DATASET_QUEUE_CAPACITY`.
    #[must_use]
    pub fn with_dataset_queue_capacity(self, queue_capacity: usize) -> Self {
        self.with_env("SOF_DATASET_QUEUE_CAPACITY", queue_capacity.to_string())
    }

    /// Sets `SOF_FEC_MAX_TRACKED_SETS`.
    #[must_use]
    pub fn with_fec_max_tracked_sets(self, max_tracked_sets: usize) -> Self {
        self.with_env("SOF_FEC_MAX_TRACKED_SETS", max_tracked_sets.to_string())
    }

    /// Sets `SOF_FEC_RETAINED_SLOT_LAG`.
    #[must_use]
    pub fn with_fec_retained_slot_lag(self, slot_lag: u64) -> Self {
        self.with_env("SOF_FEC_RETAINED_SLOT_LAG", slot_lag.to_string())
    }

    /// Sets `SOF_LOG_ALL_TXS`.
    #[must_use]
    pub fn with_log_all_txs(self, enabled: bool) -> Self {
        self.with_env("SOF_LOG_ALL_TXS", enabled.to_string())
    }

    /// Sets `SOF_LOG_NON_VOTE_TXS`.
    #[must_use]
    pub fn with_log_non_vote_txs(self, enabled: bool) -> Self {
        self.with_env("SOF_LOG_NON_VOTE_TXS", enabled.to_string())
    }

    /// Sets `SOF_SKIP_VOTE_ONLY_TX_DETAIL_PATH`.
    #[must_use]
    pub fn with_skip_vote_only_tx_detail_path(self, enabled: bool) -> Self {
        self.with_env("SOF_SKIP_VOTE_ONLY_TX_DETAIL_PATH", enabled.to_string())
    }

    /// Sets `SOF_INLINE_TRANSACTION_DISPATCH`.
    ///
    /// This is an explicit runtime override. By default, SOF automatically chooses
    /// the inline transaction path when a compatible plugin opts in with
    /// [`crate::framework::PluginConfig::with_inline_transaction`].
    ///
    /// Setting this to `false` forces the standard dataset-worker path even for
    /// inline-capable plugins. Setting it to `true` keeps inline dispatch eligible,
    /// but incompatible dataset or derived-state consumers still force a fallback to
    /// the standard path.
    #[must_use]
    pub fn with_inline_transaction_dispatch(self, enabled: bool) -> Self {
        self.with_env("SOF_INLINE_TRANSACTION_DISPATCH", enabled.to_string())
    }

    /// Forces the standard dataset-worker transaction path.
    #[must_use]
    pub fn with_standard_transaction_dispatch(self) -> Self {
        self.with_inline_transaction_dispatch(false)
    }

    /// Sets `SOF_LOG_DATASET_RECONSTRUCTION`.
    #[must_use]
    pub fn with_log_dataset_reconstruction(self, enabled: bool) -> Self {
        self.with_env("SOF_LOG_DATASET_RECONSTRUCTION", enabled.to_string())
    }

    /// Sets `SOF_DERIVED_STATE_CHECKPOINT_INTERVAL_MS`.
    ///
    /// Use `0` to disable periodic checkpoint barriers.
    #[must_use]
    pub fn with_derived_state_checkpoint_interval_ms(self, interval_ms: u64) -> Self {
        self.with_env(
            "SOF_DERIVED_STATE_CHECKPOINT_INTERVAL_MS",
            interval_ms.to_string(),
        )
    }

    /// Sets `SOF_DERIVED_STATE_RECOVERY_INTERVAL_MS`.
    ///
    /// Use `0` to disable periodic replay-based recovery attempts for unhealthy consumers.
    #[must_use]
    pub fn with_derived_state_recovery_interval_ms(self, interval_ms: u64) -> Self {
        self.with_env(
            "SOF_DERIVED_STATE_RECOVERY_INTERVAL_MS",
            interval_ms.to_string(),
        )
    }

    /// Sets `SOF_DERIVED_STATE_REPLAY_MAX_ENVELOPES`.
    ///
    /// Use `0` to disable the runtime-owned retained replay tail.
    #[must_use]
    pub fn with_derived_state_replay_max_envelopes(self, max_envelopes: usize) -> Self {
        self.with_env(
            "SOF_DERIVED_STATE_REPLAY_MAX_ENVELOPES",
            max_envelopes.to_string(),
        )
    }

    /// Sets `SOF_DERIVED_STATE_REPLAY_MAX_SESSIONS`.
    ///
    /// Used by the disk backend to compact older session logs.
    #[must_use]
    pub fn with_derived_state_replay_max_sessions(self, max_sessions: usize) -> Self {
        self.with_env(
            "SOF_DERIVED_STATE_REPLAY_MAX_SESSIONS",
            max_sessions.to_string(),
        )
    }

    /// Disables the runtime-owned replay tail and keeps derived-state recovery checkpoint-only.
    #[must_use]
    pub fn with_derived_state_checkpoint_only(self) -> Self {
        self.with_derived_state_replay_max_envelopes(0)
            .with_derived_state_replay_max_sessions(0)
    }

    /// Applies one typed derived-state configuration bundle.
    #[must_use]
    pub fn with_derived_state_config(self, config: DerivedStateRuntimeConfig) -> Self {
        self.with_derived_state_checkpoint_interval_ms(config.checkpoint_interval_ms)
            .with_derived_state_recovery_interval_ms(config.recovery_interval_ms)
            .with_derived_state_replay_max_envelopes(config.replay.max_envelopes)
            .with_derived_state_replay_max_sessions(config.replay.max_sessions)
            .with_derived_state_replay_backend(config.replay.backend)
            .with_derived_state_replay_dir(config.replay.replay_dir)
            .with_derived_state_replay_durability(config.replay.durability)
    }

    /// Sets `SOF_DERIVED_STATE_REPLAY_BACKEND`.
    #[must_use]
    pub fn with_derived_state_replay_backend(self, backend: DerivedStateReplayBackend) -> Self {
        self.with_env("SOF_DERIVED_STATE_REPLAY_BACKEND", backend.as_str())
    }

    /// Sets `SOF_DERIVED_STATE_REPLAY_DIR`.
    #[must_use]
    pub fn with_derived_state_replay_dir(self, replay_dir: impl Into<PathBuf>) -> Self {
        let replay_dir = replay_dir.into();
        self.with_env(
            "SOF_DERIVED_STATE_REPLAY_DIR",
            replay_dir.to_string_lossy().into_owned(),
        )
    }

    /// Sets `SOF_DERIVED_STATE_REPLAY_DURABILITY`.
    #[must_use]
    pub fn with_derived_state_replay_durability(
        self,
        durability: DerivedStateReplayDurability,
    ) -> Self {
        self.with_env("SOF_DERIVED_STATE_REPLAY_DURABILITY", durability.as_str())
    }

    /// Sets `SOF_LIVE_SHREDS_ENABLED`.
    ///
    /// When false, runtime keeps control-plane hooks active but skips live
    /// shred data-plane processing (`on_raw_packet`, `on_shred`, datasets, txs).
    #[must_use]
    pub fn with_live_shreds_enabled(self, enabled: bool) -> Self {
        self.with_env("SOF_LIVE_SHREDS_ENABLED", enabled.to_string())
    }

    /// Sets `SOF_VERIFY_SHREDS`.
    #[must_use]
    pub fn with_verify_shreds(self, enabled: bool) -> Self {
        self.with_env("SOF_VERIFY_SHREDS", enabled.to_string())
    }

    /// Sets `SOF_VERIFY_STRICT`.
    #[must_use]
    pub fn with_verify_strict_unknown(self, enabled: bool) -> Self {
        self.with_env("SOF_VERIFY_STRICT", enabled.to_string())
    }

    /// Sets `SOF_VERIFY_RECOVERED_SHREDS`.
    #[must_use]
    pub fn with_verify_recovered_shreds(self, enabled: bool) -> Self {
        self.with_env("SOF_VERIFY_RECOVERED_SHREDS", enabled.to_string())
    }

    /// Sets `SOF_RUNTIME_DATASET_DECODE_FAILURES_UNHEALTHY_PER_TICK`.
    #[must_use]
    pub fn with_runtime_dataset_decode_failures_unhealthy_per_tick(self, failures: u64) -> Self {
        self.with_env(
            "SOF_RUNTIME_DATASET_DECODE_FAILURES_UNHEALTHY_PER_TICK",
            failures.to_string(),
        )
    }

    /// Sets `SOF_RUNTIME_DATASET_TAIL_SKIPS_UNHEALTHY_PER_TICK`.
    #[must_use]
    pub fn with_runtime_dataset_tail_skips_unhealthy_per_tick(self, tail_skips: u64) -> Self {
        self.with_env(
            "SOF_RUNTIME_DATASET_TAIL_SKIPS_UNHEALTHY_PER_TICK",
            tail_skips.to_string(),
        )
    }

    /// Sets `SOF_RUNTIME_DATASET_UNHEALTHY_SUSTAIN_TICKS`.
    #[must_use]
    pub fn with_runtime_dataset_unhealthy_sustain_ticks(self, ticks: u64) -> Self {
        self.with_env(
            "SOF_RUNTIME_DATASET_UNHEALTHY_SUSTAIN_TICKS",
            ticks.to_string(),
        )
    }

    /// Sets `SOF_VERIFY_SLOT_WINDOW`.
    #[must_use]
    pub fn with_verify_slot_window(self, slot_window: u64) -> Self {
        self.with_env("SOF_VERIFY_SLOT_WINDOW", slot_window.to_string())
    }

    /// Sets `SOF_VERIFY_SIGNATURE_CACHE`.
    #[must_use]
    pub fn with_verify_signature_cache_entries(self, entries: usize) -> Self {
        self.with_env("SOF_VERIFY_SIGNATURE_CACHE", entries.to_string())
    }

    /// Sets `SOF_SHRED_DEDUP_CAPACITY`.
    #[must_use]
    pub fn with_shred_dedupe_capacity(self, dedupe_capacity: usize) -> Self {
        self.with_env("SOF_SHRED_DEDUP_CAPACITY", dedupe_capacity.to_string())
    }

    /// Sets `SOF_SHRED_DEDUP_TTL_MS`.
    #[must_use]
    pub fn with_shred_dedupe_ttl_ms(self, dedupe_ttl_ms: u64) -> Self {
        self.with_env("SOF_SHRED_DEDUP_TTL_MS", dedupe_ttl_ms.to_string())
    }

    /// Sets `SOF_TVU_SOCKETS`.
    #[must_use]
    pub fn with_tvu_receive_sockets(self, tvu_receive_sockets: usize) -> Self {
        self.with_env("SOF_TVU_SOCKETS", tvu_receive_sockets.to_string())
    }

    /// Sets `SOF_UDP_RECEIVER_CORE`.
    #[must_use]
    pub fn with_udp_receiver_core(self, core_index: usize) -> Self {
        self.with_env("SOF_UDP_RECEIVER_CORE", core_index.to_string())
    }

    /// Sets `SOF_UDP_RCVBUF`.
    #[must_use]
    pub fn with_udp_rcvbuf_bytes(self, rcvbuf_bytes: usize) -> Self {
        self.with_env("SOF_UDP_RCVBUF", rcvbuf_bytes.to_string())
    }

    /// Sets `SOF_UDP_BATCH_SIZE`.
    #[must_use]
    pub fn with_udp_batch_size(self, batch_size: usize) -> Self {
        self.with_env("SOF_UDP_BATCH_SIZE", batch_size.to_string())
    }

    /// Sets `SOF_UDP_BATCH_MAX_WAIT_MS`.
    #[must_use]
    pub fn with_udp_batch_max_wait_ms(self, batch_max_wait_ms: u64) -> Self {
        self.with_env("SOF_UDP_BATCH_MAX_WAIT_MS", batch_max_wait_ms.to_string())
    }

    /// Sets `SOF_UDP_RECEIVER_PIN_BY_PORT`.
    #[must_use]
    pub fn with_udp_receiver_pin_by_port(self, enabled: bool) -> Self {
        self.with_env("SOF_UDP_RECEIVER_PIN_BY_PORT", enabled.to_string())
    }

    /// Sets `SOF_UDP_IDLE_WAIT_MS`.
    #[must_use]
    pub fn with_udp_idle_wait_ms(self, idle_wait_ms: u64) -> Self {
        self.with_env("SOF_UDP_IDLE_WAIT_MS", idle_wait_ms.to_string())
    }

    /// Sets `SOF_UDP_BUSY_POLL_US`.
    ///
    /// Linux only. Use `0` or leave unset to disable socket busy polling.
    #[must_use]
    pub fn with_udp_busy_poll_us(self, busy_poll_us: u32) -> Self {
        self.with_env("SOF_UDP_BUSY_POLL_US", busy_poll_us.to_string())
    }

    /// Sets `SOF_UDP_BUSY_POLL_BUDGET`.
    ///
    /// Linux only. This controls how many packets one busy-poll pass will try to drain.
    #[must_use]
    pub fn with_udp_busy_poll_budget(self, busy_poll_budget: u32) -> Self {
        self.with_env("SOF_UDP_BUSY_POLL_BUDGET", busy_poll_budget.to_string())
    }

    /// Sets `SOF_UDP_PREFER_BUSY_POLL`.
    ///
    /// Linux only. This asks the kernel to prefer busy polling for supported UDP sockets.
    #[must_use]
    pub fn with_udp_prefer_busy_poll(self, enabled: bool) -> Self {
        self.with_env("SOF_UDP_PREFER_BUSY_POLL", enabled.to_string())
    }

    /// Sets `SOF_INGEST_QUEUE_MODE`.
    ///
    /// Accepted values:
    /// - `bounded` (default): Tokio bounded channel.
    /// - `unbounded`: Tokio unbounded channel.
    /// - `lockfree`: lock-free `ArrayQueue` ring with async wakeups.
    #[must_use]
    pub fn with_ingest_queue_mode(self, mode: impl Into<String>) -> Self {
        self.with_env("SOF_INGEST_QUEUE_MODE", mode)
    }

    /// Sets `SOF_INGEST_QUEUE_CAPACITY`.
    ///
    /// Used by `bounded` and `lockfree` modes.
    #[must_use]
    pub fn with_ingest_queue_capacity(self, queue_capacity: usize) -> Self {
        self.with_env("SOF_INGEST_QUEUE_CAPACITY", queue_capacity.to_string())
    }

    /// Sets `SOF_INGEST_QUEUE_MODE` from a typed queue mode.
    #[must_use]
    pub fn with_ingest_queue_mode_typed(self, mode: IngestQueueMode) -> Self {
        self.with_ingest_queue_mode(mode.as_str())
    }

    /// Applies one typed SOF-supported gossip/runtime tuning bundle.
    #[must_use]
    pub fn with_sof_gossip_runtime_tuning(self, tuning: SofRuntimeTuning) -> Self {
        let mut adapter = RuntimeSetupTuningAdapter::new(self);
        GossipTuningService::apply_runtime_tuning(tuning, &mut adapter);
        adapter.into_setup()
    }

    /// Applies one typed gossip host profile.
    ///
    /// This applies the SOF runtime subset plus bundled gossip queue/worker tuning and
    /// semantic shred dedupe capacity.
    #[must_use]
    pub fn with_gossip_tuning_profile(self, profile: GossipTuningProfile) -> Self {
        let mut adapter = RuntimeSetupTuningAdapter::new(self);
        GossipTuningService::apply_supported_runtime_tuning(profile, &mut adapter);
        adapter.into_setup()
    }

    /// Sets `SOF_REPAIR_ENABLED`.
    #[must_use]
    pub fn with_repair_enabled(self, enabled: bool) -> Self {
        self.with_env("SOF_REPAIR_ENABLED", enabled.to_string())
    }

    /// Sets `SOF_REPAIR_TICK_MS`.
    #[must_use]
    pub fn with_repair_tick_ms(self, repair_tick_ms: u64) -> Self {
        self.with_env("SOF_REPAIR_TICK_MS", repair_tick_ms.to_string())
    }

    /// Sets `SOF_REPAIR_SLOT_WINDOW`.
    #[must_use]
    pub fn with_repair_slot_window(self, repair_slot_window: u64) -> Self {
        self.with_env("SOF_REPAIR_SLOT_WINDOW", repair_slot_window.to_string())
    }

    /// Sets `SOF_REPAIR_PEER_SAMPLE_SIZE`.
    #[must_use]
    pub fn with_repair_peer_sample_size(self, peer_sample_size: usize) -> Self {
        self.with_env("SOF_REPAIR_PEER_SAMPLE_SIZE", peer_sample_size.to_string())
    }

    /// Sets `SOF_REPAIR_SERVE_MAX_BYTES_PER_SEC`.
    #[must_use]
    pub fn with_repair_serve_max_bytes_per_sec(self, bytes_per_sec: usize) -> Self {
        self.with_env(
            "SOF_REPAIR_SERVE_MAX_BYTES_PER_SEC",
            bytes_per_sec.to_string(),
        )
    }

    /// Sets `SOF_REPAIR_SERVE_UNSTAKED_MAX_BYTES_PER_SEC`.
    #[must_use]
    pub fn with_repair_serve_unstaked_max_bytes_per_sec(self, bytes_per_sec: usize) -> Self {
        self.with_env(
            "SOF_REPAIR_SERVE_UNSTAKED_MAX_BYTES_PER_SEC",
            bytes_per_sec.to_string(),
        )
    }

    /// Sets `SOF_REPAIR_SERVE_MAX_REQUESTS_PER_PEER_PER_SEC`.
    #[must_use]
    pub fn with_repair_serve_max_requests_per_peer_per_sec(self, requests_per_sec: usize) -> Self {
        self.with_env(
            "SOF_REPAIR_SERVE_MAX_REQUESTS_PER_PEER_PER_SEC",
            requests_per_sec.to_string(),
        )
    }

    /// Sets `SOF_REPAIR_MAX_REQUESTS_PER_TICK`.
    #[must_use]
    pub fn with_repair_max_requests_per_tick(self, max_requests_per_tick: usize) -> Self {
        self.with_env(
            "SOF_REPAIR_MAX_REQUESTS_PER_TICK",
            max_requests_per_tick.to_string(),
        )
    }

    /// Sets `SOF_REPAIR_TIP_STALL_MS`.
    #[must_use]
    pub fn with_repair_tip_stall_ms(self, tip_stall_ms: u64) -> Self {
        self.with_env("SOF_REPAIR_TIP_STALL_MS", tip_stall_ms.to_string())
    }

    /// Sets `SOF_REPAIR_DATASET_STALL_MS`.
    #[must_use]
    pub fn with_repair_dataset_stall_ms(self, dataset_stall_ms: u64) -> Self {
        self.with_env("SOF_REPAIR_DATASET_STALL_MS", dataset_stall_ms.to_string())
    }

    /// Sets `SOF_REPAIR_STALL_SUSTAIN_MS`.
    #[must_use]
    pub fn with_repair_stall_sustain_ms(self, stall_sustain_ms: u64) -> Self {
        self.with_env("SOF_REPAIR_STALL_SUSTAIN_MS", stall_sustain_ms.to_string())
    }

    /// Sets `SOF_GOSSIP_RUNTIME_SWITCH_ENABLED`.
    #[must_use]
    pub fn with_gossip_runtime_switch_enabled(self, enabled: bool) -> Self {
        self.with_env("SOF_GOSSIP_RUNTIME_SWITCH_ENABLED", enabled.to_string())
    }

    /// Sets `SOF_GOSSIP_RUNTIME_SWITCH_STALL_MS`.
    #[must_use]
    pub fn with_gossip_runtime_switch_stall_ms(self, switch_stall_ms: u64) -> Self {
        self.with_env(
            "SOF_GOSSIP_RUNTIME_SWITCH_STALL_MS",
            switch_stall_ms.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_RUNTIME_SWITCH_DATASET_STALL_MS`.
    #[must_use]
    pub fn with_gossip_runtime_switch_dataset_stall_ms(self, switch_dataset_stall_ms: u64) -> Self {
        self.with_env(
            "SOF_GOSSIP_RUNTIME_SWITCH_DATASET_STALL_MS",
            switch_dataset_stall_ms.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_RUNTIME_SWITCH_COOLDOWN_MS`.
    #[must_use]
    pub fn with_gossip_runtime_switch_cooldown_ms(self, switch_cooldown_ms: u64) -> Self {
        self.with_env(
            "SOF_GOSSIP_RUNTIME_SWITCH_COOLDOWN_MS",
            switch_cooldown_ms.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_RUNTIME_SWITCH_WARMUP_MS`.
    #[must_use]
    pub fn with_gossip_runtime_switch_warmup_ms(self, switch_warmup_ms: u64) -> Self {
        self.with_env(
            "SOF_GOSSIP_RUNTIME_SWITCH_WARMUP_MS",
            switch_warmup_ms.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_RUNTIME_SWITCH_PROACTIVE_ENABLED`.
    #[must_use]
    pub fn with_gossip_runtime_switch_proactive_enabled(self, enabled: bool) -> Self {
        self.with_env(
            "SOF_GOSSIP_RUNTIME_SWITCH_PROACTIVE_ENABLED",
            enabled.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_RUNTIME_SWITCH_PROACTIVE_EVAL_MS`.
    #[must_use]
    pub fn with_gossip_runtime_switch_proactive_eval_ms(self, eval_ms: u64) -> Self {
        self.with_env(
            "SOF_GOSSIP_RUNTIME_SWITCH_PROACTIVE_EVAL_MS",
            eval_ms.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_RUNTIME_SWITCH_PROACTIVE_ACTIVE_RANK_MAX`.
    #[must_use]
    pub fn with_gossip_runtime_switch_proactive_active_rank_max(
        self,
        active_rank_max: usize,
    ) -> Self {
        self.with_env(
            "SOF_GOSSIP_RUNTIME_SWITCH_PROACTIVE_ACTIVE_RANK_MAX",
            active_rank_max.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_RUNTIME_SWITCH_PRIORITIZED_CANDIDATES_MAX`.
    #[must_use]
    pub fn with_gossip_runtime_switch_prioritized_candidates_max(
        self,
        candidates_max: usize,
    ) -> Self {
        self.with_env(
            "SOF_GOSSIP_RUNTIME_SWITCH_PRIORITIZED_CANDIDATES_MAX",
            candidates_max.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_RUNTIME_SWITCH_STABILIZE_MIN_PEERS`.
    #[must_use]
    pub fn with_gossip_runtime_switch_stabilize_min_peers(self, min_peers: usize) -> Self {
        self.with_env(
            "SOF_GOSSIP_RUNTIME_SWITCH_STABILIZE_MIN_PEERS",
            min_peers.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_RUNTIME_SWITCH_PROACTIVE_STABLE_EVALS`.
    #[must_use]
    pub fn with_gossip_runtime_switch_proactive_stable_evals(self, stable_evals: usize) -> Self {
        self.with_env(
            "SOF_GOSSIP_RUNTIME_SWITCH_PROACTIVE_STABLE_EVALS",
            stable_evals.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_RUNTIME_SWITCH_PROACTIVE_MIN_RUNTIME_AGE_MS`.
    #[must_use]
    pub fn with_gossip_runtime_switch_proactive_min_runtime_age_ms(self, age_ms: u64) -> Self {
        self.with_env(
            "SOF_GOSSIP_RUNTIME_SWITCH_PROACTIVE_MIN_RUNTIME_AGE_MS",
            age_ms.to_string(),
        )
    }

    /// Applies setup overrides to the runtime config layer.
    fn apply(&self) {
        crate::runtime_env::set_runtime_env_overrides(self.env_overrides.clone());
    }
}

/// Runtime adapter that applies typed gossip tuning through `RuntimeSetup` env overrides.
#[derive(Debug)]
struct RuntimeSetupTuningAdapter {
    /// Accumulated runtime setup under construction.
    setup: RuntimeSetup,
}

impl RuntimeSetupTuningAdapter {
    /// Creates a new runtime tuning adapter from an existing setup value.
    const fn new(setup: RuntimeSetup) -> Self {
        Self { setup }
    }

    /// Returns the fully projected runtime setup after port application.
    fn into_setup(self) -> RuntimeSetup {
        self.setup
    }
}

impl RuntimeTuningPort for RuntimeSetupTuningAdapter {
    fn set_ingest_queue_mode(&mut self, mode: IngestQueueMode) {
        self.setup = self.setup.clone().with_ingest_queue_mode_typed(mode);
    }

    fn set_ingest_queue_capacity(&mut self, capacity: QueueCapacity) {
        self.setup = self
            .setup
            .clone()
            .with_ingest_queue_capacity(capacity.get() as usize);
    }

    fn set_udp_batch_size(&mut self, batch_size: u16) {
        self.setup = self.setup.clone().with_udp_batch_size(batch_size as usize);
    }

    fn set_receiver_coalesce_window(&mut self, window: ReceiverCoalesceWindow) {
        self.setup = self
            .setup
            .clone()
            .with_udp_batch_max_wait_ms(window.as_millis_u64());
    }

    fn set_udp_receiver_core(&mut self, core: Option<CpuCoreIndex>) {
        if let Some(core_index) = core {
            self.setup = self.setup.clone().with_udp_receiver_core(core_index.get());
        }
    }

    fn set_udp_receiver_pin_by_port(&mut self, enabled: bool) {
        self.setup = self.setup.clone().with_udp_receiver_pin_by_port(enabled);
    }

    fn set_tvu_receive_sockets(&mut self, sockets: TvuReceiveSocketCount) {
        self.setup = self.setup.clone().with_tvu_receive_sockets(sockets.get());
    }

    fn set_gossip_channel_consume_capacity(&mut self, capacity: QueueCapacity) {
        self.setup = self.setup.clone().with_gossip_channel_consume_capacity(
            usize::try_from(capacity.get()).unwrap_or(usize::MAX),
        );
    }

    fn set_gossip_consume_threads(&mut self, thread_count: usize) {
        self.setup = self.setup.clone().with_gossip_consume_threads(thread_count);
    }

    fn set_gossip_listen_threads(&mut self, thread_count: usize) {
        self.setup = self.setup.clone().with_gossip_listen_threads(thread_count);
    }

    fn set_gossip_run_threads(&mut self, thread_count: usize) {
        self.setup = self.setup.clone().with_gossip_run_threads(thread_count);
    }

    fn set_shred_dedup_capacity(&mut self, dedupe_capacity: usize) {
        self.setup = self
            .setup
            .clone()
            .with_shred_dedupe_capacity(dedupe_capacity);
    }

    fn set_gossip_channel_tuning(&mut self, tuning: GossipChannelTuning) {
        self.setup = self
            .setup
            .clone()
            .with_gossip_receiver_channel_capacity(
                usize::try_from(tuning.gossip_receiver_channel_capacity.get())
                    .unwrap_or(usize::MAX),
            )
            .with_gossip_socket_consume_channel_capacity(
                usize::try_from(tuning.socket_consume_channel_capacity.get()).unwrap_or(usize::MAX),
            )
            .with_gossip_response_channel_capacity(
                usize::try_from(tuning.gossip_response_channel_capacity.get())
                    .unwrap_or(usize::MAX),
            );
    }
}

/// Runs the packaged observer runtime on a Tokio multi-thread runtime.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub fn run() -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(crate::app::runtime::run()?)
}

/// Runs the packaged observer runtime with a custom plugin host.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub fn run_with_plugin_host(plugin_host: PluginHost) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(crate::app::runtime::run_with_plugin_host(plugin_host)?)
}

/// Runs the packaged observer runtime with a custom runtime extension host.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub fn run_with_extension_host(extension_host: RuntimeExtensionHost) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(crate::app::runtime::run_with_extension_host(
        extension_host,
    )?)
}

/// Runs the packaged observer runtime with an explicit derived-state host.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub fn run_with_derived_state_host(
    derived_state_host: DerivedStateHost,
) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(crate::app::runtime::run_with_derived_state_host(
        derived_state_host,
    )?)
}

/// Runs the packaged observer runtime with explicit plugin and runtime extension hosts.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub fn run_with_hosts(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(crate::app::runtime::run_with_hosts(
        plugin_host,
        extension_host,
        DerivedStateHost::builder().build(),
    )?)
}

/// Runs the packaged observer runtime with explicit hosts, including derived-state consumers.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub fn run_with_hosts_and_derived_state_host(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(crate::app::runtime::run_with_hosts(
        plugin_host,
        extension_host,
        derived_state_host,
    )?)
}

/// Runs the packaged observer runtime with a derived-state host and explicit setup overrides.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub fn run_with_derived_state_host_and_setup(
    derived_state_host: DerivedStateHost,
    setup: &RuntimeSetup,
) -> Result<(), RuntimeError> {
    setup.apply();
    Ok(crate::app::runtime::run_with_derived_state_host(
        derived_state_host,
    )?)
}

/// Runs the packaged observer runtime with explicit hosts, a derived-state host, and setup overrides.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub fn run_with_hosts_and_derived_state_host_and_setup(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
    setup: &RuntimeSetup,
) -> Result<(), RuntimeError> {
    setup.apply();
    Ok(crate::app::runtime::run_with_hosts(
        plugin_host,
        extension_host,
        derived_state_host,
    )?)
}

/// Runs the packaged observer runtime with explicit code-driven setup overrides.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub fn run_with_setup(setup: &RuntimeSetup) -> Result<(), RuntimeError> {
    setup.apply();
    Ok(crate::app::runtime::run()?)
}

/// Runs the packaged observer runtime with a custom plugin host and explicit setup overrides.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub fn run_with_plugin_host_and_setup(
    plugin_host: PluginHost,
    setup: &RuntimeSetup,
) -> Result<(), RuntimeError> {
    setup.apply();
    Ok(crate::app::runtime::run_with_plugin_host(plugin_host)?)
}

/// Runs the packaged observer runtime with a custom runtime extension host and setup overrides.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub fn run_with_extension_host_and_setup(
    extension_host: RuntimeExtensionHost,
    setup: &RuntimeSetup,
) -> Result<(), RuntimeError> {
    setup.apply();
    Ok(crate::app::runtime::run_with_extension_host(
        extension_host,
    )?)
}

/// Runs the packaged observer runtime with explicit hosts and setup overrides.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub fn run_with_hosts_and_setup(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    setup: &RuntimeSetup,
) -> Result<(), RuntimeError> {
    setup.apply();
    Ok(crate::app::runtime::run_with_hosts(
        plugin_host,
        extension_host,
        DerivedStateHost::builder().build(),
    )?)
}

/// Programmatic observer runtime composition with an optional cooperative shutdown future.
///
/// This is the main entry point when embedding SOF into an application instead of
/// calling the crate-level convenience helpers.
///
/// # Examples
///
/// ```no_run
/// use async_trait::async_trait;
/// use sof::framework::{ObserverPlugin, PluginConfig, PluginHost};
/// use sof::runtime::{ObserverRuntime, RuntimeError};
///
/// struct TransactionLogger;
///
/// #[async_trait]
/// impl ObserverPlugin for TransactionLogger {
///     fn config(&self) -> PluginConfig {
///         PluginConfig::new().with_transaction()
///     }
/// }
///
/// #[tokio::main]
/// async fn main() -> Result<(), RuntimeError> {
/// let host = PluginHost::builder().add_plugin(TransactionLogger).build();
///
/// ObserverRuntime::new()
///     .with_plugin_host(host)
///     .run_until(async {
///         tokio::time::sleep(std::time::Duration::from_secs(30)).await;
///     })
///     .await
/// }
/// ```
#[derive(Default)]
pub struct ObserverRuntime {
    /// Plugin host invoked by the packaged observer runtime.
    plugin_host: PluginHost,
    /// Runtime extension host invoked by the packaged observer runtime.
    extension_host: RuntimeExtensionHost,
    /// Derived-state host invoked by the packaged observer runtime.
    derived_state_host: DerivedStateHost,
    /// Programmatic setup overrides applied before startup.
    setup: RuntimeSetup,
    /// Optional externally supplied ingress receiver used by kernel-bypass mode.
    #[cfg(feature = "kernel-bypass")]
    packet_ingest_rx: Option<KernelBypassIngressReceiver>,
}

impl ObserverRuntime {
    /// Creates a runtime composition using SOF defaults.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sof::runtime::ObserverRuntime;
    ///
    /// let _runtime = ObserverRuntime::new();
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the plugin host used by this runtime instance.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sof::framework::PluginHost;
    /// use sof::runtime::ObserverRuntime;
    ///
    /// let host = PluginHost::builder().build();
    /// let _runtime = ObserverRuntime::new().with_plugin_host(host);
    /// ```
    #[must_use]
    pub fn with_plugin_host(mut self, plugin_host: PluginHost) -> Self {
        self.plugin_host = plugin_host;
        self
    }

    /// Replaces the runtime extension host used by this runtime instance.
    #[must_use]
    pub fn with_extension_host(mut self, extension_host: RuntimeExtensionHost) -> Self {
        self.extension_host = extension_host;
        self
    }

    /// Replaces the derived-state host used by this runtime instance.
    #[must_use]
    pub fn with_derived_state_host(mut self, derived_state_host: DerivedStateHost) -> Self {
        self.derived_state_host = derived_state_host;
        self
    }

    /// Applies programmatic setup overrides before the runtime boots.
    #[must_use]
    pub fn with_setup(mut self, setup: RuntimeSetup) -> Self {
        self.setup = setup;
        self
    }

    #[cfg(feature = "kernel-bypass")]
    /// Replaces the built-in UDP ingress with an externally supplied kernel-bypass ingress receiver.
    #[must_use]
    pub fn with_kernel_bypass_ingress(
        mut self,
        packet_ingest_rx: impl Into<KernelBypassIngressReceiver>,
    ) -> Self {
        self.packet_ingest_rx = Some(packet_ingest_rx.into());
        self
    }

    /// Runs the configured runtime until it exits on its own.
    ///
    /// # Errors
    /// Returns any runtime initialization or shutdown error from the underlying observer runtime.
    pub async fn run(self) -> Result<(), RuntimeError> {
        self.run_with_optional_shutdown(None).await
    }

    async fn run_with_optional_shutdown(
        self,
        shutdown_signal: Option<ShutdownSignal>,
    ) -> Result<(), RuntimeError> {
        crate::runtime_env::clear_runtime_env_overrides();
        self.setup.apply();
        #[cfg(feature = "kernel-bypass")]
        if let Some(packet_ingest_rx) = self.packet_ingest_rx {
            return Ok(
                crate::app::runtime::run_async_with_hosts_and_kernel_bypass_ingress(
                    self.plugin_host,
                    self.extension_host,
                    self.derived_state_host,
                    shutdown_signal,
                    packet_ingest_rx,
                )
                .await?,
            );
        }
        Ok(crate::app::runtime::run_async_with_hosts(
            self.plugin_host,
            self.extension_host,
            self.derived_state_host,
            shutdown_signal,
        )
        .await?)
    }

    /// Runs the configured runtime until one shutdown future resolves.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use sof::runtime::{ObserverRuntime, RuntimeError};
    ///
    /// #[tokio::main]
    /// async fn main() -> Result<(), RuntimeError> {
    /// ObserverRuntime::new()
    ///     .run_until(async {
    ///         tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    ///     })
    ///     .await
    /// }
    /// ```
    ///
    /// # Errors
    /// Returns any runtime initialization or shutdown error from the underlying observer runtime.
    pub async fn run_until<F>(self, shutdown_signal: F) -> Result<(), RuntimeError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            shutdown_signal.await;
            if shutdown_tx.send(()).is_err() {}
        });
        self.run_with_optional_shutdown(Some(Box::pin(async move {
            if shutdown_rx.await.is_err() {
                tracing::warn!("runtime shutdown trigger task dropped before notifying runloop");
            }
        })))
        .await
    }

    /// Runs the configured runtime until the process receives a termination signal.
    ///
    /// On Unix this listens for `SIGTERM` and `SIGINT`. On other platforms it
    /// listens for the standard Ctrl-C shutdown event.
    ///
    /// # Errors
    /// Returns any runtime initialization or shutdown error from the underlying observer runtime.
    pub async fn run_until_termination_signal(self) -> Result<(), RuntimeError> {
        self.run_until(async {
            wait_for_termination_signal().await;
        })
        .await
    }
}

/// Waits for a process-level termination signal used by demo/example runtimes.
async fn wait_for_termination_signal() {
    #[cfg(unix)]
    {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let mut signals = match signal_hook::iterator::Signals::new([
                signal_hook::consts::signal::SIGTERM,
                signal_hook::consts::signal::SIGINT,
            ]) {
                Ok(signals) => signals,
                Err(error) => {
                    tracing::warn!(%error, "failed to register process signal listeners");
                    let _send_result = shutdown_tx.send(());
                    return;
                }
            };
            let _ = signals.forever().next();
            let _send_result = shutdown_tx.send(());
        });
        if shutdown_rx.await.is_err() {
            tracing::warn!("process signal listener dropped before notifying shutdown");
        }
    }

    #[cfg(not(unix))]
    {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "failed to wait for Ctrl-C shutdown signal");
        }
    }
}

/// Async variant of [`run`], for callers that already own a Tokio runtime.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async() -> Result<(), RuntimeError> {
    ObserverRuntime::new().run().await
}

/// Async variant of [`run_with_plugin_host`].
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_plugin_host(plugin_host: PluginHost) -> Result<(), RuntimeError> {
    ObserverRuntime::new()
        .with_plugin_host(plugin_host)
        .run()
        .await
}

/// Async variant of [`run_with_extension_host`].
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_extension_host(
    extension_host: RuntimeExtensionHost,
) -> Result<(), RuntimeError> {
    ObserverRuntime::new()
        .with_extension_host(extension_host)
        .run()
        .await
}

/// Async variant of [`run_with_derived_state_host`].
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_derived_state_host(
    derived_state_host: DerivedStateHost,
) -> Result<(), RuntimeError> {
    ObserverRuntime::new()
        .with_derived_state_host(derived_state_host)
        .run()
        .await
}

/// Async variant of [`run_with_hosts`].
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_hosts(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(crate::app::runtime::run_async_with_hosts(
        plugin_host,
        extension_host,
        DerivedStateHost::builder().build(),
        None,
    )
    .await?)
}

/// Async variant of [`run_with_hosts_and_derived_state_host`].
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_hosts_and_derived_state_host(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(crate::app::runtime::run_async_with_hosts(
        plugin_host,
        extension_host,
        derived_state_host,
        None,
    )
    .await?)
}

/// Async variant of [`run_with_derived_state_host_and_setup`].
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_derived_state_host_and_setup(
    derived_state_host: DerivedStateHost,
    setup: &RuntimeSetup,
) -> Result<(), RuntimeError> {
    setup.apply();
    Ok(crate::app::runtime::run_async_with_derived_state_host(derived_state_host).await?)
}

/// Async variant of [`run_with_hosts_and_derived_state_host_and_setup`].
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_hosts_and_derived_state_host_and_setup(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
    setup: &RuntimeSetup,
) -> Result<(), RuntimeError> {
    setup.apply();
    Ok(crate::app::runtime::run_async_with_hosts(
        plugin_host,
        extension_host,
        derived_state_host,
        None,
    )
    .await?)
}

#[cfg(feature = "kernel-bypass")]
/// External ingress sender type used by `kernel-bypass` integrations.
///
/// Producers publish [`crate::ingest::RawPacketBatch`] values through this queue.
pub type KernelBypassIngressSender = crate::ingest::RawPacketBatchSender;

#[cfg(feature = "kernel-bypass")]
/// External ingress receiver type used by `kernel-bypass` integrations.
pub type KernelBypassIngressReceiver = crate::ingest::RawPacketBatchReceiver;

#[cfg(feature = "kernel-bypass")]
/// Creates a kernel-bypass ingress queue pair.
///
/// Queue behavior is controlled by `SOF_INGEST_QUEUE_MODE` and
/// `SOF_INGEST_QUEUE_CAPACITY`, allowing bounded/unbounded/lock-free modes.
#[must_use]
pub fn create_kernel_bypass_ingress_queue()
-> (KernelBypassIngressSender, KernelBypassIngressReceiver) {
    crate::ingest::create_raw_packet_batch_queue()
}

#[cfg(feature = "kernel-bypass")]
/// Async runtime entrypoint that consumes packets from external `kernel-bypass` ingress.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_kernel_bypass_ingress(
    packet_ingest_rx: impl Into<KernelBypassIngressReceiver>,
) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(crate::app::runtime::run_async_with_kernel_bypass_ingress(packet_ingest_rx.into()).await?)
}

#[cfg(feature = "kernel-bypass")]
/// Async variant of [`run_async_with_plugin_host`] using external `kernel-bypass` ingress.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_plugin_host_and_kernel_bypass_ingress(
    plugin_host: PluginHost,
    packet_ingest_rx: impl Into<KernelBypassIngressReceiver>,
) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(
        crate::app::runtime::run_async_with_plugin_host_and_kernel_bypass_ingress(
            plugin_host,
            packet_ingest_rx.into(),
        )
        .await?,
    )
}

#[cfg(feature = "kernel-bypass")]
/// Async variant of [`run_async_with_extension_host`] using external `kernel-bypass` ingress.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_extension_host_and_kernel_bypass_ingress(
    extension_host: RuntimeExtensionHost,
    packet_ingest_rx: impl Into<KernelBypassIngressReceiver>,
) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(
        crate::app::runtime::run_async_with_extension_host_and_kernel_bypass_ingress(
            extension_host,
            packet_ingest_rx.into(),
        )
        .await?,
    )
}

#[cfg(feature = "kernel-bypass")]
/// Async variant of [`run_async_with_hosts`] using external `kernel-bypass` ingress.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_hosts_and_kernel_bypass_ingress(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    packet_ingest_rx: impl Into<KernelBypassIngressReceiver>,
) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(
        crate::app::runtime::run_async_with_hosts_and_kernel_bypass_ingress(
            plugin_host,
            extension_host,
            DerivedStateHost::builder().build(),
            None,
            packet_ingest_rx.into(),
        )
        .await?,
    )
}

#[cfg(feature = "kernel-bypass")]
/// Async runtime entrypoint using external ingress and an explicit derived-state host.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_derived_state_host_and_kernel_bypass_ingress(
    derived_state_host: DerivedStateHost,
    packet_ingest_rx: impl Into<KernelBypassIngressReceiver>,
) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(
        crate::app::runtime::run_async_with_derived_state_host_and_kernel_bypass_ingress(
            derived_state_host,
            packet_ingest_rx.into(),
        )
        .await?,
    )
}

#[cfg(feature = "kernel-bypass")]
/// Async runtime entrypoint using external ingress, a derived-state host, and setup overrides.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_derived_state_host_and_kernel_bypass_ingress_and_setup(
    derived_state_host: DerivedStateHost,
    packet_ingest_rx: impl Into<KernelBypassIngressReceiver>,
    setup: &RuntimeSetup,
) -> Result<(), RuntimeError> {
    setup.apply();
    Ok(
        crate::app::runtime::run_async_with_derived_state_host_and_kernel_bypass_ingress(
            derived_state_host,
            packet_ingest_rx.into(),
        )
        .await?,
    )
}

#[cfg(feature = "kernel-bypass")]
/// Async runtime entrypoint using explicit hosts and external ingress, including a derived-state host.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_hosts_and_derived_state_host_and_kernel_bypass_ingress(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
    packet_ingest_rx: impl Into<KernelBypassIngressReceiver>,
) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(
        crate::app::runtime::run_async_with_hosts_and_kernel_bypass_ingress(
            plugin_host,
            extension_host,
            derived_state_host,
            None,
            packet_ingest_rx.into(),
        )
        .await?,
    )
}

#[cfg(feature = "kernel-bypass")]
/// Async runtime entrypoint using explicit hosts, external ingress, a derived-state host, and setup overrides.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_hosts_and_derived_state_host_and_kernel_bypass_ingress_and_setup(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
    packet_ingest_rx: impl Into<KernelBypassIngressReceiver>,
    setup: &RuntimeSetup,
) -> Result<(), RuntimeError> {
    setup.apply();
    Ok(
        crate::app::runtime::run_async_with_hosts_and_kernel_bypass_ingress(
            plugin_host,
            extension_host,
            derived_state_host,
            None,
            packet_ingest_rx.into(),
        )
        .await?,
    )
}

/// Async variant of [`run_with_setup`], for callers that already own a Tokio runtime.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_setup(setup: &RuntimeSetup) -> Result<(), RuntimeError> {
    setup.apply();
    Ok(crate::app::runtime::run_async().await?)
}

/// Async variant of [`run_with_plugin_host_and_setup`].
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_plugin_host_and_setup(
    plugin_host: PluginHost,
    setup: &RuntimeSetup,
) -> Result<(), RuntimeError> {
    setup.apply();
    Ok(crate::app::runtime::run_async_with_plugin_host(plugin_host).await?)
}

/// Async variant of [`run_with_extension_host_and_setup`].
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_extension_host_and_setup(
    extension_host: RuntimeExtensionHost,
    setup: &RuntimeSetup,
) -> Result<(), RuntimeError> {
    setup.apply();
    Ok(crate::app::runtime::run_async_with_extension_host(extension_host).await?)
}

/// Async variant of [`run_with_hosts_and_setup`].
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_hosts_and_setup(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    setup: &RuntimeSetup,
) -> Result<(), RuntimeError> {
    setup.apply();
    Ok(crate::app::runtime::run_async_with_hosts(
        plugin_host,
        extension_host,
        DerivedStateHost::builder().build(),
        None,
    )
    .await?)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use sof_gossip_tuning::{GossipTuningProfile, HostProfilePreset, IngestQueueMode};

    #[test]
    fn typed_derived_state_config_serializes_into_env_overrides() {
        let setup = RuntimeSetup::new().with_derived_state_config(DerivedStateRuntimeConfig {
            checkpoint_interval_ms: 111,
            recovery_interval_ms: 222,
            replay: DerivedStateReplayConfig {
                backend: DerivedStateReplayBackend::Disk,
                replay_dir: PathBuf::from("/tmp/sof-derived-state"),
                durability: DerivedStateReplayDurability::Fsync,
                max_envelopes: 333,
                max_sessions: 4,
            },
        });
        let overrides = setup.env_overrides.into_iter().collect::<BTreeMap<_, _>>();

        assert_eq!(
            overrides.get("SOF_DERIVED_STATE_CHECKPOINT_INTERVAL_MS"),
            Some(&"111".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_DERIVED_STATE_RECOVERY_INTERVAL_MS"),
            Some(&"222".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_DERIVED_STATE_REPLAY_BACKEND"),
            Some(&"disk".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_DERIVED_STATE_REPLAY_DURABILITY"),
            Some(&"fsync".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_DERIVED_STATE_REPLAY_MAX_ENVELOPES"),
            Some(&"333".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_DERIVED_STATE_REPLAY_MAX_SESSIONS"),
            Some(&"4".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_DERIVED_STATE_REPLAY_DIR"),
            Some(&"/tmp/sof-derived-state".to_owned())
        );
    }

    #[test]
    fn checkpoint_only_replay_config_disables_runtime_tail() {
        let replay = DerivedStateReplayConfig::checkpoint_only();
        assert!(!replay.is_enabled());
        assert_eq!(replay.max_envelopes, 0);
        assert_eq!(replay.max_sessions, 0);
    }

    #[test]
    fn checkpoint_only_runtime_setup_serializes_zero_retention() {
        let setup = RuntimeSetup::new().with_derived_state_checkpoint_only();
        let overrides = setup.env_overrides.into_iter().collect::<BTreeMap<_, _>>();

        assert_eq!(
            overrides.get("SOF_DERIVED_STATE_REPLAY_MAX_ENVELOPES"),
            Some(&"0".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_DERIVED_STATE_REPLAY_MAX_SESSIONS"),
            Some(&"0".to_owned())
        );
    }

    #[test]
    fn typed_gossip_runtime_tuning_sets_expected_env_overrides() {
        let profile = GossipTuningProfile::preset(HostProfilePreset::Vps);
        let setup = RuntimeSetup::new().with_gossip_tuning_profile(profile);

        assert!(setup.env_overrides.contains(&(
            String::from("SOF_INGEST_QUEUE_MODE"),
            String::from("lockfree")
        )));
        assert!(
            setup
                .env_overrides
                .contains(&(String::from("SOF_TVU_SOCKETS"), String::from("4")))
        );
        assert!(setup.env_overrides.contains(&(
            String::from("SOF_UDP_RECEIVER_PIN_BY_PORT"),
            String::from("false")
        )));
        assert!(setup.env_overrides.contains(&(
            String::from("SOF_GOSSIP_RECEIVER_CHANNEL_CAPACITY"),
            String::from("131072")
        )));
        assert!(setup.env_overrides.contains(&(
            String::from("SOF_GOSSIP_SOCKET_CONSUME_CHANNEL_CAPACITY"),
            String::from("65536")
        )));
        assert!(setup.env_overrides.contains(&(
            String::from("SOF_GOSSIP_RESPONSE_CHANNEL_CAPACITY"),
            String::from("65536")
        )));
        assert!(setup.env_overrides.contains(&(
            String::from("SOF_GOSSIP_CHANNEL_CONSUME_CAPACITY"),
            String::from("4096")
        )));
        assert!(setup.env_overrides.contains(&(
            String::from("SOF_GOSSIP_CONSUME_THREADS"),
            String::from("4")
        )));
        assert!(
            setup
                .env_overrides
                .contains(&(String::from("SOF_GOSSIP_LISTEN_THREADS"), String::from("4")))
        );
        assert!(
            setup
                .env_overrides
                .contains(&(String::from("SOF_GOSSIP_RUN_THREADS"), String::from("4")))
        );
        assert!(setup.env_overrides.contains(&(
            String::from("SOF_SHRED_DEDUP_CAPACITY"),
            String::from("524288")
        )));
        assert!(setup.env_overrides.contains(&(
            String::from("SOF_UDP_RECEIVER_PIN_BY_PORT"),
            String::from("false")
        )));
    }

    #[test]
    fn runtime_dataset_health_thresholds_serialize_into_env_overrides() {
        let setup = RuntimeSetup::new()
            .with_runtime_dataset_decode_failures_unhealthy_per_tick(7)
            .with_runtime_dataset_tail_skips_unhealthy_per_tick(11)
            .with_runtime_dataset_unhealthy_sustain_ticks(5);
        let overrides = setup.env_overrides.into_iter().collect::<BTreeMap<_, _>>();

        assert_eq!(
            overrides.get("SOF_RUNTIME_DATASET_DECODE_FAILURES_UNHEALTHY_PER_TICK"),
            Some(&"7".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_RUNTIME_DATASET_TAIL_SKIPS_UNHEALTHY_PER_TICK"),
            Some(&"11".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_RUNTIME_DATASET_UNHEALTHY_SUSTAIN_TICKS"),
            Some(&"5".to_owned())
        );
    }

    #[test]
    fn direct_gossip_backend_tuning_sets_expected_env_overrides() {
        let setup = RuntimeSetup::new()
            .with_gossip_run_threads(6)
            .with_gossip_socket_consume_parallel_packet_threshold(2048)
            .with_gossip_stats_interval_secs(0)
            .with_gossip_sample_logs_enabled(false);

        assert!(
            setup
                .env_overrides
                .contains(&(String::from("SOF_GOSSIP_RUN_THREADS"), String::from("6")))
        );

        assert!(setup.env_overrides.contains(&(
            String::from("SOF_GOSSIP_SOCKET_CONSUME_PARALLEL_PACKET_THRESHOLD"),
            String::from("2048")
        )));
        assert!(setup.env_overrides.contains(&(
            String::from("SOF_GOSSIP_STATS_INTERVAL_SECS"),
            String::from("0")
        )));
        assert!(setup.env_overrides.contains(&(
            String::from("SOF_GOSSIP_SAMPLE_LOGS_ENABLED"),
            String::from("false")
        )));
    }

    #[test]
    fn typed_ingest_queue_mode_uses_expected_strings() {
        let setup = RuntimeSetup::new().with_ingest_queue_mode_typed(IngestQueueMode::Bounded);
        assert_eq!(
            setup.env_overrides.last(),
            Some(&(
                String::from("SOF_INGEST_QUEUE_MODE"),
                String::from("bounded")
            ))
        );
    }

    #[test]
    fn typed_observability_config_serializes_into_env_overrides() {
        let bind_addr: SocketAddr = "127.0.0.1:9108".parse().expect("valid bind addr");
        let setup = RuntimeSetup::new()
            .with_observability_config(RuntimeObservabilityConfig::with_bind_addr(bind_addr));
        let overrides = setup.env_overrides.into_iter().collect::<BTreeMap<_, _>>();

        assert_eq!(
            overrides.get("SOF_OBSERVABILITY_BIND"),
            Some(&"127.0.0.1:9108".to_owned())
        );
    }

    #[test]
    fn disabled_observability_config_is_not_serialized() {
        let setup =
            RuntimeSetup::new().with_observability_config(RuntimeObservabilityConfig::disabled());

        assert!(
            !setup
                .env_overrides
                .iter()
                .any(|(key, _)| key == "SOF_OBSERVABILITY_BIND")
        );
    }

    #[test]
    fn udp_busy_poll_overrides_serialize_into_env_overrides() {
        let setup = RuntimeSetup::new()
            .with_udp_busy_poll_us(50)
            .with_udp_busy_poll_budget(64)
            .with_udp_prefer_busy_poll(true);
        let overrides = setup.env_overrides.into_iter().collect::<BTreeMap<_, _>>();

        assert_eq!(
            overrides.get("SOF_UDP_BUSY_POLL_US"),
            Some(&"50".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_UDP_BUSY_POLL_BUDGET"),
            Some(&"64".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_UDP_PREFER_BUSY_POLL"),
            Some(&"true".to_owned())
        );
    }
}
