#![recursion_limit = "256"]
#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::missing_docs_in_private_items,
        clippy::panic,
        missing_docs
    )
)]

//! SOF observer engine and plugin framework.
//!
//! External users should usually start from [`crate::runtime`] and [`crate::framework`].
//!
//! # Start The Packaged Runtime
//!
//! ```no_run
//! use sof::runtime::ObserverRuntime;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), sof::runtime::RuntimeError> {
//!     ObserverRuntime::new().run_until_termination_signal().await
//! }
//! ```
//!
//! # Attach One Plugin
//!
//! ```no_run
//! use async_trait::async_trait;
//! use sof::{
//!     event::TxKind,
//!     framework::{ObserverPlugin, PluginConfig, PluginHost, TransactionEvent, TxCommitmentStatus},
//!     runtime::ObserverRuntime,
//! };
//!
//! #[derive(Clone, Copy, Debug, Default)]
//! struct NonVoteLogger;
//!
//! #[async_trait]
//! impl ObserverPlugin for NonVoteLogger {
//!     fn config(&self) -> PluginConfig {
//!         PluginConfig::new()
//!             .with_transaction()
//!             .at_commitment(TxCommitmentStatus::Confirmed)
//!     }
//!
//!     async fn on_transaction(&self, event: &TransactionEvent) {
//!         if event.kind == TxKind::VoteOnly {
//!             return;
//!         }
//!         tracing::info!(slot = event.slot, kind = ?event.kind, "transaction observed");
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), sof::runtime::RuntimeError> {
//!     let host = PluginHost::builder().add_plugin(NonVoteLogger).build();
//!
//!     ObserverRuntime::new()
//!         .with_plugin_host(host)
//!         .run_until_termination_signal()
//!         .await
//! }
//! ```
//!
//! # Request Explicit Inline Transaction Delivery
//!
//! ```no_run
//! use async_trait::async_trait;
//! use sof::{
//!     event::TxKind,
//!     framework::{
//!         ObserverPlugin, PluginConfig, PluginHost, TransactionDispatchMode, TransactionEvent,
//!         TxCommitmentStatus,
//!     },
//!     runtime::ObserverRuntime,
//! };
//!
//! #[derive(Clone, Copy, Debug, Default)]
//! struct InlineTxLogger;
//!
//! #[async_trait]
//! impl ObserverPlugin for InlineTxLogger {
//!     fn config(&self) -> PluginConfig {
//!         PluginConfig::new()
//!             .with_transaction_mode(TransactionDispatchMode::Inline)
//!             .at_commitment(TxCommitmentStatus::Processed)
//!     }
//!
//!     async fn on_transaction(&self, event: &TransactionEvent) {
//!         if event.kind == TxKind::VoteOnly {
//!             return;
//!         }
//!         tracing::info!(slot = event.slot, kind = ?event.kind, "inline transaction observed");
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), sof::runtime::RuntimeError> {
//!     let host = PluginHost::builder().add_plugin(InlineTxLogger).build();
//!
//!     ObserverRuntime::new()
//!         .with_plugin_host(host)
//!         .run_until_termination_signal()
//!         .await
//! }
//! ```
//!
//! [`crate::framework::TransactionDispatchMode::Inline`] is an explicit delivery contract for
//! `on_transaction`. SOF dispatches that hook as soon as one full serialized transaction is ready
//! on the anchored contiguous inline path, and falls back to the completed-dataset point only when
//! the early path is not yet reconstructable.
//!
//! # Prefer Compiled Transaction Prefilters
//!
//! ```no_run
//! use async_trait::async_trait;
//! use solana_pubkey::Pubkey;
//! use sof::{
//!     framework::{
//!         ObserverPlugin, PluginConfig, PluginHost, TransactionDispatchMode,
//!         TransactionInterest, TransactionPrefilter,
//!     },
//!     runtime::ObserverRuntime,
//! };
//!
//! #[derive(Clone, Debug)]
//! struct RaydiumPoolWatcher {
//!     filter: TransactionPrefilter,
//! }
//!
//! impl Default for RaydiumPoolWatcher {
//!     fn default() -> Self {
//!         let pool = Pubkey::new_unique();
//!         let program = Pubkey::new_unique();
//!         Self {
//!             filter: TransactionPrefilter::new(TransactionInterest::Critical)
//!                 .with_account_required([pool, program]),
//!         }
//!     }
//! }
//!
//! #[async_trait]
//! impl ObserverPlugin for RaydiumPoolWatcher {
//!     fn config(&self) -> PluginConfig {
//!         PluginConfig::new().with_transaction_mode(TransactionDispatchMode::Inline)
//!     }
//!
//!     fn transaction_prefilter(&self) -> Option<&TransactionPrefilter> {
//!         Some(&self.filter)
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), sof::runtime::RuntimeError> {
//!     let host = PluginHost::builder()
//!         .add_plugin(RaydiumPoolWatcher::default())
//!         .build();
//!
//!     ObserverRuntime::new()
//!         .with_plugin_host(host)
//!         .run_until_termination_signal()
//!         .await
//! }
//! ```
//!
//! Prefer [`crate::framework::TransactionPrefilter`] when your plugin only
//! matches exact signatures or account-key presence. On the inline path, SOF can
//! use that compiled matcher on a sanitized transaction view and skip full owned
//! transaction decode for misses.
//!
//! More user-facing examples live in `crates/sof-observer/README.md` and the published example
//! programs under `crates/sof-observer/examples/`.

pub use sof_types::{PubkeyBytes, SignatureBytes};

#[doc(hidden)]
mod app;
/// Runtime environment override storage used by code-driven setup APIs.
mod runtime_env;

/// Runtime transaction event types.
pub mod event;
/// Plugin framework for attaching custom runtime hooks.
pub mod framework;
#[doc(hidden)]
pub mod ingest;
#[doc(hidden)]
pub mod protocol;
/// Processed provider-stream ingress types and adapters.
pub mod provider_stream;
#[doc(hidden)]
pub mod reassembly;
#[doc(hidden)]
pub mod relay;
#[doc(hidden)]
pub mod repair;
/// Packaged runtime entrypoints for embedding SOF.
pub mod runtime;
/// Runtime-stage counters for ingress, dataset reconstruction, and tx delivery.
pub mod runtime_metrics;
#[doc(hidden)]
pub mod shred;
#[doc(hidden)]
pub mod verify;
