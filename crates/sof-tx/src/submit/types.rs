//! Shared submission types, errors, and transport traits.

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sof_types::SignatureBytes;
use thiserror::Error;

use crate::{providers::LeaderTarget, routing::RoutingPolicy};

/// Legacy runtime submit presets.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum SubmitMode {
    /// Submit only through JSON-RPC.
    RpcOnly,
    /// Submit only through Jito block engine.
    JitoOnly,
    /// Submit only through direct leader/validator targets.
    DirectOnly,
    /// Submit direct first, then RPC fallback on failure.
    Hybrid,
}

/// One concrete submit route.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum SubmitRoute {
    /// JSON-RPC submission.
    Rpc,
    /// Jito block-engine submission.
    Jito,
    /// Direct leader/validator submission.
    Direct,
}

/// Route execution policy for one submission attempt.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default, Serialize, Deserialize)]
pub enum SubmitStrategy {
    /// Execute routes in order and stop at the first accepted route.
    #[default]
    OrderedFallback,
    /// Execute all configured routes at the same time.
    AllAtOnce,
}

/// Route-plan based submission configuration.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SubmitPlan {
    /// Configured submit routes.
    pub routes: Vec<SubmitRoute>,
    /// Route execution strategy.
    pub strategy: SubmitStrategy,
}

impl SubmitPlan {
    /// Creates one normalized plan.
    #[must_use]
    pub fn new(routes: Vec<SubmitRoute>, strategy: SubmitStrategy) -> Self {
        Self { routes, strategy }.into_normalized()
    }

    /// Normalizes this plan in place while preserving route order.
    #[must_use]
    pub fn into_normalized(mut self) -> Self {
        let mut seen_rpc = false;
        let mut seen_jito = false;
        let mut seen_direct = false;
        self.routes.retain(|route| match route {
            SubmitRoute::Rpc if seen_rpc => false,
            SubmitRoute::Rpc => {
                seen_rpc = true;
                true
            }
            SubmitRoute::Jito if seen_jito => false,
            SubmitRoute::Jito => {
                seen_jito = true;
                true
            }
            SubmitRoute::Direct if seen_direct => false,
            SubmitRoute::Direct => {
                seen_direct = true;
                true
            }
        });
        self
    }

    /// Returns a normalized clone of this plan.
    #[must_use]
    pub fn normalized(&self) -> Self {
        self.clone().into_normalized()
    }

    /// Builds a single-route RPC fallback plan.
    #[must_use]
    pub fn rpc_only() -> Self {
        Self::new(vec![SubmitRoute::Rpc], SubmitStrategy::OrderedFallback)
    }

    /// Builds a single-route Jito plan.
    #[must_use]
    pub fn jito_only() -> Self {
        Self::new(vec![SubmitRoute::Jito], SubmitStrategy::OrderedFallback)
    }

    /// Builds a single-route direct plan.
    #[must_use]
    pub fn direct_only() -> Self {
        Self::new(vec![SubmitRoute::Direct], SubmitStrategy::OrderedFallback)
    }

    /// Builds one custom ordered-fallback plan.
    #[must_use]
    pub fn ordered(routes: Vec<SubmitRoute>) -> Self {
        Self::new(routes, SubmitStrategy::OrderedFallback)
    }

    /// Builds the legacy direct-then-RPC fallback plan.
    #[must_use]
    pub fn hybrid() -> Self {
        Self::ordered(vec![SubmitRoute::Direct, SubmitRoute::Rpc])
    }

    /// Builds one concurrent all-route plan.
    #[must_use]
    pub fn all_at_once(routes: Vec<SubmitRoute>) -> Self {
        Self::new(routes, SubmitStrategy::AllAtOnce)
    }

    /// Returns the matching legacy preset when this plan is one exact legacy shape.
    #[must_use]
    pub fn legacy_mode(&self) -> Option<SubmitMode> {
        match (self.strategy, self.routes.as_slice()) {
            (SubmitStrategy::OrderedFallback, [SubmitRoute::Rpc]) => Some(SubmitMode::RpcOnly),
            (SubmitStrategy::OrderedFallback, [SubmitRoute::Jito]) => Some(SubmitMode::JitoOnly),
            (SubmitStrategy::OrderedFallback, [SubmitRoute::Direct]) => {
                Some(SubmitMode::DirectOnly)
            }
            (SubmitStrategy::OrderedFallback, [SubmitRoute::Direct, SubmitRoute::Rpc]) => {
                Some(SubmitMode::Hybrid)
            }
            _ => None,
        }
    }
}

