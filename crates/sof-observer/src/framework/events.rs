use std::{net::SocketAddr, sync::Arc};

use agave_transaction_view::transaction_view::SanitizedTransactionView;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;

use crate::{
    event::{ForkSlotStatus, TxCommitmentStatus, TxKind},
    shred::wire::ParsedShredHeader,
};

#[derive(Debug, Clone)]
/// Runtime event emitted for each ingress UDP packet before parsing.
pub struct RawPacketEvent {
    /// Source socket address that delivered the packet.
    pub source: SocketAddr,
    /// Packet payload bytes as seen on the network.
    pub bytes: Arc<[u8]>,
}

#[derive(Debug, Clone)]
/// Runtime event emitted after a packet was parsed as a shred.
pub struct ShredEvent {
    /// Source socket address that delivered the packet.
    pub source: SocketAddr,
    /// Original packet payload bytes.
    pub packet: Arc<[u8]>,
    /// Parsed shred header for this packet.
    pub parsed: Arc<ParsedShredHeader>,
}

#[derive(Debug, Clone, Copy)]
/// Runtime event emitted for each reconstructed contiguous dataset.
pub struct DatasetEvent {
    /// Slot number of the dataset.
    pub slot: u64,
    /// Start shred index (inclusive) in this dataset.
    pub start_index: u32,
    /// End shred index (inclusive) in this dataset.
    pub end_index: u32,
    /// True when this dataset carries the `LAST_SHRED_IN_SLOT` signal.
    pub last_in_slot: bool,
    /// Number of shreds included in this dataset.
    pub shreds: usize,
    /// Total payload bytes across shreds in this dataset.
    pub payload_len: usize,
    /// Number of decoded transactions in this dataset.
    pub tx_count: u64,
}

#[derive(Debug, Clone)]
/// Runtime event emitted for each decoded transaction.
///
/// # Examples
///
/// ```rust
/// use sof::framework::TransactionEvent;
///
/// fn signature_present(event: &TransactionEvent) -> bool {
///     event.signature.is_some()
/// }
/// ```
pub struct TransactionEvent {
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
    /// Decoded Solana transaction object.
    pub tx: Arc<VersionedTransaction>,
    /// SOF transaction kind classification.
    pub kind: TxKind,
}

#[derive(Debug, Clone)]
/// Runtime event emitted for one provider-stream websocket log notification.
///
/// This is intended for `logsSubscribe`-style feeds that can surface signatures
/// and log lines quickly but do not provide a full decoded transaction object.
///
/// # Examples
///
/// ```rust
/// use sof::framework::TransactionLogEvent;
///
/// fn signature(event: &TransactionLogEvent) -> solana_signature::Signature {
///     event.signature
/// }
/// ```
pub struct TransactionLogEvent {
    /// Slot context attached to the websocket log notification.
    pub slot: u64,
    /// Commitment status configured for the upstream websocket subscription.
    pub commitment_status: TxCommitmentStatus,
    /// Transaction signature carried by the log notification.
    pub signature: Signature,
    /// Transaction error payload when the upstream feed included one.
    pub err: Option<JsonValue>,
    /// Program/runtime log lines attached to the transaction.
    pub logs: Arc<[String]>,
    /// Matching pubkey for one `mentions`-style subscription when present.
    pub matched_filter: Option<Pubkey>,
}

