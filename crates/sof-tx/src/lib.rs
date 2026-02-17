#![forbid(unsafe_code)]

//! Transaction SDK for building, signing, routing, and submitting Solana transactions.

/// Transaction/message builder helpers.
pub mod builder;
/// Leader/blockhash provider traits and simple provider adapters.
pub mod providers;
/// Leader-target routing policy and signature dedupe primitives.
pub mod routing;
/// Signing boundary types.
pub mod signing;
/// Submission client and mode orchestration.
pub mod submit;

pub use builder::{BuilderError, DEFAULT_DEVELOPER_TIP_LAMPORTS, TxBuilder, UnsignedTx};
pub use providers::{LeaderProvider, LeaderTarget, RecentBlockhashProvider};
pub use routing::{RoutingPolicy, SignatureDeduper};
pub use signing::SignerRef;
pub use submit::{
    DirectSubmitConfig, RpcSubmitConfig, SignedTx, SubmitError, SubmitMode, SubmitReliability,
    SubmitResult, SubmitTransportError, TxSubmitClient,
};