impl Default for SubmitPlan {
    fn default() -> Self {
        Self::rpc_only()
    }
}

impl From<SubmitMode> for SubmitPlan {
    fn from(value: SubmitMode) -> Self {
        match value {
            SubmitMode::RpcOnly => Self::rpc_only(),
            SubmitMode::JitoOnly => Self::jito_only(),
            SubmitMode::DirectOnly => Self::direct_only(),
            SubmitMode::Hybrid => Self::hybrid(),
        }
    }
}

/// Reliability profile for direct and hybrid submission behavior.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default, Serialize, Deserialize)]
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

/// Jito block-engine submit tuning.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct JitoSubmitConfig {
    /// Enables revert protection by sending through bundle-only mode.
    ///
    /// This applies to the JSON-RPC transaction path. The gRPC transport is bundle-based and
    /// therefore always behaves as a bundle submission.
    pub bundle_only: bool,
}

/// Successful Jito submission metadata.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct JitoSubmitResponse {
    /// Jito JSON-RPC returned transaction signature, when available.
    pub transaction_signature: Option<String>,
    /// Jito gRPC returned bundle UUID, when available.
    pub bundle_id: Option<String>,
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
    /// No blockhash available for unsigned submit path.
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
    /// Jito mode requested but no Jito transport was configured.
    #[error("jito transport is not configured")]
    MissingJitoTransport,
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
    /// Jito transport failure.
    #[error("jito submit failed: {source}")]
    Jito {
        /// Jito transport error.
        source: SubmitTransportError,
    },
    /// Internal synchronization failure.
    #[error("internal synchronization failure: {message}")]
    InternalSync {
        /// Synchronization error details.
        message: String,
    },
    /// Submit attempt was rejected by the toxic-flow guard.
    #[error("submission rejected by toxic-flow guard: {reason}")]
    ToxicFlow {
        /// Structured reason for the rejection.
        reason: TxToxicFlowRejectionReason,
    },
}

/// Summary of a successful submission.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SubmitResult {
    /// Signature parsed from submitted transaction bytes.
    pub signature: Option<SignatureBytes>,
    /// Route plan selected by caller.
    pub plan: SubmitPlan,
    /// Legacy preset used by caller when one exact preset was selected.
    pub legacy_mode: Option<SubmitMode>,
    /// First successful route observed before this submit call returned.
    pub first_success_route: Option<SubmitRoute>,
    ///
    /// Later background accepts from concurrently configured routes are reported through
    /// [`TxSubmitOutcomeReporter`] and counted by built-in telemetry, including any route-specific
    /// acceptance metadata, rather than retroactively mutating this synchronous result.
    pub successful_routes: Vec<SubmitRoute>,
    /// Target chosen by direct path when applicable.
    pub direct_target: Option<LeaderTarget>,
    /// RPC-returned signature string when RPC path succeeded before return.
    pub rpc_signature: Option<String>,
    /// Jito block-engine returned transaction signature when the JSON-RPC path succeeded before
    /// return.
    pub jito_signature: Option<String>,
    /// Jito block-engine returned bundle UUID when the gRPC bundle path succeeded before return.
    pub jito_bundle_id: Option<String>,
    /// True when one later ordered-fallback route accepted the submit before return.
    pub used_fallback_route: bool,
    /// Number of direct targets selected for submit attempt that succeeded.
    pub selected_target_count: usize,
    /// Number of unique validator identities in selected direct targets.
    pub selected_identity_count: usize,
}

/// Coarse toxic-flow quality used by submit guards.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum TxFlowSafetyQuality {
    /// Required inputs are present, coherent, and safe to use.
    Stable,
    /// Inputs exist but have not reached a stable confirmation boundary yet.
    Provisional,
    /// Inputs exist but the current branch still carries material reorg risk.
    ReorgRisk,
    /// Inputs are present but stale.
    Stale,
    /// Inputs are present but mutually inconsistent.
    Degraded,
    /// Required inputs are still missing.
    IncompleteControlPlane,
}