#[derive(Debug, Clone)]
/// Runtime event emitted once per completed dataset with all decoded transactions.
///
/// # Examples
///
/// ```rust
/// use sof::framework::TransactionBatchEvent;
///
/// fn transaction_count(event: &TransactionBatchEvent) -> usize {
///     event.transactions.len()
/// }
/// ```
pub struct TransactionBatchEvent {
    /// Slot containing this dataset.
    pub slot: u64,
    /// Start shred index (inclusive) in this dataset.
    pub start_index: u32,
    /// End shred index (inclusive) in this dataset.
    pub end_index: u32,
    /// True when this dataset carries the `LAST_SHRED_IN_SLOT` signal.
    pub last_in_slot: bool,
    /// Number of shreds included in this dataset.
    pub shreds: usize,
    /// Total payload bytes across shreds in this dataset.
    pub payload_len: usize,
    /// Commitment status for this dataset slot when event was emitted.
    pub commitment_status: TxCommitmentStatus,
    /// Latest observed confirmed slot watermark when event was emitted.
    pub confirmed_slot: Option<u64>,
    /// Latest observed finalized slot watermark when event was emitted.
    pub finalized_slot: Option<u64>,
    /// All decoded transactions in dataset order.
    pub transactions: Arc<[VersionedTransaction]>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
/// One authoritative serialized transaction byte range inside a completed dataset payload.
pub struct SerializedTransactionRange {
    /// Start offset of the serialized transaction inside the contiguous dataset payload.
    start: u32,
    /// Exclusive end offset of the serialized transaction inside the contiguous dataset payload.
    end: u32,
}

impl SerializedTransactionRange {
    /// Creates one byte range with an exclusive end offset.
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Returns the start offset of the serialized transaction.
    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }

    /// Returns the exclusive end offset of the serialized transaction.
    #[must_use]
    pub const fn end(self) -> u32 {
        self.end
    }

    /// Returns the serialized transaction length in bytes.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// Returns true when the serialized transaction range is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

#[derive(Debug, Clone)]
/// Runtime event emitted once per completed dataset with authoritative serialized transaction views.
///
/// # Examples
///
/// ```rust
/// use sof::framework::TransactionViewBatchEvent;
///
/// fn first_transaction_len(event: &TransactionViewBatchEvent) -> Option<usize> {
///     event.transaction_bytes(0).map(|bytes| bytes.len())
/// }
/// ```
pub struct TransactionViewBatchEvent {
    /// Slot containing this dataset.
    pub slot: u64,
    /// Start shred index (inclusive) in this dataset.
    pub start_index: u32,
    /// End shred index (inclusive) in this dataset.
    pub end_index: u32,
    /// True when this dataset carries the `LAST_SHRED_IN_SLOT` signal.
    pub last_in_slot: bool,
    /// Number of shreds included in this dataset.
    pub shreds: usize,
    /// Total payload bytes across shreds in this dataset.
    pub payload_len: usize,
    /// Commitment status for this dataset slot when event was emitted.
    pub commitment_status: TxCommitmentStatus,
    /// Latest observed confirmed slot watermark when event was emitted.
    pub confirmed_slot: Option<u64>,
    /// Latest observed finalized slot watermark when event was emitted.
    pub finalized_slot: Option<u64>,
    /// Shared contiguous completed-dataset payload bytes.
    pub payload: Arc<[u8]>,
    /// Serialized transaction byte ranges in dataset order.
    pub transactions: Arc<[SerializedTransactionRange]>,
}

impl TransactionViewBatchEvent {
    /// Returns the number of serialized transactions in this completed dataset.
    #[must_use]
    pub fn len(&self) -> usize {
        self.transactions.len()
    }

    /// Returns true when the completed dataset contained no transactions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.transactions.is_empty()
    }

    /// Returns the serialized transaction bytes at one dataset position.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sof::framework::TransactionViewBatchEvent;
    ///
    /// fn first_transaction_bytes(event: &TransactionViewBatchEvent) -> Option<&[u8]> {
    ///     event.transaction_bytes(0)
    /// }
    /// ```
    #[must_use]
    pub fn transaction_bytes(&self, index: usize) -> Option<&[u8]> {
        let range = *self.transactions.get(index)?;
        let start = usize::try_from(range.start()).ok()?;
        let end = usize::try_from(range.end()).ok()?;
        self.payload.get(start..end)
    }

