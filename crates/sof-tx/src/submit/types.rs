//! Shared submission types, errors, and transport traits.

use std::time::Duration;

use async_trait::async_trait;
use solana_signature::Signature;
use thiserror::Error;

use crate::{builder::BuilderError, providers::LeaderTarget, routing::RoutingPolicy};

/// Runtime submit mode.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SubmitMode {
    /// Submit only through JSON-RPC.
    RpcOnly,
    /// Submit only through direct leader/validator targets.
    DirectOnly,
    /// Submit direct first, then RPC fallback on failure.
    Hybrid,
}

/// Reliability profile for direct and hybrid submission behavior.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum SubmitReliability {
    /// Fastest path with minimal retrying.
    LowLatency,
    /// Balanced latency and retry behavior.
    #[default]
    Balanced,
    /// Aggressive retrying before giving up.
    HighReliability,
}

/// Signed transaction payload variants accepted by submit APIs.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SignedTx {
    /// Bincode-serialized `VersionedTransaction` bytes.
    VersionedTransactionBytes(Vec<u8>),
    /// Wire-format transaction bytes.
    WireTransactionBytes(Vec<u8>),
}

/// RPC submit tuning.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RpcSubmitConfig {
    /// Skip preflight simulation when true.
    pub skip_preflight: bool,
    /// Optional preflight commitment string.
    pub preflight_commitment: Option<String>,
}

impl Default for RpcSubmitConfig {
    fn default() -> Self {
        Self {
            skip_preflight: true,
            preflight_commitment: None,
        }
    }
}

/// Direct submit tuning.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DirectSubmitConfig {
    /// Per-target send timeout.
    pub per_target_timeout: Duration,
    /// Global send budget for one submission.
    pub global_timeout: Duration,
    /// Number of rounds to iterate across selected direct targets.
    pub direct_target_rounds: usize,
    /// Number of direct-only submit attempts (target selection can refresh per attempt).
    pub direct_submit_attempts: usize,
    /// Number of direct submit attempts in `Hybrid` mode before RPC fallback.
    pub hybrid_direct_attempts: usize,
    /// Delay between direct rebroadcast attempts/rounds (Agave-like pacing).
    pub rebroadcast_interval: Duration,
    /// Enables Agave-style post-ack rebroadcast persistence for direct submits.
    pub agave_rebroadcast_enabled: bool,
    /// Maximum time budget for background rebroadcast persistence.
    pub agave_rebroadcast_window: Duration,
    /// Delay between background rebroadcast cycles.
    pub agave_rebroadcast_interval: Duration,
    /// When true, `Hybrid` mode broadcasts to RPC even after direct send succeeds.
    pub hybrid_rpc_broadcast: bool,
    /// Enables latency-aware ordering of direct targets before submit.
    pub latency_aware_targeting: bool,
    /// Timeout used for per-target TCP latency probes.
    pub latency_probe_timeout: Duration,
    /// Optional extra TCP port to probe (in addition to target TPU port).
    pub latency_probe_port: Option<u16>,
    /// Max number of targets to probe per submission.
    pub latency_probe_max_targets: usize,
}

impl DirectSubmitConfig {
    /// Builds a direct-submit config from a reliability profile.
    #[must_use]
    pub const fn from_reliability(reliability: SubmitReliability) -> Self {
        match reliability {
            SubmitReliability::LowLatency => Self {
                per_target_timeout: Duration::from_millis(200),
                global_timeout: Duration::from_millis(1_200),
                direct_target_rounds: 3,
                direct_submit_attempts: 3,
                hybrid_direct_attempts: 2,
                rebroadcast_interval: Duration::from_millis(90),
                agave_rebroadcast_enabled: true,
                agave_rebroadcast_window: Duration::from_secs(30),
                agave_rebroadcast_interval: Duration::from_millis(700),
                hybrid_rpc_broadcast: false,
                latency_aware_targeting: true,
                latency_probe_timeout: Duration::from_millis(80),
                latency_probe_port: Some(8899),
                latency_probe_max_targets: 128,
            },
            SubmitReliability::Balanced => Self {
                per_target_timeout: Duration::from_millis(300),
                global_timeout: Duration::from_millis(1_800),
                direct_target_rounds: 4,
                direct_submit_attempts: 4,
                hybrid_direct_attempts: 3,
                rebroadcast_interval: Duration::from_millis(110),
                agave_rebroadcast_enabled: true,
                agave_rebroadcast_window: Duration::from_secs(45),
                agave_rebroadcast_interval: Duration::from_millis(800),
                hybrid_rpc_broadcast: true,
                latency_aware_targeting: true,
                latency_probe_timeout: Duration::from_millis(120),
                latency_probe_port: Some(8899),
                latency_probe_max_targets: 128,
            },
            SubmitReliability::HighReliability => Self {
                per_target_timeout: Duration::from_millis(450),
                global_timeout: Duration::from_millis(3_200),
                direct_target_rounds: 6,
                direct_submit_attempts: 5,
                hybrid_direct_attempts: 4,
                rebroadcast_interval: Duration::from_millis(140),
                agave_rebroadcast_enabled: true,
                agave_rebroadcast_window: Duration::from_secs(70),
                agave_rebroadcast_interval: Duration::from_millis(900),
                hybrid_rpc_broadcast: true,
                latency_aware_targeting: true,
                latency_probe_timeout: Duration::from_millis(160),
                latency_probe_port: Some(8899),
                latency_probe_max_targets: 128,
            },
        }
    }