/// One concrete toxic-flow issue reported by a submit guard source.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum TxFlowSafetyIssue {
    /// Submit source is still recovering from replay/checkpoint continuity.
    ReplayRecoveryPending,
    /// Control-plane inputs are still missing.
    MissingControlPlane,
    /// Control-plane inputs are stale.
    StaleControlPlane,
    /// Control-plane inputs are inconsistent.
    DegradedControlPlane,
    /// Current branch carries reorg risk.
    ReorgRisk,
    /// Current branch is still provisional.
    Provisional,
}

/// Current toxic-flow safety snapshot exposed by one submit guard source.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct TxFlowSafetySnapshot {
    /// Coarse quality classification for the current control-plane view.
    pub quality: TxFlowSafetyQuality,
    /// Concrete issues behind `quality`.
    pub issues: Vec<TxFlowSafetyIssue>,
    /// Current upstream state version when known.
    pub current_state_version: Option<u64>,
    /// True when replay recovery is still pending.
    pub replay_recovery_pending: bool,
}

impl TxFlowSafetySnapshot {
    /// Returns true when the current snapshot is strategy-safe.
    #[must_use]
    pub const fn is_safe(&self) -> bool {
        matches!(self.quality, TxFlowSafetyQuality::Stable) && !self.replay_recovery_pending
    }
}

/// Dynamic source of toxic-flow safety state for submit guards.
pub trait TxFlowSafetySource: Send + Sync {
    /// Returns the latest toxic-flow safety snapshot.
    fn toxic_flow_snapshot(&self) -> TxFlowSafetySnapshot;
}

/// One key used to suppress repeated submission attempts for the same opportunity.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum TxSubmitSuppressionKey {
    /// Suppress by signature.
    Signature(SignatureBytes),
    /// Suppress by opaque opportunity identifier.
    Opportunity([u8; 32]),
    /// Suppress by hashed account set identifier.
    AccountSet([u8; 32]),
    /// Suppress by slot-window key.
    SlotWindow {
        /// Slot associated with the opportunity.
        slot: u64,
        /// Window width used to group nearby opportunities.
        window: u64,
    },
}

impl Hash for TxSubmitSuppressionKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Signature(signature) => {
                0_u8.hash(state);
                signature.as_array().hash(state);
            }
            Self::Opportunity(key) => {
                1_u8.hash(state);
                key.hash(state);
            }
            Self::AccountSet(key) => {
                2_u8.hash(state);
                key.hash(state);
            }
            Self::SlotWindow { slot, window } => {
                3_u8.hash(state);
                slot.hash(state);
                window.hash(state);
            }
        }
    }
}

/// Call-site context used by toxic-flow guards.
#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TxSubmitContext {
    /// Additional suppression keys to apply before submit.
    pub suppression_keys: Vec<TxSubmitSuppressionKey>,
    /// State version used when the submit decision was made.
    pub decision_state_version: Option<u64>,
    /// Timestamp when the opportunity or decision was created.
    pub opportunity_created_at: Option<SystemTime>,
}

/// Policy controlling toxic-flow submit rejection.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TxSubmitGuardPolicy {
    /// Reject when the flow-safety source is not `Stable`.
    pub require_stable_control_plane: bool,
    /// Reject when the flow-safety source reports replay recovery pending.
    pub reject_on_replay_recovery_pending: bool,
    /// Maximum allowed drift between decision and current state versions.
    pub max_state_version_drift: Option<u64>,
    /// Maximum allowed age for one opportunity before submit.
    pub max_opportunity_age: Option<Duration>,
    /// TTL applied to built-in suppression keys.
    pub suppression_ttl: Duration,
}

impl Default for TxSubmitGuardPolicy {
    fn default() -> Self {
        Self {
            require_stable_control_plane: true,
            reject_on_replay_recovery_pending: true,
            max_state_version_drift: Some(4),
            max_opportunity_age: Some(Duration::from_millis(750)),
            suppression_ttl: Duration::from_millis(750),
        }
    }
}