    /// Returns one iterator over sanitized authoritative transaction views in dataset order.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sof::framework::TransactionViewBatchEvent;
    ///
    /// fn count_valid_views(event: &TransactionViewBatchEvent) -> usize {
    ///     event.transaction_views().filter(|view| view.is_ok()).count()
    /// }
    /// ```
    pub fn transaction_views(
        &self,
    ) -> impl Iterator<Item = agave_transaction_view::result::Result<SanitizedTransactionView<&[u8]>>> + '_
    {
        self.transactions
            .iter()
            .filter_map(|range| {
                let start = usize::try_from(range.start()).ok()?;
                let end = usize::try_from(range.end()).ok()?;
                self.payload.get(start..end)
            })
            .map(|bytes| SanitizedTransactionView::try_new_sanitized(bytes, true))
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed runtime transaction view used for cheap hot-path preclassification.
pub struct TransactionEventRef<'event> {
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
    /// Borrowed decoded Solana transaction object.
    pub tx: &'event VersionedTransaction,
    /// SOF transaction kind classification.
    pub kind: TxKind,
}

impl TransactionEventRef<'_> {
    /// Materializes one owned transaction event only when downstream actually needs it.
    #[must_use]
    pub fn to_owned(&self) -> TransactionEvent {
        TransactionEvent {
            slot: self.slot,
            commitment_status: self.commitment_status,
            confirmed_slot: self.confirmed_slot,
            finalized_slot: self.finalized_slot,
            signature: self.signature,
            tx: Arc::new(self.tx.clone()),
            kind: self.kind,
        }
    }
}

#[derive(Debug, Clone)]
/// Runtime event emitted for each decoded transaction's touched account set.
pub struct AccountTouchEvent {
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
    /// All static message account keys present on the transaction.
    pub account_keys: Arc<[Pubkey]>,
    /// Writable static message account keys inferred from the versioned message header.
    pub writable_account_keys: Arc<[Pubkey]>,
    /// Read-only static message account keys inferred from the versioned message header.
    pub readonly_account_keys: Arc<[Pubkey]>,
    /// Lookup table account pubkeys referenced by the message itself.
    pub lookup_table_account_keys: Arc<[Pubkey]>,
}

#[derive(Debug, Clone, Copy)]
/// Borrowed transaction account-touch view used for cheap hot-path preclassification.
pub struct AccountTouchEventRef<'event> {
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
    /// Borrowed static message account keys present on the transaction.
    pub account_keys: &'event [Pubkey],
    /// Count of lookup-table account pubkeys referenced by the message itself.
    pub lookup_table_account_key_count: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
