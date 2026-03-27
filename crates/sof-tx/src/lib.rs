#![forbid(unsafe_code)]

//! Transaction SDK for building, signing, routing, and submitting Solana transactions.

#[cfg(feature = "sof-adapters")]
/// Optional adapters bridging SOF runtime signals into transaction SDK providers.
pub mod adapters;
#[allow(dead_code)]
mod builder;
/// Leader/blockhash provider traits and simple provider adapters.
pub mod providers;
/// Leader-target routing policy and signature dedupe primitives.
pub mod routing;
#[allow(dead_code)]
mod signing;
/// Submission client and mode orchestration.
pub mod submit;

pub use providers::{
    LeaderProvider, LeaderTarget, RecentBlockhashProvider, RpcRecentBlockhashProvider,
    RpcRecentBlockhashProviderConfig,
};
pub use routing::{RoutingPolicy, SignatureDeduper};
pub use sof_types::{PubkeyBytes, SignatureBytes};
#[cfg(feature = "jito-grpc")]
pub use submit::JitoGrpcTransport;
pub use submit::{
    DirectSubmitConfig, JitoSubmitConfig, RpcSubmitConfig, SignedTx, SubmitError, SubmitMode,
    SubmitReliability, SubmitResult, SubmitTransportError, TxFlowSafetyIssue, TxFlowSafetyQuality,
    TxFlowSafetySnapshot, TxFlowSafetySource, TxSubmitClient, TxSubmitClientBuilder,
    TxSubmitContext, TxSubmitGuardPolicy, TxSubmitOutcome, TxSubmitOutcomeKind,
    TxSubmitOutcomeReporter, TxSubmitSuppressionKey, TxToxicFlowRejectionReason,
    TxToxicFlowTelemetry, TxToxicFlowTelemetrySnapshot,
};
pub use submit::{
    JitoBlockEngineEndpoint, JitoBlockEngineRegion, JitoJsonRpcTransport, JitoSubmitResponse,
    JitoTransportConfig,
};
#[cfg(feature = "kernel-bypass")]
pub use submit::{KernelBypassDatagramSocket, KernelBypassDirectTransport};