/// Concrete reason one submit attempt was rejected before transport.
#[derive(Debug, Error, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum TxToxicFlowRejectionReason {
    /// Control-plane quality is not safe enough.
    #[error("control-plane quality {quality:?} is not safe for submit")]
    UnsafeControlPlane {
        /// Current quality observed from the guard source.
        quality: TxFlowSafetyQuality,
    },
    /// Replay recovery is still pending.
    #[error("submit source is still recovering replay continuity")]
    ReplayRecoveryPending,
    /// One suppression key is still active.
    #[error("submit suppressed by active key")]
    Suppressed,
    /// State version drift exceeded policy.
    #[error("state version drift {drift} exceeded maximum {max_allowed}")]
    StateDrift {
        /// Observed drift between decision and current state versions.
        drift: u64,
        /// Maximum drift allowed by policy.
        max_allowed: u64,
    },
    /// Opportunity age exceeded policy.
    #[error("opportunity age {age_ms}ms exceeded maximum {max_allowed_ms}ms")]
    OpportunityStale {
        /// Observed age in milliseconds.
        age_ms: u64,
        /// Maximum age allowed in milliseconds.
        max_allowed_ms: u64,
    },
}

/// Final or immediate outcome classification for one submit attempt.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum TxSubmitOutcomeKind {
    /// Direct path accepted the transaction.
    DirectAccepted,
    /// RPC path accepted the transaction.
    RpcAccepted,
    /// Jito block-engine accepted the transaction.
    JitoAccepted,
    /// Transaction landed on chain.
    Landed,
    /// Transaction expired before landing.
    Expired,
    /// Transaction was dropped before landing.
    Dropped,
    /// Route missed the intended leader window.
    LeaderMissed,
    /// Submit used a stale blockhash.
    BlockhashStale,
    /// Selected route was unhealthy.
    UnhealthyRoute,
    /// Submit was rejected due to stale inputs.
    RejectedDueToStaleness,
    /// Submit was rejected due to reorg risk.
    RejectedDueToReorgRisk,
    /// Submit was rejected due to state drift.
    RejectedDueToStateDrift,
    /// Submit was rejected by replay recovery pending.
    RejectedDueToReplayRecovery,
    /// Submit was suppressed by a built-in key.
    Suppressed,
}

/// Structured outcome record for toxic-flow telemetry/reporting.
///
/// Route-level accepts may carry richer metadata than the synchronous [`SubmitResult`] when one
/// different route already returned first. Consumers that care about later accepts should observe
/// this surface rather than expecting the original [`SubmitResult`] to mutate.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct TxSubmitOutcome {
    /// Outcome classification.
    pub kind: TxSubmitOutcomeKind,
    /// Transaction signature when available.
    pub signature: Option<SignatureBytes>,
    /// Concrete accepted route when the outcome came from one route-level success.
    pub route: Option<SubmitRoute>,
    /// Route plan selected for the submit attempt.
    pub plan: SubmitPlan,
    /// Legacy preset used for the submit attempt when applicable.
    pub legacy_mode: Option<SubmitMode>,
    /// RPC-returned signature metadata when the RPC route accepted.
    pub rpc_signature: Option<String>,
    /// Jito-returned transaction signature metadata when the Jito route accepted.
    pub jito_signature: Option<String>,
    /// Jito-returned bundle UUID when the gRPC bundle route accepted.
    pub jito_bundle_id: Option<String>,
    /// Current state version at outcome time when known.
    pub state_version: Option<u64>,
    /// Opportunity age in milliseconds when known.
    pub opportunity_age_ms: Option<u64>,
}

/// Callback surface for external outcome sinks.
pub trait TxSubmitOutcomeReporter: Send + Sync {
    /// Records one structured outcome.
    fn record_outcome(&self, outcome: &TxSubmitOutcome);
}

