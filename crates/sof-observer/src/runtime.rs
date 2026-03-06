use std::net::SocketAddr;

use crate::framework::{DerivedStateHost, PluginHost, RuntimeExtensionHost};
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

    /// Sets `SOF_DATASET_MAX_TRACKED_SLOTS`.
    #[must_use]
    pub fn with_dataset_max_tracked_slots(self, max_tracked_slots: usize) -> Self {
        self.with_env(
            "SOF_DATASET_MAX_TRACKED_SLOTS",
            max_tracked_slots.to_string(),
        )
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

    /// Sets `SOF_DERIVED_STATE_REPLAY_BACKEND`.
    ///
    /// Accepted values:
    /// - `memory` (default): in-process retained tail
    /// - `disk`: retained tail persisted under `SOF_DERIVED_STATE_REPLAY_DIR`
    #[must_use]
    pub fn with_derived_state_replay_backend(self, backend: impl Into<String>) -> Self {
        self.with_env("SOF_DERIVED_STATE_REPLAY_BACKEND", backend)
    }

    /// Sets `SOF_DERIVED_STATE_REPLAY_DIR`.
    ///
    /// Used when `SOF_DERIVED_STATE_REPLAY_BACKEND=disk`.
    #[must_use]
    pub fn with_derived_state_replay_dir(self, replay_dir: impl Into<String>) -> Self {
        self.with_env("SOF_DERIVED_STATE_REPLAY_DIR", replay_dir)
    }

    /// Sets `SOF_DERIVED_STATE_REPLAY_DURABILITY`.
    ///
    /// Accepted values:
    /// - `flush` (default): flush buffered writes
    /// - `fsync`: flush and sync replay files to disk
    #[must_use]
    pub fn with_derived_state_replay_durability(self, durability: impl Into<String>) -> Self {
        self.with_env("SOF_DERIVED_STATE_REPLAY_DURABILITY", durability)
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
