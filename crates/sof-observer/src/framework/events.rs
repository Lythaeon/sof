use std::net::SocketAddr;

use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;

use crate::{event::TxKind, shred::wire::ParsedShredHeader};

#[derive(Debug, Clone, Copy)]
/// Runtime event emitted for each ingress UDP packet before parsing.
pub struct RawPacketEvent<'packet> {
    /// Source socket address that delivered the packet.
    pub source: SocketAddr,
    /// Packet payload bytes as seen on the network.
    pub bytes: &'packet [u8],
}

#[derive(Debug, Clone, Copy)]
/// Runtime event emitted after a packet was parsed as a shred.
pub struct ShredEvent<'packet> {
    /// Source socket address that delivered the packet.
    pub source: SocketAddr,
    /// Original packet payload bytes.
    pub packet: &'packet [u8],
    /// Parsed shred header view for this packet.
    pub parsed: &'packet ParsedShredHeader,
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

#[derive(Debug, Clone, Copy)]
/// Runtime event emitted for each decoded transaction.
pub struct TransactionEvent<'tx> {
    /// Slot containing this transaction.
    pub slot: u64,
    /// Transaction signature if present.
    pub signature: Option<&'tx Signature>,
    /// Decoded Solana transaction object.
    pub tx: &'tx VersionedTransaction,
    /// SOF transaction kind classification.
    pub kind: TxKind,
}