/// Snapshot of built-in toxic-flow counters collected by [`TxSubmitClient`](crate::submit::TxSubmitClient).
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TxToxicFlowTelemetrySnapshot {
    /// Number of direct-route accepts observed.
    pub direct_accepted: u64,
    /// Number of RPC-route accepts observed.
    pub rpc_accepted: u64,
    /// Number of Jito-route accepts observed.
    pub jito_accepted: u64,
    /// Number of submit attempts rejected due to stale inputs.
    pub rejected_due_to_staleness: u64,
    /// Number of submit attempts rejected due to reorg risk.
    pub rejected_due_to_reorg_risk: u64,
    /// Number of submit attempts rejected due to state drift.
    pub rejected_due_to_state_drift: u64,
    /// Number of accepted submits emitted while the source was stale.
    pub submit_on_stale_blockhash: u64,
    /// Number of route misses classified as leader misses.
    pub leader_route_miss_rate: u64,
    /// Most recent opportunity age seen at submit time.
    pub opportunity_age_at_send_ms: Option<u64>,
    /// Number of submits rejected while replay recovery was pending.
    pub rejected_due_to_replay_recovery: u64,
    /// Number of submits suppressed by a built-in key.
    pub suppressed_submissions: u64,
}

/// One cache-line-aligned atomic counter used by hot telemetry updates.
#[derive(Debug, Default)]
#[repr(align(64))]
struct CacheAlignedAtomicU64(AtomicU64);

impl CacheAlignedAtomicU64 {
    /// Loads the current counter value.
    fn load(&self, ordering: Ordering) -> u64 {
        self.0.load(ordering)
    }

    /// Stores a new value and returns the previous one.
    fn swap(&self, value: u64, ordering: Ordering) -> u64 {
        self.0.swap(value, ordering)
    }

    /// Increments the counter and returns the previous value.
    fn fetch_add(&self, value: u64, ordering: Ordering) -> u64 {
        self.0.fetch_add(value, ordering)
    }
}

/// In-memory telemetry counters for toxic-flow outcomes.
#[derive(Debug, Default)]
pub struct TxToxicFlowTelemetry {
    /// Number of direct-route accepts.
    direct_accepted: CacheAlignedAtomicU64,
    /// Number of RPC-route accepts.
    rpc_accepted: CacheAlignedAtomicU64,
    /// Number of Jito-route accepts.
    jito_accepted: CacheAlignedAtomicU64,
    /// Number of stale-input rejections.
    rejected_due_to_staleness: CacheAlignedAtomicU64,
    /// Number of reorg-risk rejections.
    rejected_due_to_reorg_risk: CacheAlignedAtomicU64,
    /// Number of state-drift rejections.
    rejected_due_to_state_drift: CacheAlignedAtomicU64,
    /// Number of accepted submits observed with stale blockhash state.
    submit_on_stale_blockhash: CacheAlignedAtomicU64,
    /// Number of leader-route misses.
    leader_route_miss_rate: CacheAlignedAtomicU64,
    /// Last opportunity age seen by the client.
    opportunity_age_at_send_ms: CacheAlignedAtomicU64,
    /// Number of replay-recovery rejections.
    rejected_due_to_replay_recovery: CacheAlignedAtomicU64,
    /// Number of suppressed submissions.
    suppressed_submissions: CacheAlignedAtomicU64,
}

