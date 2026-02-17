use std::net::SocketAddr;

use crate::framework::PluginHost;
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

    /// Sets `SOF_RELAY_LISTEN`.
    #[must_use]
    pub fn with_relay_listen_addr(self, relay_listen_addr: SocketAddr) -> Self {
        self.with_env("SOF_RELAY_LISTEN", relay_listen_addr.to_string())
    }

    /// Sets `SOF_RELAY_CONNECT` from a list of upstream relay addresses.
    #[must_use]
    pub fn with_relay_connect_addrs<I>(self, relay_connect_addrs: I) -> Self
    where
        I: IntoIterator<Item = SocketAddr>,
    {
        let serialized = relay_connect_addrs
            .into_iter()
            .map(|addr| addr.to_string())
            .collect::<Vec<_>>()
            .join(",");
        self.with_env("SOF_RELAY_CONNECT", serialized)
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

    /// Sets `SOF_RPC_URL`.
    #[must_use]
    pub fn with_rpc_url(self, rpc_url: impl Into<String>) -> Self {
        self.with_env("SOF_RPC_URL", rpc_url)
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

    /// Sets `SOF_VERIFY_RPC_SLOT_LEADERS`.
    #[must_use]
    pub fn with_verify_rpc_slot_leaders(self, enabled: bool) -> Self {
        self.with_env("SOF_VERIFY_RPC_SLOT_LEADERS", enabled.to_string())
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

    /// Sets `SOF_REPAIR_SEED_SLOTS`.
    #[must_use]
    pub fn with_repair_seed_slots(self, seed_slots: u64) -> Self {
        self.with_env("SOF_REPAIR_SEED_SLOTS", seed_slots.to_string())
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
