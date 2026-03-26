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
//! use solana_message::{Message, VersionedMessage};
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
//! let transaction = VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[&signer])?;
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

use crate::event::TxCommitmentStatus;
use crate::event::TxKind;
use crate::framework::{
    ClusterTopologyEvent, ObservedRecentBlockhashEvent, SlotStatusEvent, TransactionEvent,
    TransactionLogEvent, TransactionViewBatchEvent,
};
use agave_transaction_view::{
    transaction_data::TransactionData, transaction_view::SanitizedTransactionView,
};
use solana_sdk_ids::{compute_budget, vote};
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;

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

/// Replay policy for processed provider transaction streams.
///
/// # Examples
///
/// ```rust
/// use sof::provider_stream::ProviderReplayMode;
///
/// let live = ProviderReplayMode::Live;
/// let resume = ProviderReplayMode::Resume;
/// let from_slot = ProviderReplayMode::FromSlot(1_000_000);
///
/// assert!(matches!(live, ProviderReplayMode::Live));
/// assert!(matches!(resume, ProviderReplayMode::Resume));
/// assert!(matches!(from_slot, ProviderReplayMode::FromSlot(1_000_000)));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProviderReplayMode {
    /// Start from the provider's live edge and do not request historical replay.
    Live,
    /// Start live and resume from the last tracked slot after reconnects.
    #[default]
    Resume,
    /// Request historical replay starting at one explicit slot, then continue
    /// with tracked resume behavior after reconnects.
    FromSlot(u64),
}

