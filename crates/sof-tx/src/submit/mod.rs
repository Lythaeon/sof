//! Transaction submission client and mode orchestration.

/// Submission client implementation and mode orchestration.
mod client;
/// UDP direct transport implementation.
mod direct;
/// Jito block-engine transport implementation.
mod jito;
#[cfg(feature = "jito-grpc")]
/// Jito gRPC bundle transport implementation.
mod jito_grpc;
#[cfg(feature = "kernel-bypass")]
/// Kernel-bypass direct transport hooks for `kernel-bypass` integrations.
mod kernel_bypass;
/// JSON-RPC transport implementation.
mod rpc;
#[cfg(test)]
/// Submission module unit tests.
mod tests;
/// Shared submission types, errors, and transport traits.
mod types;

pub use client::TxSubmitClient;
pub use direct::UdpDirectTransport;
pub use jito::{
    JitoBlockEngineEndpoint, JitoBlockEngineRegion, JitoJsonRpcTransport, JitoTransportConfig,
};
#[cfg(feature = "jito-grpc")]
pub use jito_grpc::JitoGrpcTransport;
#[cfg(feature = "kernel-bypass")]
pub use kernel_bypass::{KernelBypassDatagramSocket, KernelBypassDirectTransport};
pub use rpc::JsonRpcTransport;
pub use types::{
    DirectSubmitConfig, DirectSubmitTransport, JitoSubmitConfig, JitoSubmitResponse,
    JitoSubmitTransport, RpcSubmitConfig, RpcSubmitTransport, SignedTx, SubmitError, SubmitMode,
    SubmitReliability, SubmitResult, SubmitTransportError, TxFlowSafetyIssue, TxFlowSafetyQuality,
    TxFlowSafetySnapshot, TxFlowSafetySource, TxSubmitContext, TxSubmitGuardPolicy,
    TxSubmitOutcome, TxSubmitOutcomeKind, TxSubmitOutcomeReporter, TxSubmitSuppressionKey,
    TxToxicFlowRejectionReason, TxToxicFlowTelemetry, TxToxicFlowTelemetrySnapshot,
};