/// Runtime event emitted when local canonical classification for one slot changes.
pub struct SlotStatusEvent {
    /// Slot number whose status changed.
    pub slot: u64,
    /// Parent slot when known from data shreds.
    pub parent_slot: Option<u64>,
    /// Previous status for this slot (None when first tracked).
    pub previous_status: Option<ForkSlotStatus>,
    /// New status for this slot.
    pub status: ForkSlotStatus,
    /// Current canonical tip slot after applying this transition.
    pub tip_slot: Option<u64>,
    /// Current confirmed slot watermark.
    pub confirmed_slot: Option<u64>,
    /// Current finalized slot watermark.
    pub finalized_slot: Option<u64>,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
/// Runtime event emitted when local canonical tip switches to a different fork branch.
pub struct ReorgEvent {
    /// Previous local canonical tip slot.
    pub old_tip: u64,
    /// New local canonical tip slot.
    pub new_tip: u64,
    /// Lowest common ancestor between old and new tips when known.
    pub common_ancestor: Option<u64>,
    /// Slots detached from previous canonical branch (tip down to ancestor-exclusive).
    pub detached_slots: Vec<u64>,
    /// Slots attached from new canonical branch (ancestor-exclusive up to tip).
    pub attached_slots: Vec<u64>,
    /// Confirmed slot watermark after the switch.
    pub confirmed_slot: Option<u64>,
    /// Finalized slot watermark after the switch.
    pub finalized_slot: Option<u64>,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
/// Runtime event emitted when a newer observed recent blockhash is detected.
pub struct ObservedRecentBlockhashEvent {
    /// Slot where this recent blockhash was observed.
    pub slot: u64,
    /// Observed recent blockhash bytes.
    pub recent_blockhash: [u8; 32],
    /// Number of decoded transactions in the dataset that produced this observation.
    pub dataset_tx_count: u64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
/// Topology/leader event source.
pub enum ControlPlaneSource {
    /// Data gathered from gossip-bootstrap runtime state.
    GossipBootstrap,
    /// Data gathered from direct/relay runtime state.
    Direct,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
/// One known cluster node and its key advertised endpoints.
pub struct ClusterNodeInfo {
    /// Node identity.
    pub pubkey: Pubkey,
    /// Node wallclock from gossip contact info.
    pub wallclock: u64,
    /// Node shred version.
    pub shred_version: u16,
    /// Gossip endpoint when present.
    pub gossip: Option<SocketAddr>,
    /// TPU endpoint when present.
    pub tpu: Option<SocketAddr>,
    /// TPU QUIC endpoint when present.
    pub tpu_quic: Option<SocketAddr>,
    /// TPU-forwards endpoint when present.
    pub tpu_forwards: Option<SocketAddr>,
    /// TPU-forwards QUIC endpoint when present.
    pub tpu_forwards_quic: Option<SocketAddr>,
    /// TPU-vote endpoint when present.
    pub tpu_vote: Option<SocketAddr>,
    /// TVU endpoint when present.
    pub tvu: Option<SocketAddr>,
    /// RPC endpoint when present.
    pub rpc: Option<SocketAddr>,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
/// Low-frequency cluster topology update with diff + optional periodic snapshot.
pub struct ClusterTopologyEvent {
    /// Event source mode.
    pub source: ControlPlaneSource,
    /// Latest observed slot if known.
    pub slot: Option<u64>,
    /// Epoch if known (None when unavailable).
    pub epoch: Option<u64>,
    /// Active gossip entrypoint for this runtime instance.
    pub active_entrypoint: Option<String>,
    /// Number of nodes currently tracked in gossip.
    pub total_nodes: usize,
    /// Newly discovered nodes since previous event.
    pub added_nodes: Vec<ClusterNodeInfo>,
    /// Removed node identities since previous event.
    pub removed_pubkeys: Vec<Pubkey>,
    /// Existing nodes whose metadata/endpoints changed.
    pub updated_nodes: Vec<ClusterNodeInfo>,
    /// Periodic full snapshot of all currently known nodes.
    ///
    /// Empty for diff-only events.
    pub snapshot_nodes: Vec<ClusterNodeInfo>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
/// One leader assignment for a slot.
pub struct LeaderScheduleEntry {
    /// Slot number.
    pub slot: u64,
    /// Leader identity.
    pub leader: Pubkey,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
/// Event-driven leader-schedule update with diff payloads.
pub struct LeaderScheduleEvent {
    /// Event source mode.
    pub source: ControlPlaneSource,
    /// Latest observed slot if known.
    pub slot: Option<u64>,
    /// Epoch if known (None when unavailable).
    pub epoch: Option<u64>,
    /// Newly learned leader assignments.
    pub added_leaders: Vec<LeaderScheduleEntry>,
    /// Removed leader assignments keyed by slot.
    pub removed_slots: Vec<u64>,
    /// Existing assignments whose leader changed.
    pub updated_leaders: Vec<LeaderScheduleEntry>,
    /// Full snapshot of known leader assignments when emitted by a producer.
    ///
    /// Often empty for diff-only/event-driven updates.
    pub snapshot_leaders: Vec<LeaderScheduleEntry>,
}
