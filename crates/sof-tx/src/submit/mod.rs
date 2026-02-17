//! Transaction submission client and mode orchestration.

/// Submission client implementation and mode orchestration.
mod client;
/// UDP direct transport implementation.
mod direct;
/// JSON-RPC transport implementation.
mod rpc;
#[cfg(test)]
/// Submission module unit tests.
mod tests;
/// Shared submission types, errors, and transport traits.
mod types;

pub use client::TxSubmitClient;
pub use direct::UdpDirectTransport;
pub use rpc::JsonRpcTransport;
pub use types::{
    DirectSubmitConfig, DirectSubmitTransport, RpcSubmitConfig, RpcSubmitTransport, SignedTx,
    SubmitError, SubmitMode, SubmitReliability, SubmitResult, SubmitTransportError,
};
