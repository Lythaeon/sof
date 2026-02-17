use std::{net::SocketAddr, sync::Arc};

use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;

use crate::{event::TxKind, shred::wire::ParsedShredHeader};

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
pub struct TransactionEvent {
    /// Slot containing this transaction.
    pub slot: u64,
    /// Transaction signature if present.
    pub signature: Option<Signature>,
    /// Decoded Solana transaction object.
    pub tx: Arc<VersionedTransaction>,
    /// SOF transaction kind classification.
    pub kind: TxKind,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
/// Topology/leader event source.
pub enum ControlPlaneSource {
    /// Data gathered from gossip-bootstrap runtime state.
    GossipBootstrap,
    /// Data gathered from direct/relay runtime state.
    Direct,
}

#[derive(Debug, Clone, Eq, PartialEq)]
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
    /// TVU endpoint when present.
    pub tvu: Option<SocketAddr>,
    /// RPC endpoint when present.
    pub rpc: Option<SocketAddr>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
/// One leader assignment for a slot.
pub struct LeaderScheduleEntry {
    /// Slot number.
    pub slot: u64,
    /// Leader identity.
    pub leader: Pubkey,
}

#[derive(Debug, Clone, Eq, PartialEq)]
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
