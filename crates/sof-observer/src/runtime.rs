#![allow(clippy::missing_docs_in_private_items)]

use std::{
    collections::{HashMap, HashSet, VecDeque, hash_map::DefaultHasher, hash_map::Entry},
    future::Future,
    hash::{Hash, Hasher},
    net::SocketAddr,
    path::PathBuf,
    pin::Pin,
    time::Instant,
};

use crate::app::config::read_observability_bind_addr;
use crate::app::runtime::RuntimeObservabilityService;
use crate::framework::host::TransactionDispatchScope;
use crate::framework::{
    DerivedStateHost, DerivedStateReplayBackend, DerivedStateReplayDurability, PluginHost,
    RuntimeExtensionHost, TransactionEvent,
};
use crate::provider_stream::{
    ProviderSourceHealthEvent, ProviderSourceHealthStatus, ProviderSourceId,
    ProviderSourceIdentity, ProviderStreamMode, ProviderStreamReceiver, ProviderStreamUpdate,
};
use agave_transaction_view::transaction_view::SanitizedTransactionView;
use sof_gossip_tuning::{
    CpuCoreIndex, GossipChannelTuning, GossipTuningProfile, GossipTuningService, IngestQueueMode,
    QueueCapacity, ReceiverCoalesceWindow, RuntimeTuningPort, SofRuntimeTuning,
    TvuReceiveSocketCount,
};
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use thiserror::Error;

type ShutdownSignal = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
const PROVIDER_REPLAY_DEDUPE_CAPACITY: usize = 65_536;
const PROVIDER_REPLAY_DEDUPE_SLOT_WINDOW: u64 = 4_096;

/// Public runtime error surface for packaged SOF entrypoints.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// Tokio runtime initialization failed before ingest started.
    #[error("failed to build tokio runtime: {0}")]
    BuildTokioRuntime(std::io::Error),
    /// Provider-stream runtime exited with a structured operational error.
    #[error(transparent)]
    ProviderStream(#[from] ProviderStreamRuntimeError),
    /// Runtime runloop exited with an operational error.
    #[error("runtime runloop failed: {0}")]
    Runloop(String),
}

/// Structured provider-stream runtime error surface.
#[derive(Debug, Error)]
pub enum ProviderStreamRuntimeError {
    /// Provider-stream observability endpoint failed before ingest started.
    #[error("failed to start provider-stream observability endpoint on {bind_addr}: {detail}")]
    ObservabilityStart {
        /// Requested bind address for the provider runtime observability endpoint.
        bind_addr: SocketAddr,
        /// Underlying socket/listener failure string.
        detail: String,
    },
    /// One provider-stream startup stage failed before the runtime loop began.
    #[error("provider-stream startup failed: {message}")]
    Startup {
        /// Human-readable startup failure detail.
        message: String,
    },
    /// Requested hooks are not available for this provider-stream mode.
    #[error(
        "provider-stream mode {mode} does not support requested hooks: {unsupported_hooks:?}",
        mode = .mode.as_str()
    )]
    UnsupportedHooks {
        /// Provider-stream mode being validated.
        mode: ProviderStreamMode,
        /// Unsupported hook or feed labels.
        unsupported_hooks: Vec<String>,
    },
    /// Built-in provider modes rejected unsupported hooks that only generic producers can satisfy.
    #[error(
        "built-in provider-stream mode {mode} does not support requested hooks: {unsupported_hooks:?}; use the built-in config's runtime_mode() for matching built-in feeds, or ProviderStreamMode::Generic for custom or multi-source typed provider ingress",
        mode = .mode.as_str()
    )]
    BuiltInUnsupportedHooks {
        /// Built-in processed provider mode being validated.
        mode: ProviderStreamMode,
        /// Unsupported hook or feed labels.
        unsupported_hooks: Vec<String>,
    },
    /// Built-in provider runtime mode received an update from a mismatched source kind.
    #[error(
        "built-in provider-stream mode {mode} received {update_kind} from source kind {source_kind} ({source_instance})",
        mode = .mode.as_str()
    )]
    MismatchedSourceKind {
        /// Active provider-stream mode.
        mode: ProviderStreamMode,
        /// Incoming provider update family.
        update_kind: &'static str,
        /// Actual source kind label carried by the update.
        source_kind: String,
        /// Actual source instance label carried by the update.
        source_instance: String,
    },
    /// Built-in provider runtime mode received an update with no source identity.
    #[error(
        "built-in provider-stream mode {mode} received unattributed {update_kind}; built-in provider updates must carry source identity",
        mode = .mode.as_str()
    )]
    UnattributedBuiltInUpdate {
        /// Active provider-stream mode.
        mode: ProviderStreamMode,
        /// Incoming provider update family.
        update_kind: &'static str,
    },
    /// Provider ingress channel closed unexpectedly.
    #[error(
        "provider-stream ingress channel closed unexpectedly for mode {mode}",
        mode = .mode.as_str()
    )]
    IngressClosed {
        /// Provider-stream mode active when the ingress channel closed.
        mode: ProviderStreamMode,
        /// Last degraded source states observed before closure.
        degraded_sources: Vec<ProviderSourceHealthEvent>,
    },
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

/// Explicit trust posture for raw-shred ingest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShredTrustMode {
    /// Public gossip or public peers. Keep local verification on.
    PublicUntrusted,
    /// Trusted raw shred provider. Prefer provider trust over local verification cost.
    TrustedRawShredProvider,
}

impl ShredTrustMode {
    /// Returns the env-string representation used by `SOF_SHRED_TRUST_MODE`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PublicUntrusted => "public_untrusted",
            Self::TrustedRawShredProvider => "trusted_raw_shred_provider",
        }
    }
}

/// Startup policy for provider-stream modes when plugins request unsupported hooks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderStreamCapabilityPolicy {
    /// Log a warning and continue startup.
    Warn,
    /// Fail startup when any registered plugin requests an unsupported hook.
    Strict,
}

#[derive(Debug, Default)]
enum ProviderStreamCapabilityCheck {
    #[default]
    Supported,
    Warn {
        unsupported_hooks: Vec<String>,
    },
}

impl ProviderStreamCapabilityPolicy {
    /// Returns the env-string representation used by `SOF_PROVIDER_STREAM_CAPABILITY_POLICY`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Strict => "strict",
        }
    }

    fn from_runtime_env() -> Self {
        match crate::runtime_env::read_env_var("SOF_PROVIDER_STREAM_CAPABILITY_POLICY").as_deref() {
            Some("strict") => Self::Strict,
            _ => Self::Warn,
        }
    }
}

