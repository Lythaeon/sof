//! Processed provider-stream ingress surfaces for SOF.
//!
//! Use this module when the upstream source is already beyond raw shreds, for
//! example Yellowstone gRPC or LaserStream-style processed transaction feeds.
//! These updates bypass SOF's packet, shred, FEC, and reconstruction stages and
//! enter directly at the plugin/derived-state transaction layer.
//!
//! Built-in mode capability summary:
//!
//! - `YellowstoneGrpc`: built-in adapter emits `on_transaction`
//! - `LaserStream`: built-in adapter emits `on_transaction`
//! - `WebsocketTransaction`: built-in adapter emits `on_transaction`
//!
//! Generic provider producers may still enqueue `TransactionViewBatch`,
//! `RecentBlockhash`, `SlotStatus`, or `ClusterTopology` updates directly.
//!
//! # Feed Provider Transactions Into SOF
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use solana_hash::Hash;
//! use solana_keypair::Keypair;
//! use solana_message::Message;
//! use solana_signer::Signer;
//! use solana_transaction::versioned::VersionedTransaction;
//! use sof::{
//!     event::{TxCommitmentStatus, TxKind},
//!     framework::TransactionEvent,
//!     provider_stream::{
//!         create_provider_stream_queue, ProviderStreamMode, ProviderStreamUpdate,
//!     },
//!     runtime::ObserverRuntime,
//! };
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let (tx, rx) = create_provider_stream_queue(128);
//! let signer = Keypair::new();
//! let message = Message::new(&[], Some(&signer.pubkey()));
//! let transaction = VersionedTransaction::try_new(message.into(), &[&signer])?;
//!
//! tx.send(ProviderStreamUpdate::Transaction(TransactionEvent {
//!     slot: 1,
//!     commitment_status: TxCommitmentStatus::Processed,
//!     confirmed_slot: None,
//!     finalized_slot: None,
//!     signature: transaction.signatures.first().copied(),
//!     tx: Arc::new(transaction),
//!     kind: TxKind::NonVote,
//! }))
//! .await?;
//!
//! ObserverRuntime::new()
//!     .with_provider_stream_ingress(ProviderStreamMode::YellowstoneGrpc, rx)
//!     .run_until(async {})
//!     .await?;
//! # Ok(())
//! # }
//! ```

use tokio::sync::mpsc;

use crate::framework::{
    ClusterTopologyEvent, ObservedRecentBlockhashEvent, SlotStatusEvent, TransactionEvent,
    TransactionLogEvent, TransactionViewBatchEvent,
};

/// Default queue capacity for processed provider-stream ingress.
pub const DEFAULT_PROVIDER_STREAM_QUEUE_CAPACITY: usize = 8_192;

/// Identifies the processed provider family driving SOF's direct plugin ingress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderStreamMode {
    /// Yellowstone gRPC / Geyser-style processed transaction feeds.
    ///
    /// Built-in adapter hook surface today: `on_transaction`.
    YellowstoneGrpc,
    /// LaserStream-style processed transaction feeds.
    ///
    /// Built-in adapter hook surface today: `on_transaction`.
    LaserStream,
    #[cfg(feature = "provider-websocket")]
    /// Websocket `transactionSubscribe` processed transaction feeds.
    ///
    /// Built-in adapter hook surface today: `on_transaction`.
    WebsocketTransaction,
}

impl ProviderStreamMode {
    /// Returns the stable string label used in logs and docs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::YellowstoneGrpc => "yellowstone_grpc",
            Self::LaserStream => "laserstream",
            #[cfg(feature = "provider-websocket")]
            Self::WebsocketTransaction => "websocket_transaction",
        }
    }
}

/// One processed provider-stream update accepted by SOF.
#[derive(Debug, Clone)]
pub enum ProviderStreamUpdate {
    /// One provider transaction mapped onto SOF's transaction hook surface.
    Transaction(TransactionEvent),
    /// One provider/websocket transaction-log notification.
    TransactionLog(TransactionLogEvent),
    /// One provider transaction-view batch mapped onto SOF's view-batch surface.
    TransactionViewBatch(TransactionViewBatchEvent),
    /// One provider recent-blockhash observation.
    RecentBlockhash(ObservedRecentBlockhashEvent),
    /// One provider slot-status update.
    SlotStatus(SlotStatusEvent),
    /// One provider cluster-topology update.
    ClusterTopology(ClusterTopologyEvent),
}

impl From<TransactionEvent> for ProviderStreamUpdate {
    fn from(event: TransactionEvent) -> Self {
        Self::Transaction(event)
    }
}

impl From<TransactionLogEvent> for ProviderStreamUpdate {
    fn from(event: TransactionLogEvent) -> Self {
        Self::TransactionLog(event)
    }
}

impl From<TransactionViewBatchEvent> for ProviderStreamUpdate {
    fn from(event: TransactionViewBatchEvent) -> Self {
        Self::TransactionViewBatch(event)
    }
}

impl From<ObservedRecentBlockhashEvent> for ProviderStreamUpdate {
    fn from(event: ObservedRecentBlockhashEvent) -> Self {
        Self::RecentBlockhash(event)
    }
}

impl From<SlotStatusEvent> for ProviderStreamUpdate {
    fn from(event: SlotStatusEvent) -> Self {
        Self::SlotStatus(event)
    }
}

impl From<ClusterTopologyEvent> for ProviderStreamUpdate {
    fn from(event: ClusterTopologyEvent) -> Self {
        Self::ClusterTopology(event)
    }
}

/// Sender type for processed provider-stream ingress.
pub type ProviderStreamSender = mpsc::Sender<ProviderStreamUpdate>;
/// Receiver type for processed provider-stream ingress.
pub type ProviderStreamReceiver = mpsc::Receiver<ProviderStreamUpdate>;

/// Creates one bounded queue for processed provider-stream updates.
///
/// # Examples
///
/// ```rust
/// use sof::provider_stream::{create_provider_stream_queue, DEFAULT_PROVIDER_STREAM_QUEUE_CAPACITY};
///
/// let (_tx, _rx) = create_provider_stream_queue(DEFAULT_PROVIDER_STREAM_QUEUE_CAPACITY);
/// ```
#[must_use]
pub fn create_provider_stream_queue(
    capacity: usize,
) -> (ProviderStreamSender, ProviderStreamReceiver) {
    mpsc::channel(capacity.max(1))
}

#[cfg(feature = "provider-grpc")]
/// Yellowstone gRPC adapter helpers.
pub mod yellowstone;

#[cfg(feature = "provider-grpc")]
/// Helius LaserStream adapter helpers.
pub mod laserstream;

#[cfg(feature = "provider-websocket")]
/// Websocket `transactionSubscribe` adapter helpers.
pub mod websocket;
