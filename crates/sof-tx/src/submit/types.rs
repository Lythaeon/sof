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
}

impl Default for DirectSubmitConfig {
    fn default() -> Self {
        Self {
            per_target_timeout: Duration::from_millis(300),
            global_timeout: Duration::from_millis(1_200),
        }
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
