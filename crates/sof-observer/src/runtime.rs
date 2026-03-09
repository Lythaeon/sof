use std::{net::SocketAddr, path::PathBuf};

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
    /// This applies both the SOF runtime subset and the bundled gossip backend queue capacities.
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

/// Async variant of [`run`], for callers that already own a Tokio runtime.
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async() -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(crate::app::runtime::run_async().await?)
}

/// Async variant of [`run_with_plugin_host`].
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_plugin_host(plugin_host: PluginHost) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(crate::app::runtime::run_async_with_plugin_host(plugin_host).await?)
}

/// Async variant of [`run_with_extension_host`].
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_extension_host(
    extension_host: RuntimeExtensionHost,
) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(crate::app::runtime::run_async_with_extension_host(extension_host).await?)
}

/// Async variant of [`run_with_derived_state_host`].
///
/// # Errors
/// Returns any runtime initialization or shutdown error from the underlying observer runtime.
pub async fn run_async_with_derived_state_host(
    derived_state_host: DerivedStateHost,
) -> Result<(), RuntimeError> {
    crate::runtime_env::clear_runtime_env_overrides();
    Ok(crate::app::runtime::run_async_with_derived_state_host(derived_state_host).await?)
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
    Ok(
        crate::app::runtime::run_async_with_hosts(plugin_host, extension_host, derived_state_host)
            .await?,
    )
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
    Ok(
        crate::app::runtime::run_async_with_hosts(plugin_host, extension_host, derived_state_host)
            .await?,
    )
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
                .contains(&(String::from("SOF_TVU_SOCKETS"), String::from("2")))
        );
        assert!(setup.env_overrides.contains(&(
            String::from("SOF_GOSSIP_RECEIVER_CHANNEL_CAPACITY"),
            String::from("8192")
        )));
        assert!(setup.env_overrides.contains(&(
            String::from("SOF_UDP_RECEIVER_PIN_BY_PORT"),
            String::from("true")
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
}