impl TxToxicFlowTelemetry {
    /// Returns a shareable telemetry sink.
    #[must_use]
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Records one structured outcome.
    pub fn record(&self, outcome: &TxSubmitOutcome) {
        if let Some(age_ms) = outcome.opportunity_age_ms {
            let _ = self
                .opportunity_age_at_send_ms
                .swap(age_ms, Ordering::Relaxed);
        }
        match outcome.kind {
            TxSubmitOutcomeKind::DirectAccepted => {
                let _ = self.direct_accepted.fetch_add(1, Ordering::Relaxed);
            }
            TxSubmitOutcomeKind::RpcAccepted => {
                let _ = self.rpc_accepted.fetch_add(1, Ordering::Relaxed);
            }
            TxSubmitOutcomeKind::JitoAccepted => {
                let _ = self.jito_accepted.fetch_add(1, Ordering::Relaxed);
            }
            TxSubmitOutcomeKind::RejectedDueToStaleness => {
                let _ = self
                    .rejected_due_to_staleness
                    .fetch_add(1, Ordering::Relaxed);
            }
            TxSubmitOutcomeKind::RejectedDueToReorgRisk => {
                let _ = self
                    .rejected_due_to_reorg_risk
                    .fetch_add(1, Ordering::Relaxed);
            }
            TxSubmitOutcomeKind::RejectedDueToStateDrift => {
                let _ = self
                    .rejected_due_to_state_drift
                    .fetch_add(1, Ordering::Relaxed);
            }
            TxSubmitOutcomeKind::RejectedDueToReplayRecovery => {
                let _ = self
                    .rejected_due_to_replay_recovery
                    .fetch_add(1, Ordering::Relaxed);
            }
            TxSubmitOutcomeKind::Suppressed => {
                let _ = self.suppressed_submissions.fetch_add(1, Ordering::Relaxed);
            }
            TxSubmitOutcomeKind::LeaderMissed => {
                let _ = self.leader_route_miss_rate.fetch_add(1, Ordering::Relaxed);
            }
            TxSubmitOutcomeKind::Landed
            | TxSubmitOutcomeKind::Expired
            | TxSubmitOutcomeKind::Dropped
            | TxSubmitOutcomeKind::UnhealthyRoute => {}
            TxSubmitOutcomeKind::BlockhashStale => {
                let _ = self
                    .submit_on_stale_blockhash
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Returns the current telemetry snapshot.
    #[must_use]
    pub fn snapshot(&self) -> TxToxicFlowTelemetrySnapshot {
        let age_ms = self.opportunity_age_at_send_ms.load(Ordering::Relaxed);
        TxToxicFlowTelemetrySnapshot {
            direct_accepted: self.direct_accepted.load(Ordering::Relaxed),
            rpc_accepted: self.rpc_accepted.load(Ordering::Relaxed),
            jito_accepted: self.jito_accepted.load(Ordering::Relaxed),
            rejected_due_to_staleness: self.rejected_due_to_staleness.load(Ordering::Relaxed),
            rejected_due_to_reorg_risk: self.rejected_due_to_reorg_risk.load(Ordering::Relaxed),
            rejected_due_to_state_drift: self.rejected_due_to_state_drift.load(Ordering::Relaxed),
            submit_on_stale_blockhash: self.submit_on_stale_blockhash.load(Ordering::Relaxed),
            leader_route_miss_rate: self.leader_route_miss_rate.load(Ordering::Relaxed),
            opportunity_age_at_send_ms: if age_ms == 0 { None } else { Some(age_ms) },
            rejected_due_to_replay_recovery: self
                .rejected_due_to_replay_recovery
                .load(Ordering::Relaxed),
            suppressed_submissions: self.suppressed_submissions.load(Ordering::Relaxed),
        }
    }
}

impl TxSubmitOutcomeReporter for TxToxicFlowTelemetry {
    fn record_outcome(&self, outcome: &TxSubmitOutcome) {
        self.record(outcome);
    }
}

/// Internal suppression map used by the submit client.
#[derive(Debug, Default)]
pub(crate) struct TxSuppressionCache {
    /// Active suppression entries keyed by opportunity identity.
    entries: HashMap<TxSubmitSuppressionKey, SystemTime>,
}

impl TxSuppressionCache {
    /// Returns true when at least one key is still active inside `ttl`.
    pub(crate) fn is_suppressed(
        &mut self,
        keys: &[TxSubmitSuppressionKey],
        now: SystemTime,
        ttl: Duration,
    ) -> bool {
        self.evict_expired(now, ttl);
        keys.iter().any(|key| self.entries.contains_key(key))
    }

    /// Inserts all provided suppression keys with the current timestamp.
    pub(crate) fn insert_all(&mut self, keys: &[TxSubmitSuppressionKey], now: SystemTime) {
        for key in keys {
            let _ = self.entries.insert(key.clone(), now);
        }
    }

    /// Removes entries older than the current TTL window.
    fn evict_expired(&mut self, now: SystemTime, ttl: Duration) {
        self.entries.retain(|_, inserted_at| {
            now.duration_since(*inserted_at)
                .map(|elapsed| elapsed <= ttl)
                .unwrap_or(false)
        });
    }
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

/// Jito transport interface.
#[async_trait]
pub trait JitoSubmitTransport: Send + Sync {
    /// Submits transaction bytes to Jito block engine and returns Jito-specific acceptance data.
    async fn submit_jito(
        &self,
        tx_bytes: &[u8],
        config: &JitoSubmitConfig,
    ) -> Result<JitoSubmitResponse, SubmitTransportError>;
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