/// One processed provider-stream update accepted by SOF.
#[derive(Debug, Clone)]
pub enum ProviderStreamUpdate {
    /// One provider transaction mapped onto SOF's transaction hook surface.
    Transaction(TransactionEvent),
    /// One serialized provider transaction that can still be filtered before full decode.
    SerializedTransaction(SerializedTransactionEvent),
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

impl From<SerializedTransactionEvent> for ProviderStreamUpdate {
    fn from(event: SerializedTransactionEvent) -> Self {
        Self::SerializedTransaction(event)
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

/// One serialized provider-fed transaction that has not yet been materialized.
#[derive(Debug, Clone)]
pub struct SerializedTransactionEvent {
    /// Slot containing this transaction.
    pub slot: u64,
    /// Commitment status for this transaction slot when event was emitted.
    pub commitment_status: TxCommitmentStatus,
    /// Latest observed confirmed slot watermark when event was emitted.
    pub confirmed_slot: Option<u64>,
    /// Latest observed finalized slot watermark when event was emitted.
    pub finalized_slot: Option<u64>,
    /// Transaction signature if present.
    pub signature: Option<Signature>,
    /// Serialized transaction bytes.
    pub bytes: Box<[u8]>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ProviderCommitmentWatermarks {
    pub(crate) confirmed_slot: Option<u64>,
    pub(crate) finalized_slot: Option<u64>,
}

impl ProviderCommitmentWatermarks {
    #[inline]
    pub(crate) fn observe_transaction_commitment(
        &mut self,
        slot: u64,
        commitment_status: TxCommitmentStatus,
    ) {
        match commitment_status {
            TxCommitmentStatus::Processed => {}
            TxCommitmentStatus::Confirmed => {
                self.confirmed_slot = Some(self.confirmed_slot.unwrap_or(slot).max(slot));
            }
            TxCommitmentStatus::Finalized => {
                self.confirmed_slot = Some(self.confirmed_slot.unwrap_or(slot).max(slot));
                self.finalized_slot = Some(self.finalized_slot.unwrap_or(slot).max(slot));
            }
        }
    }

    #[inline]
    pub(crate) fn observe_confirmed_slot(&mut self, slot: u64) {
        self.confirmed_slot = Some(self.confirmed_slot.unwrap_or(slot).max(slot));
    }

    #[inline]
    pub(crate) fn observe_finalized_slot(&mut self, slot: u64) {
        self.observe_confirmed_slot(slot);
        self.finalized_slot = Some(self.finalized_slot.unwrap_or(slot).max(slot));
    }
}

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

/// Classifies provider-fed transactions consistently across built-in adapters.
pub(crate) fn classify_provider_transaction_kind(tx: &VersionedTransaction) -> TxKind {
    let mut has_vote = false;
    let mut has_non_vote_non_budget = false;
    let keys = tx.message.static_account_keys();
    for instruction in tx.message.instructions() {
        if let Some(program_id) = keys.get(usize::from(instruction.program_id_index)) {
            if *program_id == vote::id() {
                has_vote = true;
                if has_non_vote_non_budget {
                    return TxKind::Mixed;
                }
                continue;
            }
            if *program_id != compute_budget::id() {
                has_non_vote_non_budget = true;
                if has_vote {
                    return TxKind::Mixed;
                }
            }
        }
    }
    if has_vote && !has_non_vote_non_budget {
        TxKind::VoteOnly
    } else if has_vote {
        TxKind::Mixed
    } else {
        TxKind::NonVote
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn classify_provider_transaction_kind_view<D: TransactionData>(
    view: &SanitizedTransactionView<D>,
) -> TxKind {
    let mut has_vote = false;
    let mut has_non_vote_non_budget = false;
    for (program_id, _) in view.program_instructions_iter() {
        if *program_id == vote::id() {
            has_vote = true;
            if has_non_vote_non_budget {
                return TxKind::Mixed;
            }
            continue;
        }
        if *program_id != compute_budget::id() {
            has_non_vote_non_budget = true;
            if has_vote {
                return TxKind::Mixed;
            }
        }
    }
    if has_vote && !has_non_vote_non_budget {
        TxKind::VoteOnly
    } else if has_vote {
        TxKind::Mixed
    } else {
        TxKind::NonVote
    }
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

#[cfg(all(test, any(feature = "provider-grpc", feature = "provider-websocket")))]
mod tests {
    use super::*;
    use solana_instruction::Instruction;
    use solana_keypair::Keypair;
    use solana_message::{Message, VersionedMessage};
    use solana_sdk_ids::system_program;
    use solana_signer::Signer;
    use std::time::Instant;

    fn profile_iterations(default: usize) -> usize {
        std::env::var("SOF_PROFILE_ITERATIONS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(default)
    }

    fn sample_mixed_transaction() -> VersionedTransaction {
        let signer = Keypair::new();
        let mut instructions = Vec::with_capacity(34);
        instructions.push(Instruction::new_with_bytes(vote::id(), &[], vec![]));
        instructions.push(Instruction::new_with_bytes(
            system_program::id(),
            &[],
            vec![],
        ));
        for _ in 0..32 {
            instructions.push(Instruction::new_with_bytes(
                compute_budget::id(),
                &[],
                vec![],
            ));
        }
        let message = Message::new(&instructions, Some(&signer.pubkey()));
        VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[&signer]).expect("tx")
    }

    fn classify_provider_transaction_kind_baseline(tx: &VersionedTransaction) -> TxKind {
        let mut has_vote = false;
        let mut has_non_vote_non_budget = false;
        let keys = tx.message.static_account_keys();
        for instruction in tx.message.instructions() {
            if let Some(program_id) = keys.get(usize::from(instruction.program_id_index)) {
                if *program_id == vote::id() {
                    has_vote = true;
                    continue;
                }
                if *program_id != compute_budget::id() {
                    has_non_vote_non_budget = true;
                }
            }
        }
        if has_vote && !has_non_vote_non_budget {
            TxKind::VoteOnly
        } else if has_vote {
            TxKind::Mixed
        } else {
            TxKind::NonVote
        }
    }

    #[test]
    fn classify_provider_transaction_kind_detects_mixed() {
        let tx = sample_mixed_transaction();
        assert_eq!(classify_provider_transaction_kind(&tx), TxKind::Mixed);
    }

    #[test]
    #[ignore = "profiling fixture for provider transaction kind classification A/B"]
    fn provider_transaction_kind_profile_fixture() {
        let iterations = profile_iterations(1_000_000);

        let tx = sample_mixed_transaction();

        let baseline_started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(classify_provider_transaction_kind_baseline(&tx));
        }
        let baseline_elapsed = baseline_started.elapsed();

        let optimized_started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(classify_provider_transaction_kind(&tx));
        }
        let optimized_elapsed = optimized_started.elapsed();

        eprintln!(
            "provider_transaction_kind_profile_fixture iterations={} baseline_us={} optimized_us={}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
        );
    }

    #[test]
    #[ignore = "profiling fixture for baseline provider tx kind classification"]
    fn provider_transaction_kind_baseline_profile_fixture() {
        let iterations = profile_iterations(1_000_000);

        let tx = sample_mixed_transaction();
        for _ in 0..iterations {
            std::hint::black_box(classify_provider_transaction_kind_baseline(&tx));
        }
    }

    #[test]
    #[ignore = "profiling fixture for optimized provider tx kind classification"]
    fn provider_transaction_kind_optimized_profile_fixture() {
        let iterations = profile_iterations(1_000_000);

        let tx = sample_mixed_transaction();
        for _ in 0..iterations {
            std::hint::black_box(classify_provider_transaction_kind(&tx));
        }
    }
}