fn provider_stream_allow_eof_from_runtime_env() -> bool {
    matches!(
        crate::runtime_env::read_env_var("SOF_PROVIDER_STREAM_ALLOW_EOF").as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
    )
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

    /// Sets `SOF_GOSSIP_ENTRYPOINT_PINNED`.
    ///
    /// When enabled, runtime switching stays inside the configured entrypoint
    /// list instead of expanding to discovered gossip peers.
    #[must_use]
    pub fn with_gossip_entrypoint_pinned(self, pinned: bool) -> Self {
        self.with_env("SOF_GOSSIP_ENTRYPOINT_PINNED", pinned.to_string())
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

    /// Sets `SOF_VERIFY_SHREDS` explicitly, overriding the trust-mode default.
    #[must_use]
    pub fn with_verify_shreds(self, enabled: bool) -> Self {
        self.with_env("SOF_VERIFY_SHREDS", enabled.to_string())
    }

    /// Sets `SOF_SHRED_TRUST_MODE` for raw-shred ingest.
    #[must_use]
    pub fn with_shred_trust_mode(self, mode: ShredTrustMode) -> Self {
        self.with_env("SOF_SHRED_TRUST_MODE", mode.as_str())
    }

    /// Sets `SOF_PROVIDER_STREAM_CAPABILITY_POLICY`.
    #[must_use]
    pub fn with_provider_stream_capability_policy(
        self,
        policy: ProviderStreamCapabilityPolicy,
    ) -> Self {
        self.with_env("SOF_PROVIDER_STREAM_CAPABILITY_POLICY", policy.as_str())
    }

    /// Sets `SOF_PROVIDER_STREAM_ALLOW_EOF`.
    ///
    /// When enabled, `ProviderStreamMode::Generic` may close its ingress queue
    /// cleanly after emitting a bounded stream.
    #[must_use]
    pub fn with_provider_stream_allow_eof(self, allow_eof: bool) -> Self {
        self.with_env("SOF_PROVIDER_STREAM_ALLOW_EOF", allow_eof.to_string())
    }

    /// Sets `SOF_VERIFY_STRICT`.
    #[must_use]
    pub fn with_verify_strict_unknown(self, enabled: bool) -> Self {
        self.with_env("SOF_VERIFY_STRICT", enabled.to_string())
    }

    /// Sets `SOF_VERIFY_RECOVERED_SHREDS` explicitly, overriding the trust-mode default.
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

    /// Sets `SOF_GOSSIP_LOAD_SHED_ENABLED`.
    #[must_use]
    pub fn with_gossip_load_shed_enabled(self, enabled: bool) -> Self {
        self.with_env("SOF_GOSSIP_LOAD_SHED_ENABLED", enabled.to_string())
    }

    /// Sets `SOF_GOSSIP_LOAD_SHED_QUEUE_PRESSURE_PCT`.
    #[must_use]
    pub fn with_gossip_load_shed_queue_pressure_pct(self, pressure_pct: u64) -> Self {
        self.with_env(
            "SOF_GOSSIP_LOAD_SHED_QUEUE_PRESSURE_PCT",
            pressure_pct.to_string(),
        )
    }

    /// Sets `SOF_VERIFY_STRICT_UNKNOWN_QUEUE_PRESSURE_PCT`.
    #[must_use]
    pub fn with_verify_strict_unknown_queue_pressure_pct(self, pressure_pct: u64) -> Self {
        self.with_env(
            "SOF_VERIFY_STRICT_UNKNOWN_QUEUE_PRESSURE_PCT",
            pressure_pct.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_LOAD_SHED_KEEP_TOP_SOURCES`.
    #[must_use]
    pub fn with_gossip_load_shed_keep_top_sources(self, keep_top_sources: usize) -> Self {
        self.with_env(
            "SOF_GOSSIP_LOAD_SHED_KEEP_TOP_SOURCES",
            keep_top_sources.to_string(),
        )
    }

    /// Sets `SOF_GOSSIP_SOCKET_CONSUME_VERIFY_QUEUE_CAPACITY`.
    #[must_use]
    pub fn with_gossip_socket_consume_verify_queue_capacity(self, capacity: usize) -> Self {
        self.with_env(
            "SOF_GOSSIP_SOCKET_CONSUME_VERIFY_QUEUE_CAPACITY",
            capacity.to_string(),
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
    /// Optional externally supplied processed provider-stream receiver.
    provider_stream: Option<(ProviderStreamMode, ProviderStreamReceiver)>,
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

    /// Replaces the raw-shred runtime with a processed provider-stream ingress.
    ///
    /// Processed provider streams such as Yellowstone gRPC or LaserStream feed
    /// transactions directly into SOF's plugin/derived-state transaction
    /// surfaces. They bypass packet, shred, FEC, and reconstruction stages.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sof::{
    ///     provider_stream::{create_provider_stream_queue, ProviderStreamMode},
    ///     runtime::ObserverRuntime,
    /// };
    ///
    /// let (_tx, rx) = create_provider_stream_queue(128);
    /// let _runtime = ObserverRuntime::new()
    ///     .with_provider_stream_ingress(ProviderStreamMode::Generic, rx);
    /// ```
    #[must_use]
    pub fn with_provider_stream_ingress(
        mut self,
        mode: ProviderStreamMode,
        provider_stream_rx: ProviderStreamReceiver,
    ) -> Self {
        self.provider_stream = Some((mode, provider_stream_rx));
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
        if let Some((mode, provider_stream_rx)) = self.provider_stream {
            return run_provider_stream_runtime(
                self.plugin_host,
                self.extension_host,
                self.derived_state_host,
                shutdown_signal,
                mode,
                provider_stream_rx,
            )
            .await;
        }
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

async fn run_provider_stream_runtime(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
    shutdown_signal: Option<ShutdownSignal>,
    mode: ProviderStreamMode,
    mut provider_stream_rx: ProviderStreamReceiver,
) -> Result<(), RuntimeError> {
    let capability_check =
        enforce_provider_stream_capability_policy(mode, &plugin_host, &derived_state_host)?;
    let observability = if let Some(bind_addr) = read_observability_bind_addr() {
        let service = RuntimeObservabilityService::start(
            bind_addr,
            plugin_host.clone(),
            extension_host.clone(),
            derived_state_host.clone(),
        )
        .await
        .map_err(|source| ProviderStreamRuntimeError::ObservabilityStart {
            bind_addr,
            detail: source.to_string(),
        })?;
        tracing::info!(
            bind_addr = %service.local_addr(),
            mode = mode.as_str(),
            "provider-stream runtime observability endpoint enabled"
        );
        Some(service)
    } else {
        None
    };
    let observability_handle = observability
        .as_ref()
        .map(RuntimeObservabilityService::handle)
        .cloned();
    if let (Some(handle), ProviderStreamCapabilityCheck::Warn { unsupported_hooks }) =
        (observability_handle.as_ref(), &capability_check)
    {
        handle.observe_provider_capability_warning(mode.as_str(), unsupported_hooks);
    }
    plugin_host
        .startup()
        .await
        .map_err(|error| ProviderStreamRuntimeError::Startup {
            message: error.to_string(),
        })?;
    let extension_report = extension_host.startup().await;
    if extension_report.failed_extensions > 0 {
        tracing::warn!(
            mode = mode.as_str(),
            failed_extensions = extension_report.failed_extensions,
            "provider-stream runtime started with extension startup failures"
        );
    }
    derived_state_host.initialize();
    tracing::info!(mode = mode.as_str(), "starting SOF provider-stream runtime");

    let mut shutdown_signal = shutdown_signal;
    let mut replay_dedupe = ProviderReplayDedupe::new(PROVIDER_REPLAY_DEDUPE_CAPACITY);
    let mut provider_health = ProviderStreamHealth::default();
    let allow_eof =
        mode == ProviderStreamMode::Generic && provider_stream_allow_eof_from_runtime_env();
    let mut generic_progress_ready = false;
    let result = loop {
        tokio::select! {
            biased;
            () = async {
                if let Some(signal) = shutdown_signal.as_mut() {
                    signal.await;
                } else {
                    futures_util::future::pending::<()>().await;
                }
            } => {
                break Ok(());
            }
            update = provider_stream_rx.recv() => {
                let Some(update) = update else {
                    if allow_eof && provider_health.degraded_sources().is_empty() {
                        tracing::info!(
                            mode = mode.as_str(),
                            "SOF provider-stream runtime reached configured generic EOF"
                        );
                        break Ok(());
                    }
                    let error = provider_health.closed_error(mode);
                    tracing::error!(mode = mode.as_str(), error = %error, "SOF provider-stream runtime stopped after provider ingress closed unexpectedly");
                    break Err(RuntimeError::ProviderStream(error));
                };
                if let Err(error) = validate_provider_stream_update_mode(mode, &update) {
                    tracing::error!(mode = mode.as_str(), error = %error, "SOF provider-stream runtime received an update incompatible with the active provider mode");
                    break Err(RuntimeError::ProviderStream(error));
                }
                if let ProviderStreamUpdate::Health(event) = &update {
                    provider_health.observe(event);
                    if let Some(handle) = observability_handle.as_ref() {
                        handle.observe_provider_source_health(event);
                    }
                    continue;
                }
                if mode == ProviderStreamMode::Generic
                    && !generic_progress_ready
                    && !provider_health.has_sources()
                {
                    if let Some(handle) = observability_handle.as_ref() {
                        handle.mark_ready();
                    }
                    generic_progress_ready = true;
                }
                if replay_dedupe.observe(&update) {
                    continue;
                }
                dispatch_provider_stream_update(&plugin_host, &derived_state_host, update);
            }
        }
    };

    plugin_host.shutdown().await;
    extension_host.shutdown().await;
    if let Some(service) = observability {
        service.shutdown().await;
    }
    if result.is_ok() {
        tracing::info!(mode = mode.as_str(), "SOF provider-stream runtime stopped");
    }
    result
}

#[derive(Default)]
struct ProviderStreamHealth {
    sources: HashMap<ProviderSourceIdentity, ProviderSourceHealthEvent>,
}

impl ProviderStreamHealth {
    fn observe(&mut self, event: &ProviderSourceHealthEvent) {
        if matches!(event.status, ProviderSourceHealthStatus::Removed) {
            let removed = self.sources.remove(&event.source);
            if removed.is_some() {
                tracing::info!(
                    source_kind = event.source.kind_str(),
                    source_instance = event.source.instance_str(),
                    reason = event.reason.as_str(),
                    message = event.message.as_str(),
                    "provider source was removed from tracking"
                );
            }
            return;
        }
        let previous = self.sources.insert(event.source.clone(), event.clone());
        if previous.as_ref() == Some(event) {
            return;
        }
        match event.status {
            ProviderSourceHealthStatus::Healthy => {
                tracing::info!(
                    source_kind = event.source.kind_str(),
                    source_instance = event.source.instance_str(),
                    reason = event.reason.as_str(),
                    message = event.message.as_str(),
                    "provider source is healthy"
                );
            }
            ProviderSourceHealthStatus::Reconnecting => {
                tracing::warn!(
                    source_kind = event.source.kind_str(),
                    source_instance = event.source.instance_str(),
                    reason = event.reason.as_str(),
                    message = event.message.as_str(),
                    "provider source is reconnecting"
                );
            }
            ProviderSourceHealthStatus::Unhealthy => {
                tracing::error!(
                    source_kind = event.source.kind_str(),
                    source_instance = event.source.instance_str(),
                    reason = event.reason.as_str(),
                    message = event.message.as_str(),
                    "provider source is unhealthy"
                );
            }
            ProviderSourceHealthStatus::Removed => {}
        }
    }

    fn degraded_sources(&self) -> Vec<ProviderSourceHealthEvent> {
        let mut degraded = self
            .sources
            .values()
            .filter(|event| {
                matches!(
                    event.status,
                    ProviderSourceHealthStatus::Reconnecting
                        | ProviderSourceHealthStatus::Unhealthy
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        degraded.sort_by_key(|event| {
            (
                event.source.kind_str().to_owned(),
                event.source.instance_str().to_owned(),
            )
        });
        degraded
    }

    fn has_sources(&self) -> bool {
        !self.sources.is_empty()
    }

    fn closed_error(&self, mode: ProviderStreamMode) -> ProviderStreamRuntimeError {
        ProviderStreamRuntimeError::IngressClosed {
            mode,
            degraded_sources: self.degraded_sources(),
        }
    }
}

fn validate_provider_stream_update_mode(
    mode: ProviderStreamMode,
    update: &ProviderStreamUpdate,
) -> Result<(), ProviderStreamRuntimeError> {
    if mode == ProviderStreamMode::Generic {
        return Ok(());
    }
    let update_kind = provider_stream_update_kind(update);
    let Some(source) = provider_stream_update_source(update) else {
        return Err(ProviderStreamRuntimeError::UnattributedBuiltInUpdate { mode, update_kind });
    };
    if provider_stream_mode_accepts_source_kind(mode, &source.kind) {
        return Ok(());
    }
    Err(ProviderStreamRuntimeError::MismatchedSourceKind {
        mode,
        update_kind,
        source_kind: source.kind_str().to_owned(),
        source_instance: source.instance_str().to_owned(),
    })
}

const fn provider_stream_update_kind(update: &ProviderStreamUpdate) -> &'static str {
    match update {
        ProviderStreamUpdate::Transaction(_) => "transaction update",
        ProviderStreamUpdate::SerializedTransaction(_) => "serialized transaction update",
        ProviderStreamUpdate::TransactionLog(_) => "transaction log update",
        ProviderStreamUpdate::TransactionStatus(_) => "transaction-status update",
        ProviderStreamUpdate::TransactionViewBatch(_) => "transaction view-batch update",
        ProviderStreamUpdate::AccountUpdate(_) => "account update",
        ProviderStreamUpdate::BlockMeta(_) => "block-meta update",
        ProviderStreamUpdate::RecentBlockhash(_) => "recent-blockhash update",
        ProviderStreamUpdate::SlotStatus(_) => "slot-status update",
        ProviderStreamUpdate::ClusterTopology(_) => "cluster-topology update",
        ProviderStreamUpdate::LeaderSchedule(_) => "leader-schedule update",
        ProviderStreamUpdate::Reorg(_) => "reorg update",
        ProviderStreamUpdate::Health(_) => "health update",
    }
}

fn provider_stream_update_source(update: &ProviderStreamUpdate) -> Option<&ProviderSourceIdentity> {
    match update {
        ProviderStreamUpdate::Transaction(event) => event.provider_source.as_deref(),
        ProviderStreamUpdate::SerializedTransaction(event) => event.provider_source.as_deref(),
        ProviderStreamUpdate::TransactionLog(event) => event.provider_source.as_deref(),
        ProviderStreamUpdate::TransactionStatus(event) => event.provider_source.as_deref(),
        ProviderStreamUpdate::TransactionViewBatch(event) => event.provider_source.as_deref(),
        ProviderStreamUpdate::AccountUpdate(event) => event.provider_source.as_deref(),
        ProviderStreamUpdate::BlockMeta(event) => event.provider_source.as_deref(),
        ProviderStreamUpdate::RecentBlockhash(event) => event.provider_source.as_deref(),
        ProviderStreamUpdate::SlotStatus(event) => event.provider_source.as_deref(),
        ProviderStreamUpdate::ClusterTopology(event) => event.provider_source.as_deref(),
        ProviderStreamUpdate::LeaderSchedule(event) => event.provider_source.as_deref(),
        ProviderStreamUpdate::Reorg(event) => event.provider_source.as_deref(),
        ProviderStreamUpdate::Health(event) => Some(&event.source),
    }
}

const fn provider_stream_mode_accepts_source_kind(
    mode: ProviderStreamMode,
    source_kind: &ProviderSourceId,
) -> bool {
    match (mode, source_kind) {
        (ProviderStreamMode::Generic, _) => true,
        (ProviderStreamMode::YellowstoneGrpc, ProviderSourceId::YellowstoneGrpc) => true,
        (
            ProviderStreamMode::YellowstoneGrpcTransactionStatus,
            ProviderSourceId::YellowstoneGrpcTransactionStatus,
        ) => true,
        (
            ProviderStreamMode::YellowstoneGrpcAccounts,
            ProviderSourceId::YellowstoneGrpcAccounts,
        ) => true,
        (
            ProviderStreamMode::YellowstoneGrpcBlockMeta,
            ProviderSourceId::YellowstoneGrpcBlockMeta,
        ) => true,
        (ProviderStreamMode::YellowstoneGrpcSlots, ProviderSourceId::YellowstoneGrpcSlots) => true,
        (ProviderStreamMode::LaserStream, ProviderSourceId::LaserStream) => true,
        (
            ProviderStreamMode::LaserStreamTransactionStatus,
            ProviderSourceId::LaserStreamTransactionStatus,
        ) => true,
        (ProviderStreamMode::LaserStreamAccounts, ProviderSourceId::LaserStreamAccounts) => true,
        (ProviderStreamMode::LaserStreamBlockMeta, ProviderSourceId::LaserStreamBlockMeta) => true,
        (ProviderStreamMode::LaserStreamSlots, ProviderSourceId::LaserStreamSlots) => true,
        #[cfg(feature = "provider-websocket")]
        (ProviderStreamMode::WebsocketTransaction, ProviderSourceId::WebsocketTransaction) => true,
        #[cfg(feature = "provider-websocket")]
        (ProviderStreamMode::WebsocketLogs, ProviderSourceId::WebsocketLogs) => true,
        #[cfg(feature = "provider-websocket")]
        (ProviderStreamMode::WebsocketAccount, ProviderSourceId::WebsocketAccount) => true,
        #[cfg(feature = "provider-websocket")]
        (ProviderStreamMode::WebsocketProgram, ProviderSourceId::WebsocketProgram) => true,
        _ => false,
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum ProviderReplayLogicalKey {
    Transaction {
        slot: u64,
        signature: Signature,
        commitment_status: u8,
        confirmed_slot: Option<u64>,
        finalized_slot: Option<u64>,
    },
    SerializedTransaction {
        slot: u64,
        commitment_status: u8,
        confirmed_slot: Option<u64>,
        finalized_slot: Option<u64>,
        fingerprint: u64,
    },
    ControlPlane {
        slot: u64,
        kind: u8,
        fingerprint: u64,
    },
}

impl ProviderReplayLogicalKey {
    const fn slot(&self) -> u64 {
        match self {
            Self::Transaction { slot, .. }
            | Self::SerializedTransaction { slot, .. }
            | Self::ControlPlane { slot, .. } => *slot,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ProviderReplayObservedKey {
    source: Option<crate::provider_stream::ProviderSourceRef>,
    logical: ProviderReplayLogicalKey,
}

impl ProviderReplayObservedKey {
    const fn slot(&self) -> u64 {
        self.logical.slot()
    }
}

#[derive(Clone, Debug)]
struct ProviderReplayArbitratedWinner {
    priority: u16,
    source: crate::provider_stream::ProviderSourceRef,
}

#[derive(Default)]
struct ProviderReplayDedupe {
    seen: HashSet<ProviderReplayObservedKey>,
    order: VecDeque<ProviderReplayObservedKey>,
    arbitrated: HashMap<ProviderReplayLogicalKey, ProviderReplayArbitratedWinner>,
    arbitrated_order: VecDeque<ProviderReplayLogicalKey>,
    capacity: usize,
    max_slot_seen: u64,
}

impl ProviderReplayDedupe {
    fn new(capacity: usize) -> Self {
        Self {
            seen: HashSet::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            arbitrated: HashMap::with_capacity(capacity),
            arbitrated_order: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
            max_slot_seen: 0,
        }
    }

    fn observe(&mut self, update: &ProviderStreamUpdate) -> bool {
        let Some(logical) = provider_replay_dedupe_key(update) else {
            return false;
        };
        self.max_slot_seen = self.max_slot_seen.max(logical.slot());
        let source = provider_stream_update_source_ref(update);
        let observed = ProviderReplayObservedKey {
            source: source.clone(),
            logical: logical.clone(),
        };
        if !self.seen.insert(observed.clone()) {
            return true;
        }
        self.order.push_back(observed);

        let Some(source) = source else {
            self.evict();
            return false;
        };

        match source.arbitration() {
            crate::provider_stream::ProviderSourceArbitrationMode::EmitAll => {
                self.evict();
                false
            }
            crate::provider_stream::ProviderSourceArbitrationMode::FirstSeen => {
                match self.arbitrated.entry(logical.clone()) {
                    Entry::Occupied(_) => {
                        self.evict();
                        true
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(ProviderReplayArbitratedWinner {
                            priority: source.priority(),
                            source,
                        });
                        self.arbitrated_order.push_back(logical);
                        self.evict();
                        false
                    }
                }
            }
            crate::provider_stream::ProviderSourceArbitrationMode::FirstSeenThenPromote => {
                match self.arbitrated.entry(logical.clone()) {
                    Entry::Occupied(mut entry) => {
                        let winner = entry.get_mut();
                        if source.priority() > winner.priority {
                            winner.priority = source.priority();
                            winner.source = source;
                            self.evict();
                            false
                        } else {
                            self.evict();
                            true
                        }
                    }
                    Entry::Vacant(entry) => {
                        entry.insert(ProviderReplayArbitratedWinner {
                            priority: source.priority(),
                            source,
                        });
                        self.arbitrated_order.push_back(logical);
                        self.evict();
                        false
                    }
                }
            }
        }
    }

    fn evict(&mut self) {
        let min_slot = self
            .max_slot_seen
            .saturating_sub(PROVIDER_REPLAY_DEDUPE_SLOT_WINDOW);
        while let Some(oldest) = self.order.front().cloned() {
            if self.order.len() <= self.capacity && oldest.slot() >= min_slot {
                break;
            }
            let _ = self.order.pop_front();
            self.seen.remove(&oldest);
        }
        while let Some(oldest) = self.arbitrated_order.front().cloned() {
            if self.arbitrated_order.len() <= self.capacity && oldest.slot() >= min_slot {
                break;
            }
            let _ = self.arbitrated_order.pop_front();
            self.arbitrated.remove(&oldest);
        }
    }
}

fn provider_stream_update_source_ref(
    update: &ProviderStreamUpdate,
) -> Option<crate::provider_stream::ProviderSourceRef> {
    match update {
        ProviderStreamUpdate::Transaction(event) => event.provider_source.clone(),
        ProviderStreamUpdate::SerializedTransaction(event) => event.provider_source.clone(),
        ProviderStreamUpdate::TransactionLog(event) => event.provider_source.clone(),
        ProviderStreamUpdate::TransactionStatus(event) => event.provider_source.clone(),
        ProviderStreamUpdate::TransactionViewBatch(event) => event.provider_source.clone(),
        ProviderStreamUpdate::AccountUpdate(event) => event.provider_source.clone(),
        ProviderStreamUpdate::BlockMeta(event) => event.provider_source.clone(),
        ProviderStreamUpdate::RecentBlockhash(event) => event.provider_source.clone(),
        ProviderStreamUpdate::SlotStatus(event) => event.provider_source.clone(),
        ProviderStreamUpdate::ClusterTopology(event) => event.provider_source.clone(),
        ProviderStreamUpdate::LeaderSchedule(event) => event.provider_source.clone(),
        ProviderStreamUpdate::Reorg(event) => event.provider_source.clone(),
        ProviderStreamUpdate::Health(_) => None,
    }
}

fn provider_replay_dedupe_key(update: &ProviderStreamUpdate) -> Option<ProviderReplayLogicalKey> {
    match update {
        ProviderStreamUpdate::Transaction(event) => event
            .signature
            .map(crate::framework::SignatureBytes::to_solana)
            .or_else(|| event.tx.signatures.first().copied())
            .map(|signature| ProviderReplayLogicalKey::Transaction {
                slot: event.slot,
                signature,
                commitment_status: provider_replay_commitment_key(event.commitment_status),
                confirmed_slot: event.confirmed_slot,
                finalized_slot: event.finalized_slot,
            }),
        ProviderStreamUpdate::SerializedTransaction(event) => event
            .signature
            .map(crate::framework::SignatureBytes::to_solana)
            .map_or_else(
                || {
                    Some(ProviderReplayLogicalKey::SerializedTransaction {
                        slot: event.slot,
                        commitment_status: provider_replay_commitment_key(event.commitment_status),
                        confirmed_slot: event.confirmed_slot,
                        finalized_slot: event.finalized_slot,
                        fingerprint: provider_replay_fingerprint(&event.bytes),
                    })
                },
                |signature| {
                    Some(ProviderReplayLogicalKey::Transaction {
                        slot: event.slot,
                        signature,
                        commitment_status: provider_replay_commitment_key(event.commitment_status),
                        confirmed_slot: event.confirmed_slot,
                        finalized_slot: event.finalized_slot,
                    })
                },
            ),
        ProviderStreamUpdate::RecentBlockhash(event) => {
            Some(ProviderReplayLogicalKey::ControlPlane {
                slot: event.slot,
                kind: 1,
                fingerprint: provider_replay_fingerprint(event),
            })
        }
        ProviderStreamUpdate::SlotStatus(event) => Some(ProviderReplayLogicalKey::ControlPlane {
            slot: event.slot,
            kind: 2,
            fingerprint: provider_replay_fingerprint(event),
        }),
        ProviderStreamUpdate::ClusterTopology(event) => {
            Some(ProviderReplayLogicalKey::ControlPlane {
                slot: event.slot.unwrap_or_default(),
                kind: 3,
                fingerprint: provider_replay_fingerprint(event),
            })
        }
        ProviderStreamUpdate::LeaderSchedule(event) => {
            Some(ProviderReplayLogicalKey::ControlPlane {
                slot: event.slot.unwrap_or_default(),
                kind: 4,
                fingerprint: provider_replay_fingerprint(event),
            })
        }
        ProviderStreamUpdate::Reorg(event) => Some(ProviderReplayLogicalKey::ControlPlane {
            slot: event.new_tip,
            kind: 5,
            fingerprint: provider_replay_fingerprint(event),
        }),
        ProviderStreamUpdate::TransactionLog(event) => {
            Some(ProviderReplayLogicalKey::ControlPlane {
                slot: event.slot,
                kind: 6,
                fingerprint: provider_replay_transaction_log_fingerprint(event),
            })
        }
        ProviderStreamUpdate::TransactionStatus(event) => {
            Some(ProviderReplayLogicalKey::ControlPlane {
                slot: event.slot,
                kind: 8,
                fingerprint: provider_replay_fingerprint(event),
            })
        }
        ProviderStreamUpdate::TransactionViewBatch(event) => {
            Some(ProviderReplayLogicalKey::ControlPlane {
                slot: event.slot,
                kind: 7,
                fingerprint: provider_replay_transaction_view_batch_fingerprint(event),
            })
        }
        ProviderStreamUpdate::AccountUpdate(event) => {
            Some(ProviderReplayLogicalKey::ControlPlane {
                slot: event.slot,
                kind: 9,
                fingerprint: provider_replay_fingerprint(event),
            })
        }
        ProviderStreamUpdate::BlockMeta(event) => Some(ProviderReplayLogicalKey::ControlPlane {
            slot: event.slot,
            kind: 10,
            fingerprint: provider_replay_fingerprint(event),
        }),
        ProviderStreamUpdate::Health(_) => None,
    }
}

fn provider_replay_fingerprint<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn provider_replay_transaction_log_fingerprint(
    event: &crate::framework::TransactionLogEvent,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    event.signature.hash(&mut hasher);
    provider_replay_commitment_key(event.commitment_status).hash(&mut hasher);
    event.matched_filter.hash(&mut hasher);
    if let Some(err) = &event.err {
        err.to_string().hash(&mut hasher);
    }
    for line in event.logs.iter() {
        line.hash(&mut hasher);
    }
    hasher.finish()
}

fn provider_replay_transaction_view_batch_fingerprint(
    event: &crate::framework::TransactionViewBatchEvent,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    event.start_index.hash(&mut hasher);
    event.end_index.hash(&mut hasher);
    event.last_in_slot.hash(&mut hasher);
    event.shreds.hash(&mut hasher);
    event.payload_len.hash(&mut hasher);
    provider_replay_commitment_key(event.commitment_status).hash(&mut hasher);
    event.confirmed_slot.hash(&mut hasher);
    event.finalized_slot.hash(&mut hasher);
    event.payload.as_ref().hash(&mut hasher);
    for range in event.transactions.iter() {
        range.hash(&mut hasher);
    }
    hasher.finish()
}

const fn provider_replay_commitment_key(commitment_status: crate::event::TxCommitmentStatus) -> u8 {
    match commitment_status {
        crate::event::TxCommitmentStatus::Processed => 0,
        crate::event::TxCommitmentStatus::Confirmed => 1,
        crate::event::TxCommitmentStatus::Finalized => 2,
    }
}

fn dispatch_provider_stream_update(
    plugin_host: &PluginHost,
    derived_state_host: &DerivedStateHost,
    update: ProviderStreamUpdate,
) {
    if plugin_host.is_empty() && derived_state_host.is_empty() {
        return;
    }
    match update {
        ProviderStreamUpdate::Transaction(event) => {
            if plugin_host.wants_recent_blockhash() {
                plugin_host.on_recent_blockhash(crate::framework::ObservedRecentBlockhashEvent {
                    slot: event.slot,
                    recent_blockhash: event.tx.message.recent_blockhash().to_bytes(),
                    dataset_tx_count: 1,
                    provider_source: event.provider_source.clone(),
                });
            }
            if derived_state_host.wants_transaction_applied() {
                let tx_index = derived_state_host.reserve_slot_tx_indexes(event.slot, 1);
                derived_state_host.on_transaction(tx_index, event.clone());
            }
            if plugin_host.wants_transaction() {
                plugin_host.on_transaction(event);
            }
        }
        ProviderStreamUpdate::SerializedTransaction(event) => {
            dispatch_provider_stream_serialized_transaction(
                plugin_host,
                derived_state_host,
                &event,
            );
        }
        ProviderStreamUpdate::TransactionLog(event) => {
            if plugin_host.wants_transaction_log() {
                plugin_host.on_transaction_log(event);
            }
        }
        ProviderStreamUpdate::TransactionStatus(event) => {
            if derived_state_host.wants_transaction_status_observed() {
                derived_state_host.on_transaction_status(event.clone());
            }
            if plugin_host.wants_transaction_status() {
                plugin_host.on_transaction_status(event);
            }
        }
        ProviderStreamUpdate::TransactionViewBatch(event) => {
            if plugin_host.wants_transaction_view_batch() {
                plugin_host.on_transaction_view_batch(event, Instant::now());
            }
        }
        ProviderStreamUpdate::AccountUpdate(event) => {
            if plugin_host.wants_account_update() {
                plugin_host.on_account_update(event);
            }
        }
        ProviderStreamUpdate::BlockMeta(event) => {
            if derived_state_host.wants_block_meta_observed() {
                derived_state_host.on_block_meta(event.clone());
            }
            if plugin_host.wants_block_meta() {
                plugin_host.on_block_meta(event);
            }
        }
        ProviderStreamUpdate::RecentBlockhash(event) => {
            if !derived_state_host.is_empty() {
                derived_state_host.on_recent_blockhash(event.clone());
            }
            if plugin_host.wants_recent_blockhash() {
                plugin_host.on_recent_blockhash(event);
            }
        }
        ProviderStreamUpdate::SlotStatus(event) => {
            if !derived_state_host.is_empty() {
                derived_state_host.on_slot_status(event.clone());
            }
            if plugin_host.wants_slot_status() {
                plugin_host.on_slot_status(event);
            }
        }
        ProviderStreamUpdate::ClusterTopology(event) => {
            if !derived_state_host.is_empty() {
                derived_state_host.on_cluster_topology(event.clone());
            }
            if plugin_host.wants_cluster_topology() {
                plugin_host.on_cluster_topology(event);
            }
        }
        ProviderStreamUpdate::LeaderSchedule(event) => {
            if !derived_state_host.is_empty() {
                derived_state_host.on_leader_schedule(event.clone());
            }
            if plugin_host.wants_leader_schedule() {
                plugin_host.on_leader_schedule(event);
            }
        }
        ProviderStreamUpdate::Reorg(event) => {
            if !derived_state_host.is_empty() {
                derived_state_host.on_reorg(event.clone());
            }
            if plugin_host.wants_reorg() {
                plugin_host.on_reorg(event);
            }
        }
        ProviderStreamUpdate::Health(_) => {}
    }
}

fn dispatch_provider_stream_serialized_transaction(
    plugin_host: &PluginHost,
    derived_state_host: &DerivedStateHost,
    event: &crate::provider_stream::SerializedTransactionEvent,
) {
    let derived_state_empty = derived_state_host.is_empty();
    let wants_transaction = plugin_host.transaction_enabled_at_commitment(event.commitment_status);
    let wants_recent_blockhash = plugin_host.wants_recent_blockhash();
    let wants_derived_state_transaction =
        !derived_state_empty && derived_state_host.wants_transaction_applied();
    if !wants_transaction && !wants_recent_blockhash && !wants_derived_state_transaction {
        return;
    }

    let mut signature = event.signature;
    let mut recent_blockhash = None;
    let mut kind = None;
    let needs_view_prefilter = wants_transaction
        && plugin_host.has_transaction_prefilter_at_commitment(event.commitment_status)
        && !wants_derived_state_transaction;
    let should_try_view = wants_recent_blockhash || needs_view_prefilter || wants_transaction;
    if should_try_view
        && let Ok(view) = SanitizedTransactionView::try_new_sanitized(event.bytes.as_ref(), true)
    {
        if signature.is_none() {
            signature = view
                .signatures()
                .first()
                .copied()
                .map(crate::framework::SignatureBytes::from_solana);
        }
        kind = Some(crate::provider_stream::classify_provider_transaction_kind_view(&view));
        if wants_recent_blockhash {
            recent_blockhash = Some(view.recent_blockhash().to_bytes());
        }
        if needs_view_prefilter {
            let prefiltered = plugin_host.classify_transaction_view_in_scope(
                &view,
                event.commitment_status,
                TransactionDispatchScope::All,
            );
            if !prefiltered.needs_full_classification
                && prefiltered.dispatch.is_empty()
                && !wants_derived_state_transaction
            {
                if let Some(recent_blockhash) = recent_blockhash {
                    plugin_host.on_recent_blockhash(
                        crate::framework::ObservedRecentBlockhashEvent {
                            slot: event.slot,
                            recent_blockhash,
                            dataset_tx_count: 1,
                            provider_source: event.provider_source.clone(),
                        },
                    );
                }
                return;
            }
        }
        if !wants_transaction && !wants_derived_state_transaction {
            if let Some(recent_blockhash) = recent_blockhash {
                plugin_host.on_recent_blockhash(crate::framework::ObservedRecentBlockhashEvent {
                    slot: event.slot,
                    recent_blockhash,
                    dataset_tx_count: 1,
                    provider_source: event.provider_source.clone(),
                });
            }
            return;
        }
    }

    let Ok(tx) = bincode::deserialize::<VersionedTransaction>(event.bytes.as_ref()) else {
        tracing::warn!(
            slot = event.slot,
            "failed to deserialize provider serialized transaction"
        );
        return;
    };
    let tx = std::sync::Arc::new(tx);
    let event = TransactionEvent {
        slot: event.slot,
        commitment_status: event.commitment_status,
        confirmed_slot: event.confirmed_slot,
        finalized_slot: event.finalized_slot,
        signature: signature.or_else(|| {
            tx.signatures
                .first()
                .copied()
                .map(crate::framework::SignatureBytes::from_solana)
        }),
        provider_source: event.provider_source.clone(),
        kind: kind
            .unwrap_or_else(|| crate::provider_stream::classify_provider_transaction_kind(&tx)),
        tx,
    };
    if wants_recent_blockhash {
        plugin_host.on_recent_blockhash(crate::framework::ObservedRecentBlockhashEvent {
            slot: event.slot,
            recent_blockhash: recent_blockhash
                .unwrap_or_else(|| event.tx.message.recent_blockhash().to_bytes()),
            dataset_tx_count: 1,
            provider_source: event.provider_source.clone(),
        });
    }
    if wants_derived_state_transaction {
        let tx_index = derived_state_host.reserve_slot_tx_indexes(event.slot, 1);
        derived_state_host.on_transaction(tx_index, event.clone());
    }
    if wants_transaction {
        plugin_host.on_transaction(event);
    }
}

fn provider_stream_unsupported_hooks(
    mode: ProviderStreamMode,
    plugin_host: &PluginHost,
    derived_state_host: &DerivedStateHost,
) -> Vec<&'static str> {
    let supports_account_update = provider_stream_mode_supports_account_update(mode);
    let supports_block_meta = provider_stream_mode_supports_block_meta(mode);
    let supports_recent_blockhash = provider_stream_mode_supports_recent_blockhash(mode);
    let supports_transaction_log = provider_stream_mode_supports_transaction_log(mode);
    let supports_transaction_status = provider_stream_mode_supports_transaction_status(mode);
    let supports_transaction_view_batch =
        provider_stream_mode_supports_transaction_view_batch(mode);
    let supports_slot_status = provider_stream_mode_supports_slot_status(mode);
    let supports_control_plane = provider_stream_mode_supports_control_plane(mode);
    let mut unsupported = Vec::new();
    if plugin_host.wants_raw_packet() {
        unsupported.push("on_raw_packet");
    }
    if plugin_host.wants_shred() {
        unsupported.push("on_shred");
    }
    if plugin_host.wants_dataset() {
        unsupported.push("on_dataset");
    }
    if plugin_host.wants_account_touch() {
        unsupported.push("on_account_touch");
    }
    if plugin_host.wants_account_update() && !supports_account_update {
        unsupported.push("on_account_update");
    }
    if plugin_host.wants_block_meta() && !supports_block_meta {
        unsupported.push("on_block_meta");
    }
    if plugin_host.wants_transaction_batch() {
        unsupported.push("on_transaction_batch");
    }
    if plugin_host.wants_recent_blockhash() && !supports_recent_blockhash {
        unsupported.push("on_recent_blockhash");
    }
    if plugin_host.wants_transaction_log() && !supports_transaction_log {
        unsupported.push("on_transaction_log");
    }
    if plugin_host.wants_transaction_status() && !supports_transaction_status {
        unsupported.push("on_transaction_status");
    }
    if derived_state_host.wants_transaction_status_observed() && !supports_transaction_status {
        unsupported.push("derived_state.transaction_status_observed");
    }
    if plugin_host.wants_transaction_view_batch() && !supports_transaction_view_batch {
        unsupported.push("on_transaction_view_batch");
    }
    if derived_state_host.wants_block_meta_observed() && !supports_block_meta {
        unsupported.push("derived_state.block_meta_observed");
    }
    if plugin_host.wants_slot_status() && !supports_slot_status {
        unsupported.push("on_slot_status");
    }
    if plugin_host.wants_cluster_topology() && !supports_control_plane {
        unsupported.push("on_cluster_topology");
    }
    if plugin_host.wants_leader_schedule() && !supports_control_plane {
        unsupported.push("on_leader_schedule");
    }
    if plugin_host.wants_reorg() && !supports_control_plane {
        unsupported.push("on_reorg");
    }
    if derived_state_host.wants_account_touch_observed() {
        unsupported.push("derived_state.account_touch_observed");
    }
    if !supports_control_plane && derived_state_host.wants_control_plane_observed() {
        unsupported.push("derived_state.control_plane_observed");
    }
    unsupported
}

const fn provider_stream_mode_supports_account_update(mode: ProviderStreamMode) -> bool {
    match mode {
        ProviderStreamMode::Generic
        | ProviderStreamMode::YellowstoneGrpcAccounts
        | ProviderStreamMode::LaserStreamAccounts => true,
        #[cfg(feature = "provider-websocket")]
        ProviderStreamMode::WebsocketAccount | ProviderStreamMode::WebsocketProgram => true,
        _ => false,
    }
}

const fn provider_stream_mode_supports_block_meta(mode: ProviderStreamMode) -> bool {
    matches!(
        mode,
        ProviderStreamMode::Generic
            | ProviderStreamMode::YellowstoneGrpcBlockMeta
            | ProviderStreamMode::LaserStreamBlockMeta
    )
}

const fn provider_stream_mode_supports_recent_blockhash(mode: ProviderStreamMode) -> bool {
    match mode {
        ProviderStreamMode::Generic
        | ProviderStreamMode::YellowstoneGrpc
        | ProviderStreamMode::LaserStream => true,
        #[cfg(feature = "provider-websocket")]
        ProviderStreamMode::WebsocketTransaction => true,
        _ => false,
    }
}

const fn provider_stream_mode_supports_transaction_log(mode: ProviderStreamMode) -> bool {
    match mode {
        ProviderStreamMode::Generic => true,
        #[cfg(feature = "provider-websocket")]
        ProviderStreamMode::WebsocketLogs => true,
        _ => false,
    }
}

const fn provider_stream_mode_supports_transaction_status(mode: ProviderStreamMode) -> bool {
    matches!(
        mode,
        ProviderStreamMode::Generic
            | ProviderStreamMode::YellowstoneGrpcTransactionStatus
            | ProviderStreamMode::LaserStreamTransactionStatus
    )
}

const fn provider_stream_mode_supports_transaction_view_batch(mode: ProviderStreamMode) -> bool {
    matches!(mode, ProviderStreamMode::Generic)
}

const fn provider_stream_mode_supports_slot_status(mode: ProviderStreamMode) -> bool {
    matches!(
        mode,
        ProviderStreamMode::Generic
            | ProviderStreamMode::YellowstoneGrpcSlots
            | ProviderStreamMode::LaserStreamSlots
    )
}

const fn provider_stream_mode_supports_control_plane(mode: ProviderStreamMode) -> bool {
    matches!(mode, ProviderStreamMode::Generic)
}

fn enforce_provider_stream_capability_policy(
    mode: ProviderStreamMode,
    plugin_host: &PluginHost,
    derived_state_host: &DerivedStateHost,
) -> Result<ProviderStreamCapabilityCheck, RuntimeError> {
    let unsupported = provider_stream_unsupported_hooks(mode, plugin_host, derived_state_host);
    if unsupported.is_empty() {
        return Ok(ProviderStreamCapabilityCheck::Supported);
    }
    if mode != ProviderStreamMode::Generic {
        return Err(ProviderStreamRuntimeError::BuiltInUnsupportedHooks {
            mode,
            unsupported_hooks: unsupported.into_iter().map(String::from).collect(),
        }
        .into());
    }
    match ProviderStreamCapabilityPolicy::from_runtime_env() {
        ProviderStreamCapabilityPolicy::Warn => {
            let unsupported_hooks = unsupported
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>();
            tracing::warn!(
                mode = mode.as_str(),
                unsupported_hooks = unsupported_hooks.join(","),
                "provider-stream mode will not emit some requested plugin hooks"
            );
            Ok(ProviderStreamCapabilityCheck::Warn { unsupported_hooks })
        }
        ProviderStreamCapabilityPolicy::Strict => {
            Err(ProviderStreamRuntimeError::UnsupportedHooks {
                mode,
                unsupported_hooks: unsupported.into_iter().map(String::from).collect(),
            }
            .into())
        }
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
    use std::{
        collections::BTreeMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Instant,
    };

    use super::*;
    use crate::framework::{
        DerivedStateCheckpoint, DerivedStateConsumer, DerivedStateConsumerConfig,
        DerivedStateConsumerContext, DerivedStateConsumerFault, DerivedStateFeedEnvelope,
        DerivedStateFeedEvent, ExtensionContext, ExtensionManifest, ObserverPlugin, PluginConfig,
        PluginContext, RawPacketEvent, RuntimeExtension, TransactionInterest, TransactionPrefilter,
    };
    use async_trait::async_trait;
    use sof_gossip_tuning::{GossipTuningProfile, HostProfilePreset, IngestQueueMode};
    use solana_keypair::Keypair;
    use solana_message::{Message, VersionedMessage};
    use solana_signer::Signer;
    use solana_transaction::versioned::VersionedTransaction;

    fn profile_iterations(default: usize) -> usize {
        std::env::var("SOF_PROFILE_ITERATIONS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(default)
    }

    fn sample_provider_transaction_update() -> ProviderStreamUpdate {
        let signer = Keypair::new();
        let message = Message::new(&[], Some(&signer.pubkey()));
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[&signer])
            .expect("tx");
        ProviderStreamUpdate::Transaction(crate::framework::TransactionEvent {
            slot: 1,
            commitment_status: crate::event::TxCommitmentStatus::Processed,
            confirmed_slot: None,
            finalized_slot: None,
            signature: tx
                .signatures
                .first()
                .copied()
                .map(crate::framework::SignatureBytes::from_solana),
            provider_source: None,
            kind: crate::event::TxKind::NonVote,
            tx: Arc::new(tx),
        })
    }

    struct RawPacketPlugin;

    struct PrefilterIgnorePlugin {
        filter: TransactionPrefilter,
    }

    struct TransactionOnlyPlugin;
    struct RecentBlockhashPlugin;
    struct StartupCounterPlugin {
        counter: Arc<AtomicUsize>,
    }
    struct StartupCounterExtension {
        counter: Arc<AtomicUsize>,
    }
    struct AccountTouchDerivedStateConsumer;
    struct TransactionStatusDerivedStateConsumer;
    struct BlockMetaDerivedStateConsumer;
    struct ControlPlaneDerivedStateConsumer;
    struct SofTxAdapterLikePlugin;
    struct SofTxDerivedStateAdapterLikeConsumer;
    struct RecordingDerivedStateConsumer {
        events: Arc<Mutex<Vec<DerivedStateFeedEvent>>>,
        config: DerivedStateConsumerConfig,
    }

    #[async_trait]
    impl ObserverPlugin for RawPacketPlugin {
        fn config(&self) -> PluginConfig {
            PluginConfig::new().with_raw_packet()
        }

        async fn on_raw_packet(&self, _event: RawPacketEvent) {}
    }

    #[async_trait]
    impl ObserverPlugin for PrefilterIgnorePlugin {
        fn config(&self) -> PluginConfig {
            PluginConfig::new().with_transaction()
        }

        fn transaction_prefilter(&self) -> Option<&TransactionPrefilter> {
            Some(&self.filter)
        }
    }

    #[async_trait]
    impl ObserverPlugin for TransactionOnlyPlugin {
        fn config(&self) -> PluginConfig {
            PluginConfig::new().with_transaction()
        }
    }

    #[async_trait]
    impl ObserverPlugin for RecentBlockhashPlugin {
        fn config(&self) -> PluginConfig {
            PluginConfig::new().with_recent_blockhash()
        }
    }

    #[async_trait]
    impl ObserverPlugin for StartupCounterPlugin {
        fn config(&self) -> PluginConfig {
            PluginConfig::new().with_raw_packet()
        }

        async fn setup(
            &self,
            _ctx: PluginContext,
        ) -> Result<(), crate::framework::PluginSetupError> {
            self.counter.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn on_raw_packet(&self, _event: RawPacketEvent) {}
    }

    #[async_trait]
    impl RuntimeExtension for StartupCounterExtension {
        async fn setup(
            &self,
            _ctx: ExtensionContext,
        ) -> Result<ExtensionManifest, crate::framework::extension::ExtensionSetupError> {
            self.counter.fetch_add(1, Ordering::Relaxed);
            Ok(ExtensionManifest::default())
        }
    }

    impl DerivedStateConsumer for AccountTouchDerivedStateConsumer {
        fn name(&self) -> &'static str {
            "account-touch-derived-state"
        }

        fn state_version(&self) -> u32 {
            1
        }

        fn extension_version(&self) -> &'static str {
            "1"
        }

        fn load_checkpoint(
            &mut self,
        ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
            Ok(None)
        }

        fn config(&self) -> DerivedStateConsumerConfig {
            DerivedStateConsumerConfig::new().with_account_touch_observed()
        }

        fn setup(
            &mut self,
            _ctx: DerivedStateConsumerContext,
        ) -> Result<(), crate::framework::DerivedStateConsumerSetupError> {
            Ok(())
        }

        fn apply(
            &mut self,
            _envelope: &DerivedStateFeedEnvelope,
        ) -> Result<(), DerivedStateConsumerFault> {
            Ok(())
        }

        fn flush_checkpoint(
            &mut self,
            checkpoint: DerivedStateCheckpoint,
        ) -> Result<(), DerivedStateConsumerFault> {
            let _ = checkpoint;
            Ok(())
        }
    }

    impl DerivedStateConsumer for ControlPlaneDerivedStateConsumer {
        fn name(&self) -> &'static str {
            "control-plane-derived-state"
        }

        fn state_version(&self) -> u32 {
            1
        }

        fn extension_version(&self) -> &'static str {
            "1"
        }

        fn load_checkpoint(
            &mut self,
        ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
            Ok(None)
        }

        fn config(&self) -> DerivedStateConsumerConfig {
            DerivedStateConsumerConfig::new().with_control_plane_observed()
        }

        fn setup(
            &mut self,
            _ctx: DerivedStateConsumerContext,
        ) -> Result<(), crate::framework::DerivedStateConsumerSetupError> {
            Ok(())
        }

        fn apply(
            &mut self,
            _envelope: &DerivedStateFeedEnvelope,
        ) -> Result<(), DerivedStateConsumerFault> {
            Ok(())
        }

        fn flush_checkpoint(
            &mut self,
            checkpoint: DerivedStateCheckpoint,
        ) -> Result<(), DerivedStateConsumerFault> {
            let _ = checkpoint;
            Ok(())
        }
    }

    impl DerivedStateConsumer for TransactionStatusDerivedStateConsumer {
        fn name(&self) -> &'static str {
            "transaction-status-derived-state"
        }

        fn state_version(&self) -> u32 {
            1
        }

        fn extension_version(&self) -> &'static str {
            "1"
        }

        fn load_checkpoint(
            &mut self,
        ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
            Ok(None)
        }

        fn config(&self) -> DerivedStateConsumerConfig {
            DerivedStateConsumerConfig::new().with_transaction_status_observed()
        }

        fn apply(
            &mut self,
            _envelope: &DerivedStateFeedEnvelope,
        ) -> Result<(), DerivedStateConsumerFault> {
            Ok(())
        }

        fn flush_checkpoint(
            &mut self,
            _checkpoint: DerivedStateCheckpoint,
        ) -> Result<(), DerivedStateConsumerFault> {
            Ok(())
        }
    }

    impl DerivedStateConsumer for BlockMetaDerivedStateConsumer {
        fn name(&self) -> &'static str {
            "block-meta-derived-state"
        }

        fn state_version(&self) -> u32 {
            1
        }

        fn extension_version(&self) -> &'static str {
            "1"
        }

        fn load_checkpoint(
            &mut self,
        ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
            Ok(None)
        }

        fn config(&self) -> DerivedStateConsumerConfig {
            DerivedStateConsumerConfig::new().with_block_meta_observed()
        }

        fn apply(
            &mut self,
            _envelope: &DerivedStateFeedEnvelope,
        ) -> Result<(), DerivedStateConsumerFault> {
            Ok(())
        }

        fn flush_checkpoint(
            &mut self,
            _checkpoint: DerivedStateCheckpoint,
        ) -> Result<(), DerivedStateConsumerFault> {
            Ok(())
        }
    }

    impl DerivedStateConsumer for RecordingDerivedStateConsumer {
        fn name(&self) -> &'static str {
            "recording-derived-state"
        }

        fn state_version(&self) -> u32 {
            1
        }

        fn extension_version(&self) -> &'static str {
            "1"
        }

        fn load_checkpoint(
            &mut self,
        ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
            Ok(None)
        }

        fn config(&self) -> DerivedStateConsumerConfig {
            self.config
        }

        fn apply(
            &mut self,
            envelope: &DerivedStateFeedEnvelope,
        ) -> Result<(), DerivedStateConsumerFault> {
            self.events
                .lock()
                .map_err(|_poison| {
                    DerivedStateConsumerFault::new(
                        crate::framework::DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                        Some(envelope.sequence),
                        "recording-derived-state mutex poisoned during apply",
                    )
                })?
                .push(envelope.event.clone());
            Ok(())
        }

        fn flush_checkpoint(
            &mut self,
            _checkpoint: DerivedStateCheckpoint,
        ) -> Result<(), DerivedStateConsumerFault> {
            Ok(())
        }
    }

    #[async_trait]
    impl ObserverPlugin for SofTxAdapterLikePlugin {
        fn name(&self) -> &'static str {
            "sof-tx-provider-adapter"
        }

        fn config(&self) -> PluginConfig {
            PluginConfig::new()
                .with_recent_blockhash()
                .with_cluster_topology()
                .with_leader_schedule()
        }
    }

    impl DerivedStateConsumer for SofTxDerivedStateAdapterLikeConsumer {
        fn name(&self) -> &'static str {
            "sof-tx-derived-state-provider-adapter"
        }

        fn state_version(&self) -> u32 {
            1
        }

        fn extension_version(&self) -> &'static str {
            "1"
        }

        fn load_checkpoint(
            &mut self,
        ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
            Ok(None)
        }

        fn config(&self) -> DerivedStateConsumerConfig {
            DerivedStateConsumerConfig::new().with_control_plane_observed()
        }

        fn apply(
            &mut self,
            _envelope: &DerivedStateFeedEnvelope,
        ) -> Result<(), DerivedStateConsumerFault> {
            Ok(())
        }

        fn flush_checkpoint(
            &mut self,
            _checkpoint: DerivedStateCheckpoint,
        ) -> Result<(), DerivedStateConsumerFault> {
            Ok(())
        }
    }

    fn dispatch_provider_stream_update_baseline(
        plugin_host: &PluginHost,
        derived_state_host: &DerivedStateHost,
        update: ProviderStreamUpdate,
    ) {
        match update {
            ProviderStreamUpdate::Transaction(event) => {
                plugin_host.on_recent_blockhash(crate::framework::ObservedRecentBlockhashEvent {
                    slot: event.slot,
                    recent_blockhash: event.tx.message.recent_blockhash().to_bytes(),
                    dataset_tx_count: 1,
                    provider_source: event.provider_source.clone(),
                });
                let tx_index = derived_state_host.reserve_slot_tx_indexes(event.slot, 1);
                derived_state_host.on_transaction(tx_index, event.clone());
                plugin_host.on_transaction(event);
            }
            ProviderStreamUpdate::SerializedTransaction(_) => {}
            ProviderStreamUpdate::TransactionLog(event) => {
                plugin_host.on_transaction_log(event);
            }
            ProviderStreamUpdate::TransactionStatus(event) => {
                plugin_host.on_transaction_status(event);
            }
            ProviderStreamUpdate::TransactionViewBatch(event) => {
                plugin_host.on_transaction_view_batch(event, Instant::now());
            }
            ProviderStreamUpdate::AccountUpdate(event) => {
                plugin_host.on_account_update(event);
            }
            ProviderStreamUpdate::BlockMeta(event) => {
                plugin_host.on_block_meta(event);
            }
            ProviderStreamUpdate::RecentBlockhash(event) => {
                derived_state_host.on_recent_blockhash(event.clone());
                plugin_host.on_recent_blockhash(event);
            }
            ProviderStreamUpdate::SlotStatus(event) => {
                derived_state_host.on_slot_status(event.clone());
                plugin_host.on_slot_status(event);
            }
            ProviderStreamUpdate::ClusterTopology(event) => {
                derived_state_host.on_cluster_topology(event.clone());
                plugin_host.on_cluster_topology(event);
            }
            ProviderStreamUpdate::LeaderSchedule(event) => {
                derived_state_host.on_leader_schedule(event.clone());
                plugin_host.on_leader_schedule(event);
            }
            ProviderStreamUpdate::Reorg(event) => {
                derived_state_host.on_reorg(event.clone());
                plugin_host.on_reorg(event);
            }
            ProviderStreamUpdate::Health(_) => {}
        }
    }

    fn dispatch_provider_stream_serialized_transaction_baseline(
        plugin_host: &PluginHost,
        derived_state_host: &DerivedStateHost,
        event: &crate::provider_stream::SerializedTransactionEvent,
    ) {
        let wants_transaction =
            plugin_host.transaction_enabled_at_commitment(event.commitment_status);
        let wants_recent_blockhash = plugin_host.wants_recent_blockhash();
        let wants_derived_state_transaction = derived_state_host.wants_transaction_applied();
        if !wants_transaction && !wants_recent_blockhash && !wants_derived_state_transaction {
            return;
        }

        let mut signature = event.signature;
        let mut recent_blockhash = None;
        let mut kind = None;
        if let Ok(view) = SanitizedTransactionView::try_new_sanitized(event.bytes.as_ref(), true) {
            if signature.is_none() {
                signature = view
                    .signatures()
                    .first()
                    .copied()
                    .map(crate::framework::SignatureBytes::from_solana);
            }
            kind = Some(crate::provider_stream::classify_provider_transaction_kind_view(&view));
            if wants_recent_blockhash {
                recent_blockhash = Some(view.recent_blockhash().to_bytes());
            }
            if wants_transaction {
                let prefiltered = plugin_host.classify_transaction_view_in_scope(
                    &view,
                    event.commitment_status,
                    TransactionDispatchScope::All,
                );
                if !prefiltered.needs_full_classification
                    && prefiltered.dispatch.is_empty()
                    && !wants_derived_state_transaction
                {
                    if let Some(recent_blockhash) = recent_blockhash {
                        plugin_host.on_recent_blockhash(
                            crate::framework::ObservedRecentBlockhashEvent {
                                slot: event.slot,
                                recent_blockhash,
                                dataset_tx_count: 1,
                                provider_source: event.provider_source.clone(),
                            },
                        );
                    }
                    return;
                }
            }
            if !wants_transaction && !wants_derived_state_transaction {
                if let Some(recent_blockhash) = recent_blockhash {
                    plugin_host.on_recent_blockhash(
                        crate::framework::ObservedRecentBlockhashEvent {
                            slot: event.slot,
                            recent_blockhash,
                            dataset_tx_count: 1,
                            provider_source: event.provider_source.clone(),
                        },
                    );
                }
                return;
            }
        }

        let Ok(tx) = bincode::deserialize::<VersionedTransaction>(event.bytes.as_ref()) else {
            return;
        };
        let tx = Arc::new(tx);
        let event = TransactionEvent {
            slot: event.slot,
            commitment_status: event.commitment_status,
            confirmed_slot: event.confirmed_slot,
            finalized_slot: event.finalized_slot,
            signature: signature.or_else(|| {
                tx.signatures
                    .first()
                    .copied()
                    .map(crate::framework::SignatureBytes::from_solana)
            }),
            provider_source: event.provider_source.clone(),
            kind: kind
                .unwrap_or_else(|| crate::provider_stream::classify_provider_transaction_kind(&tx)),
            tx,
        };
        if wants_recent_blockhash {
            plugin_host.on_recent_blockhash(crate::framework::ObservedRecentBlockhashEvent {
                slot: event.slot,
                recent_blockhash: recent_blockhash
                    .unwrap_or_else(|| event.tx.message.recent_blockhash().to_bytes()),
                dataset_tx_count: 1,
                provider_source: event.provider_source.clone(),
            });
        }
        if wants_derived_state_transaction {
            let tx_index = derived_state_host.reserve_slot_tx_indexes(event.slot, 1);
            derived_state_host.on_transaction(tx_index, event.clone());
        }
        if wants_transaction {
            plugin_host.on_transaction(event);
        }
    }

    fn sample_serialized_provider_transaction_update() -> ProviderStreamUpdate {
        match sample_provider_transaction_update() {
            ProviderStreamUpdate::Transaction(event) => {
                let bytes = bincode::serialize(event.tx.as_ref()).expect("serialize tx");
                ProviderStreamUpdate::SerializedTransaction(
                    crate::provider_stream::SerializedTransactionEvent {
                        slot: event.slot,
                        commitment_status: event.commitment_status,
                        confirmed_slot: event.confirmed_slot,
                        finalized_slot: event.finalized_slot,
                        signature: event.signature,
                        provider_source: event.provider_source,
                        bytes: bytes.into_boxed_slice(),
                    },
                )
            }
            other @ ProviderStreamUpdate::SerializedTransaction(_)
            | other @ ProviderStreamUpdate::TransactionLog(_)
            | other @ ProviderStreamUpdate::TransactionStatus(_)
            | other @ ProviderStreamUpdate::TransactionViewBatch(_)
            | other @ ProviderStreamUpdate::AccountUpdate(_)
            | other @ ProviderStreamUpdate::BlockMeta(_)
            | other @ ProviderStreamUpdate::RecentBlockhash(_)
            | other @ ProviderStreamUpdate::SlotStatus(_)
            | other @ ProviderStreamUpdate::ClusterTopology(_)
            | other @ ProviderStreamUpdate::LeaderSchedule(_)
            | other @ ProviderStreamUpdate::Reorg(_)
            | other @ ProviderStreamUpdate::Health(_) => other,
        }
    }

    fn sample_confirmed_provider_transaction_update() -> ProviderStreamUpdate {
        match sample_provider_transaction_update() {
            ProviderStreamUpdate::Transaction(mut event) => {
                event.commitment_status = crate::event::TxCommitmentStatus::Confirmed;
                event.confirmed_slot = Some(event.slot);
                ProviderStreamUpdate::Transaction(event)
            }
            other @ ProviderStreamUpdate::SerializedTransaction(_)
            | other @ ProviderStreamUpdate::TransactionLog(_)
            | other @ ProviderStreamUpdate::TransactionStatus(_)
            | other @ ProviderStreamUpdate::TransactionViewBatch(_)
            | other @ ProviderStreamUpdate::AccountUpdate(_)
            | other @ ProviderStreamUpdate::BlockMeta(_)
            | other @ ProviderStreamUpdate::RecentBlockhash(_)
            | other @ ProviderStreamUpdate::SlotStatus(_)
            | other @ ProviderStreamUpdate::ClusterTopology(_)
            | other @ ProviderStreamUpdate::LeaderSchedule(_)
            | other @ ProviderStreamUpdate::Reorg(_)
            | other @ ProviderStreamUpdate::Health(_) => other,
        }
    }

    fn sample_confirmed_serialized_provider_transaction_update() -> ProviderStreamUpdate {
        match sample_serialized_provider_transaction_update() {
            ProviderStreamUpdate::SerializedTransaction(mut event) => {
                event.commitment_status = crate::event::TxCommitmentStatus::Confirmed;
                event.confirmed_slot = Some(event.slot);
                ProviderStreamUpdate::SerializedTransaction(event)
            }
            other @ ProviderStreamUpdate::Transaction(_)
            | other @ ProviderStreamUpdate::TransactionLog(_)
            | other @ ProviderStreamUpdate::TransactionStatus(_)
            | other @ ProviderStreamUpdate::TransactionViewBatch(_)
            | other @ ProviderStreamUpdate::AccountUpdate(_)
            | other @ ProviderStreamUpdate::BlockMeta(_)
            | other @ ProviderStreamUpdate::RecentBlockhash(_)
            | other @ ProviderStreamUpdate::SlotStatus(_)
            | other @ ProviderStreamUpdate::ClusterTopology(_)
            | other @ ProviderStreamUpdate::LeaderSchedule(_)
            | other @ ProviderStreamUpdate::Reorg(_)
            | other @ ProviderStreamUpdate::Health(_) => other,
        }
    }

    fn sample_provider_transaction_update_at(slot: u64) -> ProviderStreamUpdate {
        match sample_provider_transaction_update() {
            ProviderStreamUpdate::Transaction(mut event) => {
                event.slot = slot;
                event.signature = Some(crate::framework::SignatureBytes::from_solana(
                    Signature::from([slot as u8; 64]),
                ));
                ProviderStreamUpdate::Transaction(event)
            }
            other @ ProviderStreamUpdate::SerializedTransaction(_)
            | other @ ProviderStreamUpdate::TransactionLog(_)
            | other @ ProviderStreamUpdate::TransactionStatus(_)
            | other @ ProviderStreamUpdate::TransactionViewBatch(_)
            | other @ ProviderStreamUpdate::AccountUpdate(_)
            | other @ ProviderStreamUpdate::BlockMeta(_)
            | other @ ProviderStreamUpdate::RecentBlockhash(_)
            | other @ ProviderStreamUpdate::SlotStatus(_)
            | other @ ProviderStreamUpdate::ClusterTopology(_)
            | other @ ProviderStreamUpdate::LeaderSchedule(_)
            | other @ ProviderStreamUpdate::Reorg(_)
            | other @ ProviderStreamUpdate::Health(_) => other,
        }
    }

    fn sample_provider_recent_blockhash_update(slot: u64) -> ProviderStreamUpdate {
        ProviderStreamUpdate::RecentBlockhash(crate::framework::ObservedRecentBlockhashEvent {
            slot,
            recent_blockhash: [slot as u8; 32],
            dataset_tx_count: 1,
            provider_source: None,
        })
    }

    fn sample_provider_transaction_status_update(slot: u64) -> ProviderStreamUpdate {
        ProviderStreamUpdate::TransactionStatus(crate::framework::TransactionStatusEvent {
            slot,
            commitment_status: crate::event::TxCommitmentStatus::Processed,
            confirmed_slot: None,
            finalized_slot: None,
            signature: crate::framework::SignatureBytes::from_solana(Signature::from(
                [slot as u8; 64],
            )),
            is_vote: false,
            index: Some(0),
            err: None,
            provider_source: None,
        })
    }

    fn sample_provider_block_meta_update(slot: u64) -> ProviderStreamUpdate {
        ProviderStreamUpdate::BlockMeta(crate::framework::BlockMetaEvent {
            slot,
            commitment_status: crate::event::TxCommitmentStatus::Processed,
            confirmed_slot: None,
            finalized_slot: None,
            blockhash: [slot as u8; 32],
            parent_slot: slot.saturating_sub(1),
            parent_blockhash: [slot.saturating_sub(1) as u8; 32],
            block_time: Some(1_700_000_000 + slot as i64),
            block_height: Some(slot),
            executed_transaction_count: 3,
            entries_count: 2,
            provider_source: None,
        })
    }

    fn sample_provider_transaction_log_update(slot: u64) -> ProviderStreamUpdate {
        ProviderStreamUpdate::TransactionLog(crate::framework::TransactionLogEvent {
            slot,
            commitment_status: crate::event::TxCommitmentStatus::Processed,
            signature: crate::framework::SignatureBytes::from_solana(Signature::from(
                [slot as u8; 64],
            )),
            err: None,
            logs: Arc::from([String::from("program log: hello")]),
            matched_filter: None,
            provider_source: None,
        })
    }

    fn sample_provider_transaction_view_batch_update(slot: u64) -> ProviderStreamUpdate {
        let payload: Arc<[u8]> = Arc::from([1_u8, 2, 3, 4]);
        let transactions: Arc<[crate::framework::SerializedTransactionRange]> =
            Arc::from([crate::framework::SerializedTransactionRange::new(0, 4)]);
        ProviderStreamUpdate::TransactionViewBatch(crate::framework::TransactionViewBatchEvent {
            slot,
            start_index: 0,
            end_index: 0,
            last_in_slot: false,
            shreds: 1,
            payload_len: payload.len(),
            commitment_status: crate::event::TxCommitmentStatus::Processed,
            confirmed_slot: None,
            finalized_slot: None,
            provider_source: None,
            payload,
            transactions,
        })
    }

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
    fn gossip_load_shed_tuning_sets_expected_env_overrides() {
        let setup = RuntimeSetup::new()
            .with_gossip_load_shed_enabled(true)
            .with_gossip_load_shed_queue_pressure_pct(65)
            .with_verify_strict_unknown_queue_pressure_pct(12)
            .with_gossip_load_shed_keep_top_sources(3)
            .with_gossip_socket_consume_verify_queue_capacity(1024);
        let overrides = setup.env_overrides.into_iter().collect::<BTreeMap<_, _>>();

        assert_eq!(
            overrides.get("SOF_GOSSIP_LOAD_SHED_ENABLED"),
            Some(&"true".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_GOSSIP_LOAD_SHED_QUEUE_PRESSURE_PCT"),
            Some(&"65".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_VERIFY_STRICT_UNKNOWN_QUEUE_PRESSURE_PCT"),
            Some(&"12".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_GOSSIP_LOAD_SHED_KEEP_TOP_SOURCES"),
            Some(&"3".to_owned())
        );
        assert_eq!(
            overrides.get("SOF_GOSSIP_SOCKET_CONSUME_VERIFY_QUEUE_CAPACITY"),
            Some(&"1024".to_owned())
        );
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

    #[test]
    fn typed_shred_trust_mode_uses_expected_strings() {
        let setup =
            RuntimeSetup::new().with_shred_trust_mode(ShredTrustMode::TrustedRawShredProvider);
        assert_eq!(
            setup.env_overrides.last(),
            Some(&(
                String::from("SOF_SHRED_TRUST_MODE"),
                String::from("trusted_raw_shred_provider"),
            ))
        );
    }

    #[test]
    fn typed_provider_stream_capability_policy_uses_expected_strings() {
        let setup = RuntimeSetup::new()
            .with_provider_stream_capability_policy(ProviderStreamCapabilityPolicy::Strict);
        assert_eq!(
            setup.env_overrides.last(),
            Some(&(
                String::from("SOF_PROVIDER_STREAM_CAPABILITY_POLICY"),
                String::from("strict"),
            ))
        );
    }

    #[test]
    fn typed_provider_stream_allow_eof_uses_expected_strings() {
        let setup = RuntimeSetup::new().with_provider_stream_allow_eof(true);
        assert_eq!(
            setup.env_overrides.last(),
            Some(&(
                String::from("SOF_PROVIDER_STREAM_ALLOW_EOF"),
                String::from("true"),
            ))
        );
    }

    #[test]
    fn provider_stream_warn_policy_allows_unsupported_hooks() {
        crate::runtime_env::set_runtime_env_overrides([(
            String::from("SOF_PROVIDER_STREAM_CAPABILITY_POLICY"),
            String::from("warn"),
        )]);
        let plugin_host = PluginHost::builder().add_plugin(RawPacketPlugin).build();
        let derived_state_host = DerivedStateHost::builder().build();
        let result = enforce_provider_stream_capability_policy(
            ProviderStreamMode::Generic,
            &plugin_host,
            &derived_state_host,
        );
        crate::runtime_env::clear_runtime_env_overrides();
        assert!(result.is_ok());
    }

    #[test]
    fn provider_stream_strict_policy_rejects_unsupported_hooks() {
        crate::runtime_env::set_runtime_env_overrides([(
            String::from("SOF_PROVIDER_STREAM_CAPABILITY_POLICY"),
            String::from("strict"),
        )]);
        let plugin_host = PluginHost::builder().add_plugin(RawPacketPlugin).build();
        let derived_state_host = DerivedStateHost::builder().build();
        let result = enforce_provider_stream_capability_policy(
            ProviderStreamMode::YellowstoneGrpc,
            &plugin_host,
            &derived_state_host,
        );
        crate::runtime_env::clear_runtime_env_overrides();
        match result {
            Err(RuntimeError::ProviderStream(
                ProviderStreamRuntimeError::BuiltInUnsupportedHooks {
                    mode,
                    unsupported_hooks,
                },
            )) => {
                assert_eq!(mode, ProviderStreamMode::YellowstoneGrpc);
                assert!(unsupported_hooks.contains(&String::from("on_raw_packet")));
            }
            other => panic!("expected strict provider capability failure, got {other:?}"),
        }
    }

    #[test]
    fn provider_stream_strict_policy_allows_supported_provider_updates() {
        crate::runtime_env::set_runtime_env_overrides([(
            String::from("SOF_PROVIDER_STREAM_CAPABILITY_POLICY"),
            String::from("strict"),
        )]);
        let plugin_host = PluginHost::builder()
            .add_plugin(TransactionOnlyPlugin)
            .build();
        let derived_state_host = DerivedStateHost::builder().build();
        let result = enforce_provider_stream_capability_policy(
            ProviderStreamMode::YellowstoneGrpc,
            &plugin_host,
            &derived_state_host,
        );
        crate::runtime_env::clear_runtime_env_overrides();
        assert!(result.is_ok());
    }

    #[test]
    fn built_in_non_transaction_provider_modes_reject_recent_blockhash_even_under_warn() {
        crate::runtime_env::set_runtime_env_overrides([(
            String::from("SOF_PROVIDER_STREAM_CAPABILITY_POLICY"),
            String::from("warn"),
        )]);
        let plugin_host = PluginHost::builder()
            .add_plugin(RecentBlockhashPlugin)
            .build();
        let derived_state_host = DerivedStateHost::builder().build();
        let result = enforce_provider_stream_capability_policy(
            ProviderStreamMode::YellowstoneGrpcAccounts,
            &plugin_host,
            &derived_state_host,
        );
        crate::runtime_env::clear_runtime_env_overrides();
        match result {
            Err(RuntimeError::ProviderStream(
                ProviderStreamRuntimeError::BuiltInUnsupportedHooks {
                    unsupported_hooks, ..
                },
            )) => {
                assert!(unsupported_hooks.contains(&String::from("on_recent_blockhash")));
            }
            other => panic!("expected built-in recent-blockhash capability failure, got {other:?}"),
        }
    }

    #[test]
    fn provider_stream_strict_policy_rejects_unsupported_derived_state_subscriptions() {
        crate::runtime_env::set_runtime_env_overrides([(
            String::from("SOF_PROVIDER_STREAM_CAPABILITY_POLICY"),
            String::from("strict"),
        )]);
        let plugin_host = PluginHost::builder().build();
        let derived_state_host = DerivedStateHost::builder()
            .add_consumer(AccountTouchDerivedStateConsumer)
            .add_consumer(ControlPlaneDerivedStateConsumer)
            .build();
        let result = enforce_provider_stream_capability_policy(
            ProviderStreamMode::YellowstoneGrpc,
            &plugin_host,
            &derived_state_host,
        );
        crate::runtime_env::clear_runtime_env_overrides();
        match result {
            Err(RuntimeError::ProviderStream(
                ProviderStreamRuntimeError::BuiltInUnsupportedHooks {
                    unsupported_hooks, ..
                },
            )) => {
                assert!(
                    unsupported_hooks
                        .contains(&String::from("derived_state.account_touch_observed"))
                );
                assert!(
                    unsupported_hooks
                        .contains(&String::from("derived_state.control_plane_observed"))
                );
            }
            other => panic!("expected strict provider capability failure, got {other:?}"),
        }
    }

    #[test]
    fn built_in_provider_modes_reject_control_plane_hooks_even_under_warn() {
        crate::runtime_env::set_runtime_env_overrides([(
            String::from("SOF_PROVIDER_STREAM_CAPABILITY_POLICY"),
            String::from("warn"),
        )]);
        struct ControlPlanePlugin;

        #[async_trait]
        impl ObserverPlugin for ControlPlanePlugin {
            fn name(&self) -> &'static str {
                "control-plane-plugin"
            }

            fn config(&self) -> PluginConfig {
                PluginConfig::new()
                    .with_cluster_topology()
                    .with_leader_schedule()
                    .with_reorg()
            }
        }

        let plugin_host = PluginHost::builder().add_plugin(ControlPlanePlugin).build();
        let derived_state_host = DerivedStateHost::builder()
            .add_consumer(ControlPlaneDerivedStateConsumer)
            .build();
        let result = enforce_provider_stream_capability_policy(
            ProviderStreamMode::YellowstoneGrpc,
            &plugin_host,
            &derived_state_host,
        );
        crate::runtime_env::clear_runtime_env_overrides();
        match result {
            Err(RuntimeError::ProviderStream(
                ProviderStreamRuntimeError::BuiltInUnsupportedHooks {
                    unsupported_hooks, ..
                },
            )) => {
                assert!(unsupported_hooks.contains(&String::from("on_cluster_topology")));
                assert!(unsupported_hooks.contains(&String::from("on_leader_schedule")));
                assert!(unsupported_hooks.contains(&String::from("on_reorg")));
                assert!(
                    unsupported_hooks
                        .contains(&String::from("derived_state.control_plane_observed"))
                );
            }
            other => panic!("expected built-in provider capability failure, got {other:?}"),
        }
    }

    #[test]
    fn provider_stream_strict_policy_allows_generic_leader_schedule_reorg_and_control_plane() {
        crate::runtime_env::set_runtime_env_overrides([(
            String::from("SOF_PROVIDER_STREAM_CAPABILITY_POLICY"),
            String::from("strict"),
        )]);
        struct LeaderScheduleReorgPlugin;

        #[async_trait]
        impl ObserverPlugin for LeaderScheduleReorgPlugin {
            fn name(&self) -> &'static str {
                "leader-schedule-reorg-plugin"
            }

            fn config(&self) -> PluginConfig {
                PluginConfig::new().with_leader_schedule().with_reorg()
            }
        }

        let plugin_host = PluginHost::builder()
            .add_plugin(LeaderScheduleReorgPlugin)
            .build();
        let derived_state_host = DerivedStateHost::builder()
            .add_consumer(ControlPlaneDerivedStateConsumer)
            .build();
        let result = enforce_provider_stream_capability_policy(
            ProviderStreamMode::Generic,
            &plugin_host,
            &derived_state_host,
        );
        crate::runtime_env::clear_runtime_env_overrides();
        assert!(result.is_ok());
    }

    #[test]
    fn provider_stream_rejects_sof_tx_live_adapter_even_under_warn() {
        crate::runtime_env::set_runtime_env_overrides([(
            String::from("SOF_PROVIDER_STREAM_CAPABILITY_POLICY"),
            String::from("warn"),
        )]);
        let plugin_host = PluginHost::builder()
            .add_plugin(SofTxAdapterLikePlugin)
            .build();
        let derived_state_host = DerivedStateHost::builder().build();
        let result = enforce_provider_stream_capability_policy(
            ProviderStreamMode::YellowstoneGrpc,
            &plugin_host,
            &derived_state_host,
        );
        crate::runtime_env::clear_runtime_env_overrides();
        match result {
            Err(RuntimeError::ProviderStream(
                ProviderStreamRuntimeError::BuiltInUnsupportedHooks {
                    unsupported_hooks, ..
                },
            )) => {
                assert!(unsupported_hooks.contains(&String::from("on_cluster_topology")));
                assert!(unsupported_hooks.contains(&String::from("on_leader_schedule")));
            }
            other => panic!("expected sof-tx live adapter failure, got {other:?}"),
        }
    }

    #[test]
    fn provider_stream_rejects_sof_tx_replay_adapter_even_under_warn() {
        crate::runtime_env::set_runtime_env_overrides([(
            String::from("SOF_PROVIDER_STREAM_CAPABILITY_POLICY"),
            String::from("warn"),
        )]);
        let plugin_host = PluginHost::builder().build();
        let derived_state_host = DerivedStateHost::builder()
            .add_consumer(SofTxDerivedStateAdapterLikeConsumer)
            .build();
        let result = enforce_provider_stream_capability_policy(
            ProviderStreamMode::YellowstoneGrpc,
            &plugin_host,
            &derived_state_host,
        );
        crate::runtime_env::clear_runtime_env_overrides();
        match result {
            Err(RuntimeError::ProviderStream(
                ProviderStreamRuntimeError::BuiltInUnsupportedHooks {
                    unsupported_hooks, ..
                },
            )) => {
                assert!(
                    unsupported_hooks
                        .contains(&String::from("derived_state.control_plane_observed"))
                );
            }
            other => panic!("expected sof-tx replay adapter failure, got {other:?}"),
        }
    }

    #[test]
    fn provider_stream_allows_sof_tx_live_adapter_on_generic_mode() {
        let plugin_host = PluginHost::builder()
            .add_plugin(SofTxAdapterLikePlugin)
            .build();
        let derived_state_host = DerivedStateHost::builder().build();
        let result = enforce_provider_stream_capability_policy(
            ProviderStreamMode::Generic,
            &plugin_host,
            &derived_state_host,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn provider_stream_allows_sof_tx_replay_adapter_on_generic_mode() {
        let plugin_host = PluginHost::builder().build();
        let derived_state_host = DerivedStateHost::builder()
            .add_consumer(SofTxDerivedStateAdapterLikeConsumer)
            .build();
        let result = enforce_provider_stream_capability_policy(
            ProviderStreamMode::Generic,
            &plugin_host,
            &derived_state_host,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn provider_replay_dedupe_skips_duplicate_transaction_updates() {
        let update = sample_provider_transaction_update();
        let mut dedupe = ProviderReplayDedupe::new(8);

        assert!(!dedupe.observe(&update));
        assert!(dedupe.observe(&update));
    }

    #[test]
    fn provider_replay_dedupe_keeps_same_transaction_from_distinct_sources() {
        let source_a = crate::provider_stream::ProviderSourceIdentity::new(
            crate::provider_stream::ProviderSourceId::Generic("source-a".to_owned().into()),
            "ws-tx-1",
        );
        let source_b = crate::provider_stream::ProviderSourceIdentity::new(
            crate::provider_stream::ProviderSourceId::Generic("source-b".to_owned().into()),
            "ws-tx-2",
        );
        let update = sample_provider_transaction_update();
        let update_a = update.clone().with_provider_source(source_a);
        let update_b = update.with_provider_source(source_b);
        let mut dedupe = ProviderReplayDedupe::new(8);

        assert!(!dedupe.observe(&update_a));
        assert!(!dedupe.observe(&update_b));
    }

    #[test]
    fn provider_replay_dedupe_first_seen_suppresses_later_overlapping_source() {
        let source_a = crate::provider_stream::ProviderSourceIdentity::new(
            crate::provider_stream::ProviderSourceId::Generic("source-a".to_owned().into()),
            "primary",
        )
        .with_arbitration(crate::provider_stream::ProviderSourceArbitrationMode::FirstSeen)
        .with_priority(100);
        let source_b = crate::provider_stream::ProviderSourceIdentity::new(
            crate::provider_stream::ProviderSourceId::Generic("source-b".to_owned().into()),
            "secondary",
        )
        .with_arbitration(crate::provider_stream::ProviderSourceArbitrationMode::FirstSeen)
        .with_priority(300);
        let update = sample_provider_transaction_update();
        let update_a = update.clone().with_provider_source(source_a);
        let update_b = update.with_provider_source(source_b);
        let mut dedupe = ProviderReplayDedupe::new(8);

        assert!(!dedupe.observe(&update_a));
        assert!(dedupe.observe(&update_b));
    }

    #[test]
    fn provider_replay_dedupe_promotes_higher_priority_source() {
        let source_a = crate::provider_stream::ProviderSourceIdentity::new(
            crate::provider_stream::ProviderSourceId::Generic("source-a".to_owned().into()),
            "fallback",
        )
        .with_arbitration(
            crate::provider_stream::ProviderSourceArbitrationMode::FirstSeenThenPromote,
        )
        .with_priority(100);
        let source_b = crate::provider_stream::ProviderSourceIdentity::new(
            crate::provider_stream::ProviderSourceId::Generic("source-b".to_owned().into()),
            "primary",
        )
        .with_arbitration(
            crate::provider_stream::ProviderSourceArbitrationMode::FirstSeenThenPromote,
        )
        .with_priority(300);
        let update = sample_provider_transaction_update();
        let update_a = update.clone().with_provider_source(source_a);
        let update_b = update.with_provider_source(source_b);
        let mut dedupe = ProviderReplayDedupe::new(8);

        assert!(!dedupe.observe(&update_a));
        assert!(!dedupe.observe(&update_b));
        assert!(dedupe.observe(&update_a));
        assert!(dedupe.observe(&update_b));
    }

    #[test]
    fn provider_replay_dedupe_skips_duplicate_serialized_transaction_updates() {
        let update = sample_serialized_provider_transaction_update();
        let mut dedupe = ProviderReplayDedupe::new(8);

        assert!(!dedupe.observe(&update));
        assert!(dedupe.observe(&update));
    }

    #[test]
    fn provider_replay_dedupe_skips_duplicate_serialized_updates_without_signature() {
        let ProviderStreamUpdate::SerializedTransaction(mut event) =
            sample_serialized_provider_transaction_update()
        else {
            panic!("expected serialized update fixture");
        };
        event.signature = None;
        let update = ProviderStreamUpdate::SerializedTransaction(event);
        let mut dedupe = ProviderReplayDedupe::new(8);

        assert!(!dedupe.observe(&update));
        assert!(dedupe.observe(&update));
    }

    #[test]
    fn provider_replay_dedupe_keeps_higher_commitment_transaction_update() {
        let initial = sample_provider_transaction_update();
        let progressed = sample_confirmed_provider_transaction_update();
        let mut dedupe = ProviderReplayDedupe::new(8);

        assert!(!dedupe.observe(&initial));
        assert!(!dedupe.observe(&progressed));
    }

    #[test]
    fn provider_replay_dedupe_keeps_higher_commitment_serialized_update() {
        let initial = sample_serialized_provider_transaction_update();
        let progressed = sample_confirmed_serialized_provider_transaction_update();
        let mut dedupe = ProviderReplayDedupe::new(8);

        assert!(!dedupe.observe(&initial));
        assert!(!dedupe.observe(&progressed));
    }

    #[test]
    fn provider_replay_dedupe_evicts_slots_outside_window() {
        let old = sample_provider_transaction_update_at(1);
        let new = sample_provider_transaction_update_at(PROVIDER_REPLAY_DEDUPE_SLOT_WINDOW + 2);
        let mut dedupe = ProviderReplayDedupe::new(8);

        assert!(!dedupe.observe(&old));
        assert!(!dedupe.observe(&new));
        assert!(!dedupe.observe(&old));
    }

    #[test]
    fn provider_replay_dedupe_skips_duplicate_control_plane_updates() {
        let update = sample_provider_recent_blockhash_update(42);
        let mut dedupe = ProviderReplayDedupe::new(8);

        assert!(!dedupe.observe(&update));
        assert!(dedupe.observe(&update));
    }

    #[test]
    fn provider_replay_dedupe_keeps_same_control_plane_update_from_distinct_sources() {
        let source_a = crate::provider_stream::ProviderSourceIdentity::new(
            crate::provider_stream::ProviderSourceId::YellowstoneGrpcSlots,
            "yellowstone-slot-1",
        );
        let source_b = crate::provider_stream::ProviderSourceIdentity::new(
            crate::provider_stream::ProviderSourceId::YellowstoneGrpcSlots,
            "yellowstone-slot-2",
        );
        let update = sample_provider_recent_blockhash_update(42);
        let update_a = update.clone().with_provider_source(source_a);
        let update_b = update.with_provider_source(source_b);
        let mut dedupe = ProviderReplayDedupe::new(8);

        assert!(!dedupe.observe(&update_a));
        assert!(!dedupe.observe(&update_b));
    }

    #[test]
    fn provider_replay_dedupe_skips_duplicate_transaction_log_updates() {
        let update = sample_provider_transaction_log_update(42);
        let mut dedupe = ProviderReplayDedupe::new(8);

        assert!(!dedupe.observe(&update));
        assert!(dedupe.observe(&update));
    }

    #[test]
    fn provider_replay_dedupe_skips_duplicate_transaction_view_batch_updates() {
        let update = sample_provider_transaction_view_batch_update(42);
        let mut dedupe = ProviderReplayDedupe::new(8);

        assert!(!dedupe.observe(&update));
        assert!(dedupe.observe(&update));
    }

    #[test]
    fn provider_replay_dedupe_keeps_same_transaction_view_batch_from_distinct_sources() {
        let source_a = crate::provider_stream::ProviderSourceIdentity::new(
            crate::provider_stream::ProviderSourceId::YellowstoneGrpc,
            "yellowstone-view-batch-1",
        );
        let source_b = crate::provider_stream::ProviderSourceIdentity::new(
            crate::provider_stream::ProviderSourceId::LaserStream,
            "laserstream-view-batch-1",
        );
        let update = sample_provider_transaction_view_batch_update(42);
        let update_a = update.clone().with_provider_source(source_a);
        let update_b = update.with_provider_source(source_b);
        let mut dedupe = ProviderReplayDedupe::new(8);

        assert!(!dedupe.observe(&update_a));
        assert!(!dedupe.observe(&update_b));
    }

    #[test]
    fn built_in_provider_mode_rejects_mismatched_source_kind() {
        let update = sample_provider_transaction_update().with_provider_source(
            crate::provider_stream::ProviderSourceIdentity::new(
                crate::provider_stream::ProviderSourceId::LaserStream,
                "laserstream-1",
            ),
        );
        let error =
            validate_provider_stream_update_mode(ProviderStreamMode::YellowstoneGrpc, &update)
                .expect_err("expected mismatched built-in source kind");
        let ProviderStreamRuntimeError::MismatchedSourceKind {
            mode,
            source_kind,
            source_instance,
            ..
        } = error
        else {
            panic!("expected mismatched source-kind error");
        };
        assert_eq!(mode, ProviderStreamMode::YellowstoneGrpc);
        assert_eq!(source_kind, "laserstream");
        assert_eq!(source_instance, "laserstream-1");
    }

    #[test]
    fn built_in_provider_mode_rejects_unattributed_update() {
        let update = sample_provider_transaction_update();
        let error =
            validate_provider_stream_update_mode(ProviderStreamMode::YellowstoneGrpc, &update)
                .expect_err("expected unattributed built-in update failure");
        let ProviderStreamRuntimeError::UnattributedBuiltInUpdate { mode, update_kind } = error
        else {
            panic!("expected unattributed built-in update error");
        };
        assert_eq!(mode, ProviderStreamMode::YellowstoneGrpc);
        assert_eq!(update_kind, "transaction update");
    }

    #[test]
    fn provider_stream_strict_policy_allows_transaction_status_account_block_meta_and_slot_modes() {
        struct TransactionStatusPlugin;
        struct AccountUpdatePlugin;
        struct BlockMetaPlugin;
        struct SlotStatusPlugin;

        #[async_trait]
        impl ObserverPlugin for TransactionStatusPlugin {
            fn name(&self) -> &'static str {
                "transaction-status-plugin"
            }

            fn config(&self) -> PluginConfig {
                PluginConfig::new().with_transaction_status()
            }
        }

        #[async_trait]
        impl ObserverPlugin for AccountUpdatePlugin {
            fn name(&self) -> &'static str {
                "account-update-plugin"
            }

            fn config(&self) -> PluginConfig {
                PluginConfig::new().with_account_update()
            }
        }

        #[async_trait]
        impl ObserverPlugin for BlockMetaPlugin {
            fn name(&self) -> &'static str {
                "block-meta-plugin"
            }

            fn config(&self) -> PluginConfig {
                PluginConfig::new().with_block_meta()
            }
        }

        #[async_trait]
        impl ObserverPlugin for SlotStatusPlugin {
            fn name(&self) -> &'static str {
                "slot-status-plugin"
            }

            fn config(&self) -> PluginConfig {
                PluginConfig::new().with_slot_status()
            }
        }

        crate::runtime_env::set_runtime_env_overrides([(
            String::from("SOF_PROVIDER_STREAM_CAPABILITY_POLICY"),
            String::from("strict"),
        )]);

        let transaction_status_host = PluginHost::builder()
            .add_plugin(TransactionStatusPlugin)
            .build();
        let account_update_host = PluginHost::builder()
            .add_plugin(AccountUpdatePlugin)
            .build();
        let block_meta_host = PluginHost::builder().add_plugin(BlockMetaPlugin).build();
        let slot_status_host = PluginHost::builder().add_plugin(SlotStatusPlugin).build();
        let derived_state_host = DerivedStateHost::builder().build();

        assert!(
            enforce_provider_stream_capability_policy(
                ProviderStreamMode::YellowstoneGrpcTransactionStatus,
                &transaction_status_host,
                &derived_state_host,
            )
            .is_ok()
        );
        assert!(
            enforce_provider_stream_capability_policy(
                ProviderStreamMode::YellowstoneGrpcAccounts,
                &account_update_host,
                &derived_state_host,
            )
            .is_ok()
        );
        assert!(
            enforce_provider_stream_capability_policy(
                ProviderStreamMode::YellowstoneGrpcBlockMeta,
                &block_meta_host,
                &derived_state_host,
            )
            .is_ok()
        );
        assert!(
            enforce_provider_stream_capability_policy(
                ProviderStreamMode::YellowstoneGrpcSlots,
                &slot_status_host,
                &derived_state_host,
            )
            .is_ok()
        );

        crate::runtime_env::clear_runtime_env_overrides();
    }

    #[test]
    fn provider_stream_strict_policy_allows_transaction_status_and_block_meta_derived_state() {
        crate::runtime_env::set_runtime_env_overrides([(
            String::from("SOF_PROVIDER_STREAM_CAPABILITY_POLICY"),
            String::from("strict"),
        )]);

        let plugin_host = PluginHost::builder().build();
        let transaction_status_host = DerivedStateHost::builder()
            .add_consumer(TransactionStatusDerivedStateConsumer)
            .build();
        let block_meta_host = DerivedStateHost::builder()
            .add_consumer(BlockMetaDerivedStateConsumer)
            .build();

        assert!(
            enforce_provider_stream_capability_policy(
                ProviderStreamMode::YellowstoneGrpcTransactionStatus,
                &plugin_host,
                &transaction_status_host,
            )
            .is_ok()
        );
        assert!(
            enforce_provider_stream_capability_policy(
                ProviderStreamMode::YellowstoneGrpcBlockMeta,
                &plugin_host,
                &block_meta_host,
            )
            .is_ok()
        );
        assert!(
            enforce_provider_stream_capability_policy(
                ProviderStreamMode::LaserStreamTransactionStatus,
                &plugin_host,
                &transaction_status_host,
            )
            .is_ok()
        );
        assert!(
            enforce_provider_stream_capability_policy(
                ProviderStreamMode::LaserStreamBlockMeta,
                &plugin_host,
                &block_meta_host,
            )
            .is_ok()
        );

        crate::runtime_env::clear_runtime_env_overrides();
    }

    #[test]
    fn dispatch_provider_stream_update_feeds_transaction_status_and_block_meta_into_derived_state()
    {
        let events = Arc::new(Mutex::new(Vec::new()));
        let plugin_host = PluginHost::builder().build();
        let derived_state_host = DerivedStateHost::builder()
            .add_consumer(RecordingDerivedStateConsumer {
                events: Arc::clone(&events),
                config: DerivedStateConsumerConfig::new()
                    .with_transaction_status_observed()
                    .with_block_meta_observed(),
            })
            .build();

        dispatch_provider_stream_update(
            &plugin_host,
            &derived_state_host,
            sample_provider_transaction_status_update(77),
        );
        dispatch_provider_stream_update(
            &plugin_host,
            &derived_state_host,
            sample_provider_block_meta_update(78),
        );

        let events = events
            .lock()
            .expect("recording-derived-state mutex should not be poisoned");
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            DerivedStateFeedEvent::TransactionStatusObserved(_)
        ));
        assert!(matches!(
            events[1],
            DerivedStateFeedEvent::BlockMetaObserved(_)
        ));
    }

    #[tokio::test]
    async fn provider_stream_strict_policy_fails_before_plugin_and_extension_startup() {
        crate::runtime_env::set_runtime_env_overrides([(
            String::from("SOF_PROVIDER_STREAM_CAPABILITY_POLICY"),
            String::from("strict"),
        )]);
        let plugin_counter = Arc::new(AtomicUsize::new(0));
        let extension_counter = Arc::new(AtomicUsize::new(0));
        let plugin_host = PluginHost::builder()
            .add_plugin(StartupCounterPlugin {
                counter: Arc::clone(&plugin_counter),
            })
            .build();
        let extension_host = RuntimeExtensionHost::builder()
            .add_extension(StartupCounterExtension {
                counter: Arc::clone(&extension_counter),
            })
            .build();
        let (_tx, rx) = crate::provider_stream::create_provider_stream_queue(1);

        let result = run_provider_stream_runtime(
            plugin_host,
            extension_host,
            DerivedStateHost::builder().build(),
            None,
            ProviderStreamMode::YellowstoneGrpc,
            rx,
        )
        .await;
        crate::runtime_env::clear_runtime_env_overrides();

        assert!(matches!(
            result,
            Err(RuntimeError::ProviderStream(
                ProviderStreamRuntimeError::BuiltInUnsupportedHooks { .. }
            ))
        ));
        assert_eq!(plugin_counter.load(Ordering::Relaxed), 0);
        assert_eq!(extension_counter.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn provider_stream_runtime_errors_when_ingress_channel_closes_unexpectedly() {
        let plugin_host = PluginHost::builder()
            .add_plugin(TransactionOnlyPlugin)
            .build();
        let extension_host = RuntimeExtensionHost::builder().build();
        let (tx, rx) = crate::provider_stream::create_provider_stream_queue(1);
        drop(tx);

        let result = run_provider_stream_runtime(
            plugin_host,
            extension_host,
            DerivedStateHost::builder().build(),
            None,
            ProviderStreamMode::YellowstoneGrpc,
            rx,
        )
        .await;

        match result {
            Err(RuntimeError::ProviderStream(ProviderStreamRuntimeError::IngressClosed {
                mode,
                degraded_sources,
            })) => {
                assert_eq!(mode, ProviderStreamMode::YellowstoneGrpc);
                assert!(degraded_sources.is_empty());
            }
            other => panic!("expected provider ingress closure failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn provider_stream_runtime_surfaces_degraded_source_on_channel_close() {
        let plugin_host = PluginHost::builder()
            .add_plugin(TransactionOnlyPlugin)
            .build();
        let extension_host = RuntimeExtensionHost::builder().build();
        let (tx, rx) = crate::provider_stream::create_provider_stream_queue(4);
        tx.send(
            crate::provider_stream::ProviderSourceHealthEvent {
                source: crate::provider_stream::ProviderSourceIdentity::new(
                    crate::provider_stream::ProviderSourceId::YellowstoneGrpc,
                    "yellowstone-grpc-1",
                ),
                readiness: crate::provider_stream::ProviderSourceReadiness::Required,
                status: crate::provider_stream::ProviderSourceHealthStatus::Reconnecting,
                reason: crate::provider_stream::ProviderSourceHealthReason::UpstreamProtocolFailure,
                message: "upstream stalled".to_owned(),
            }
            .into(),
        )
        .await
        .expect("health sent");
        drop(tx);

        let result = run_provider_stream_runtime(
            plugin_host,
            extension_host,
            DerivedStateHost::builder().build(),
            None,
            ProviderStreamMode::YellowstoneGrpc,
            rx,
        )
        .await;

        match result {
            Err(RuntimeError::ProviderStream(ProviderStreamRuntimeError::IngressClosed {
                degraded_sources,
                ..
            })) => {
                assert_eq!(degraded_sources.len(), 1);
                assert_eq!(
                    degraded_sources[0].source,
                    crate::provider_stream::ProviderSourceIdentity::new(
                        crate::provider_stream::ProviderSourceId::YellowstoneGrpc,
                        "yellowstone-grpc-1",
                    )
                );
                assert_eq!(
                    degraded_sources[0].status,
                    crate::provider_stream::ProviderSourceHealthStatus::Reconnecting
                );
                assert_eq!(
                    degraded_sources[0].reason,
                    crate::provider_stream::ProviderSourceHealthReason::UpstreamProtocolFailure
                );
                assert_eq!(degraded_sources[0].message, "upstream stalled");
            }
            other => panic!("expected degraded provider closure failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn provider_stream_runtime_tracks_same_kind_sources_by_instance() {
        let plugin_host = PluginHost::builder()
            .add_plugin(TransactionOnlyPlugin)
            .build();
        let extension_host = RuntimeExtensionHost::builder().build();
        let (tx, rx) = crate::provider_stream::create_provider_stream_queue(4);
        for instance in ["ws-tx-1", "ws-tx-2"] {
            tx.send(
                crate::provider_stream::ProviderSourceHealthEvent {
                    source: crate::provider_stream::ProviderSourceIdentity::new(
                        crate::provider_stream::ProviderSourceId::Generic(
                            instance.to_owned().into(),
                        ),
                        instance,
                    ),
                    readiness: crate::provider_stream::ProviderSourceReadiness::Required,
                    status: crate::provider_stream::ProviderSourceHealthStatus::Reconnecting,
                    reason:
                        crate::provider_stream::ProviderSourceHealthReason::UpstreamProtocolFailure,
                    message: format!("{instance} stalled"),
                }
                .into(),
            )
            .await
            .expect("health sent");
        }
        drop(tx);

        let result = run_provider_stream_runtime(
            plugin_host,
            extension_host,
            DerivedStateHost::builder().build(),
            None,
            ProviderStreamMode::Generic,
            rx,
        )
        .await;

        match result {
            Err(RuntimeError::ProviderStream(ProviderStreamRuntimeError::IngressClosed {
                degraded_sources,
                ..
            })) => {
                let mut instances = degraded_sources
                    .into_iter()
                    .map(|event| event.source.instance_str().to_owned())
                    .collect::<Vec<_>>();
                instances.sort();
                assert_eq!(instances, vec!["ws-tx-1".to_owned(), "ws-tx-2".to_owned()]);
            }
            other => panic!("expected degraded provider closure failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn provider_stream_runtime_allows_generic_eof_when_configured() {
        crate::runtime_env::set_runtime_env_overrides([(
            String::from("SOF_PROVIDER_STREAM_ALLOW_EOF"),
            String::from("true"),
        )]);
        let plugin_host = PluginHost::builder()
            .add_plugin(TransactionOnlyPlugin)
            .build();
        let extension_host = RuntimeExtensionHost::builder().build();
        let (tx, rx) = crate::provider_stream::create_provider_stream_queue(1);
        drop(tx);

        let result = run_provider_stream_runtime(
            plugin_host,
            extension_host,
            DerivedStateHost::builder().build(),
            None,
            ProviderStreamMode::Generic,
            rx,
        )
        .await;
        crate::runtime_env::clear_runtime_env_overrides();

        assert!(
            result.is_ok(),
            "expected configured generic EOF to stop cleanly"
        );
    }

    #[tokio::test]
    async fn provider_stream_runtime_rejects_generic_eof_after_degraded_health() {
        crate::runtime_env::set_runtime_env_overrides([(
            String::from("SOF_PROVIDER_STREAM_ALLOW_EOF"),
            String::from("true"),
        )]);
        let plugin_host = PluginHost::builder()
            .add_plugin(TransactionOnlyPlugin)
            .build();
        let extension_host = RuntimeExtensionHost::builder().build();
        let (tx, rx) = crate::provider_stream::create_provider_stream_queue(4);
        tx.send(
            crate::provider_stream::ProviderSourceHealthEvent {
                source: crate::provider_stream::ProviderSourceIdentity::new(
                    crate::provider_stream::ProviderSourceId::Generic(
                        "generic_source".to_owned().into(),
                    ),
                    "generic_source",
                ),
                readiness: crate::provider_stream::ProviderSourceReadiness::Required,
                status: crate::provider_stream::ProviderSourceHealthStatus::Reconnecting,
                reason: crate::provider_stream::ProviderSourceHealthReason::UpstreamProtocolFailure,
                message: "upstream stalled".to_owned(),
            }
            .into(),
        )
        .await
        .expect("health sent");
        drop(tx);

        let result = run_provider_stream_runtime(
            plugin_host,
            extension_host,
            DerivedStateHost::builder().build(),
            None,
            ProviderStreamMode::Generic,
            rx,
        )
        .await;
        crate::runtime_env::clear_runtime_env_overrides();

        match result {
            Err(RuntimeError::ProviderStream(ProviderStreamRuntimeError::IngressClosed {
                mode,
                degraded_sources,
            })) => {
                assert_eq!(mode, ProviderStreamMode::Generic);
                assert_eq!(degraded_sources.len(), 1);
                assert_eq!(degraded_sources[0].source.instance_str(), "generic_source");
            }
            other => panic!("expected degraded generic EOF failure, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "profiling fixture for provider-stream dispatch A/B"]
    fn provider_stream_transaction_dispatch_profile_fixture() {
        let iterations = profile_iterations(500_000);
        let plugin_host = PluginHost::builder().build();
        let derived_state_host = DerivedStateHost::builder().build();
        let update = sample_provider_transaction_update();

        let baseline_started = Instant::now();
        for _ in 0..iterations {
            dispatch_provider_stream_update_baseline(
                &plugin_host,
                &derived_state_host,
                update.clone(),
            );
        }
        let baseline_elapsed = baseline_started.elapsed();

        let optimized_started = Instant::now();
        for _ in 0..iterations {
            dispatch_provider_stream_update(&plugin_host, &derived_state_host, update.clone());
        }
        let optimized_elapsed = optimized_started.elapsed();

        eprintln!(
            "provider_stream_transaction_dispatch_profile_fixture iterations={} baseline_us={} optimized_us={}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
        );
    }

    #[test]
    #[ignore = "profiling fixture for provider serialized tx ignore fast-path"]
    fn provider_stream_serialized_transaction_prefilter_ignore_profile_fixture() {
        let iterations = profile_iterations(500_000);
        let plugin_host = PluginHost::builder()
            .add_plugin(PrefilterIgnorePlugin {
                filter: TransactionPrefilter::new(TransactionInterest::Critical)
                    .with_account_required([solana_pubkey::Pubkey::new_unique()]),
            })
            .build();
        let derived_state_host = DerivedStateHost::builder().build();
        let baseline_update = sample_provider_transaction_update();
        let optimized_update = sample_serialized_provider_transaction_update();

        let baseline_started = Instant::now();
        for _ in 0..iterations {
            dispatch_provider_stream_update_baseline(
                &plugin_host,
                &derived_state_host,
                baseline_update.clone(),
            );
        }
        let baseline_elapsed = baseline_started.elapsed();

        let optimized_started = Instant::now();
        for _ in 0..iterations {
            dispatch_provider_stream_update(
                &plugin_host,
                &derived_state_host,
                optimized_update.clone(),
            );
        }
        let optimized_elapsed = optimized_started.elapsed();

        eprintln!(
            "provider_stream_serialized_transaction_prefilter_ignore_profile_fixture iterations={} baseline_us={} optimized_us={}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
        );
    }

    #[test]
    #[ignore = "profiling fixture for baseline provider serialized tx ignore path"]
    fn provider_stream_serialized_transaction_prefilter_ignore_baseline_profile_fixture() {
        let iterations = profile_iterations(500_000);
        let plugin_host = PluginHost::builder()
            .add_plugin(PrefilterIgnorePlugin {
                filter: TransactionPrefilter::new(TransactionInterest::Critical)
                    .with_account_required([solana_pubkey::Pubkey::new_unique()]),
            })
            .build();
        let derived_state_host = DerivedStateHost::builder().build();
        let baseline_update = sample_provider_transaction_update();

        for _ in 0..iterations {
            dispatch_provider_stream_update_baseline(
                &plugin_host,
                &derived_state_host,
                baseline_update.clone(),
            );
        }
    }

    #[test]
    #[ignore = "profiling fixture for optimized provider serialized tx ignore path"]
    fn provider_stream_serialized_transaction_prefilter_ignore_optimized_profile_fixture() {
        let iterations = profile_iterations(500_000);
        let plugin_host = PluginHost::builder()
            .add_plugin(PrefilterIgnorePlugin {
                filter: TransactionPrefilter::new(TransactionInterest::Critical)
                    .with_account_required([solana_pubkey::Pubkey::new_unique()]),
            })
            .build();
        let derived_state_host = DerivedStateHost::builder().build();
        let optimized_update = sample_serialized_provider_transaction_update();

        for _ in 0..iterations {
            dispatch_provider_stream_update(
                &plugin_host,
                &derived_state_host,
                optimized_update.clone(),
            );
        }
    }

    #[test]
    #[ignore = "profiling fixture for provider serialized tx accepted path"]
    fn provider_stream_serialized_transaction_accept_profile_fixture() {
        let iterations = profile_iterations(500_000);
        let plugin_host = PluginHost::builder()
            .add_plugin(TransactionOnlyPlugin)
            .build();
        let derived_state_host = DerivedStateHost::builder().build();
        let baseline_update = sample_serialized_provider_transaction_update();
        let optimized_update = baseline_update.clone();

        let baseline_started = Instant::now();
        for _ in 0..iterations {
            let ProviderStreamUpdate::SerializedTransaction(event) = baseline_update.clone() else {
                panic!("expected serialized update fixture");
            };
            dispatch_provider_stream_serialized_transaction_baseline(
                &plugin_host,
                &derived_state_host,
                &event,
            );
        }
        let baseline_elapsed = baseline_started.elapsed();

        let optimized_started = Instant::now();
        for _ in 0..iterations {
            dispatch_provider_stream_update(
                &plugin_host,
                &derived_state_host,
                optimized_update.clone(),
            );
        }
        let optimized_elapsed = optimized_started.elapsed();

        eprintln!(
            "provider_stream_serialized_transaction_accept_profile_fixture iterations={} baseline_us={} optimized_us={}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
        );
    }
}