    /// Returns this config with minimum valid retry counters.
    #[must_use]
    pub const fn normalized(self) -> Self {
        let direct_target_rounds = if self.direct_target_rounds == 0 {
            1
        } else {
            self.direct_target_rounds
        };
        let direct_submit_attempts = if self.direct_submit_attempts == 0 {
            1
        } else {
            self.direct_submit_attempts
        };
        let hybrid_direct_attempts = if self.hybrid_direct_attempts == 0 {
            1
        } else {
            self.hybrid_direct_attempts
        };
        let latency_probe_max_targets = if self.latency_probe_max_targets == 0 {
            1
        } else {
            self.latency_probe_max_targets
        };
        let rebroadcast_interval = if self.rebroadcast_interval.is_zero() {
            Duration::from_millis(1)
        } else {
            self.rebroadcast_interval
        };
        let agave_rebroadcast_interval = if self.agave_rebroadcast_interval.is_zero() {
            Duration::from_millis(1)
        } else {
            self.agave_rebroadcast_interval
        };
        Self {
            per_target_timeout: self.per_target_timeout,
            global_timeout: self.global_timeout,
            direct_target_rounds,
            direct_submit_attempts,
            hybrid_direct_attempts,
            rebroadcast_interval,
            agave_rebroadcast_enabled: self.agave_rebroadcast_enabled,
            agave_rebroadcast_window: self.agave_rebroadcast_window,
            agave_rebroadcast_interval,
            hybrid_rpc_broadcast: self.hybrid_rpc_broadcast,
            latency_aware_targeting: self.latency_aware_targeting,
            latency_probe_timeout: self.latency_probe_timeout,
            latency_probe_port: self.latency_probe_port,
            latency_probe_max_targets,
        }
    }
}

impl Default for DirectSubmitConfig {
    fn default() -> Self {
        Self::from_reliability(SubmitReliability::default())
    }
}

/// Low-level transport errors surfaced by submit backends.
#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum SubmitTransportError {
    /// Invalid transport configuration.
    #[error("transport configuration invalid: {message}")]
    Config {
        /// Human-readable description.
        message: String,
    },
    /// Transport operation failed.
    #[error("transport failure: {message}")]
    Failure {
        /// Human-readable description.
        message: String,
    },
}

/// Submission-level errors.
#[derive(Debug, Error)]
pub enum SubmitError {
    /// Could not build/sign transaction for builder submit path.
    #[error("failed to build/sign transaction: {source}")]
    Build {
        /// Builder-layer failure.
        source: BuilderError,
    },
    /// No blockhash available for builder submit path.
    #[error("blockhash provider returned no recent blockhash")]
    MissingRecentBlockhash,
    /// Signed bytes could not be decoded into a transaction.
    #[error("failed to decode signed transaction bytes: {source}")]
    DecodeSignedBytes {
        /// Bincode decode error.
        source: Box<bincode::ErrorKind>,
    },
    /// Duplicate signature was suppressed by dedupe window.
    #[error("duplicate signature suppressed by dedupe window")]
    DuplicateSignature,
    /// RPC mode requested but no RPC transport was configured.
    #[error("rpc transport is not configured")]
    MissingRpcTransport,
    /// Direct mode requested but no direct transport was configured.
    #[error("direct transport is not configured")]
    MissingDirectTransport,
    /// No direct targets resolved from routing inputs.
    #[error("no direct targets resolved from leader/backups")]
    NoDirectTargets,
    /// Direct transport failure.
    #[error("direct submit failed: {source}")]
    Direct {
        /// Direct transport error.
        source: SubmitTransportError,
    },
    /// RPC transport failure.
    #[error("rpc submit failed: {source}")]
    Rpc {
        /// RPC transport error.
        source: SubmitTransportError,
    },
    /// Internal synchronization failure.
    #[error("internal synchronization failure: {message}")]
    InternalSync {
        /// Synchronization error details.
        message: String,
    },
}

/// Summary of a successful submission.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SubmitResult {
    /// Signature parsed from submitted transaction bytes.
    pub signature: Option<Signature>,
    /// Mode selected by caller.
    pub mode: SubmitMode,
    /// Target chosen by direct path when applicable.
    pub direct_target: Option<LeaderTarget>,
    /// RPC-returned signature string when RPC path succeeded.
    pub rpc_signature: Option<String>,
    /// True when RPC fallback was used from hybrid mode.
    pub used_rpc_fallback: bool,
    /// Number of direct targets selected for submit attempt that succeeded.
    pub selected_target_count: usize,
    /// Number of unique validator identities in selected direct targets.
    pub selected_identity_count: usize,
}

/// RPC transport interface.
#[async_trait]
pub trait RpcSubmitTransport: Send + Sync {
    /// Submits transaction bytes to RPC and returns signature string.
    async fn submit_rpc(
        &self,
        tx_bytes: &[u8],
        config: &RpcSubmitConfig,
    ) -> Result<String, SubmitTransportError>;
}

/// Direct transport interface.
#[async_trait]
pub trait DirectSubmitTransport: Send + Sync {
    /// Submits transaction bytes to direct targets and returns the first successful target.
    async fn submit_direct(
        &self,
        tx_bytes: &[u8],
        targets: &[LeaderTarget],
        policy: RoutingPolicy,
        config: &DirectSubmitConfig,
    ) -> Result<LeaderTarget, SubmitTransportError>;
}
