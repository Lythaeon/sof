//! Experimental derived-state feed types for official stateful extensions.
//!
//! This module is the first code scaffold for the architecture proposed in
//! `docs/architecture/adr/0010-dedicated-derived-state-feed.md`.
//! It defines the feed envelope, event families, checkpoints, and consumer-facing
//! fault types without yet wiring a runtime producer.

use std::{
    collections::HashMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use thiserror::Error;

use crate::{
    event::{ForkSlotStatus, TxCommitmentStatus, TxKind},
    framework::{
        AccountTouchEvent, ClusterTopologyEvent, LeaderScheduleEvent, ObservedRecentBlockhashEvent,
        ReorgEvent, SlotStatusEvent, TransactionEvent,
    },
};

/// One runtime feed session identity.
///
/// A new session id is expected whenever feed continuity cannot be assumed across process
/// lifetimes or replay sources.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct FeedSessionId(pub u128);

/// Monotonic sequence number for the derived-state feed within one session.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct FeedSequence(pub u64);

impl FeedSequence {
    /// Returns the next sequence value when `u64` space has not been exhausted.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// Runtime truth watermarks visible to derived-state consumers.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FeedWatermarks {
    /// Current canonical tip slot.
    pub canonical_tip_slot: Option<u64>,
    /// Highest processed slot visible to the runtime.
    pub processed_slot: Option<u64>,
    /// Highest confirmed slot visible to the runtime.
    pub confirmed_slot: Option<u64>,
    /// Highest finalized slot visible to the runtime.
    pub finalized_slot: Option<u64>,
}

impl FeedWatermarks {
    /// Computes the current transaction commitment classification for one slot.
    #[must_use]
    pub fn commitment_for_slot(self, slot: u64) -> TxCommitmentStatus {
        TxCommitmentStatus::from_slot(slot, self.confirmed_slot, self.finalized_slot)
    }

    /// Builds watermark state from a slot-status transition event.
    #[must_use]
    pub const fn from_slot_status(event: SlotStatusEvent) -> Self {
        Self {
            canonical_tip_slot: event.tip_slot,
            processed_slot: match event.status {
                ForkSlotStatus::Processed => Some(event.slot),
                ForkSlotStatus::Confirmed
                | ForkSlotStatus::Finalized
                | ForkSlotStatus::Orphaned => event.tip_slot,
            },
            confirmed_slot: event.confirmed_slot,
            finalized_slot: event.finalized_slot,
        }
    }

    /// Builds watermark state from a reorg event.
    #[must_use]
    pub const fn from_reorg(event: &ReorgEvent) -> Self {
        Self {
            canonical_tip_slot: Some(event.new_tip),
            processed_slot: Some(event.new_tip),
            confirmed_slot: event.confirmed_slot,
            finalized_slot: event.finalized_slot,
        }
    }
}

/// One envelope delivered to a derived-state consumer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedStateFeedEnvelope {
    /// Feed session identity.
    pub session_id: FeedSessionId,
    /// Monotonic sequence within the session.
    pub sequence: FeedSequence,
    /// Wall-clock time when the runtime emitted the envelope.
    pub emitted_at: SystemTime,
    /// Runtime truth watermarks at emission time.
    pub watermarks: FeedWatermarks,
    /// Derived-state event payload.
    pub event: DerivedStateFeedEvent,
}

/// Event families intended for authoritative stateful consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DerivedStateFeedEvent {
    /// Decoded transaction apply record.
    TransactionApplied(TransactionAppliedEvent),
    /// Recent blockhash observation suitable for direct-submit consumers.
    RecentBlockhashObserved(ObservedRecentBlockhashEvent),
    /// Cluster topology diff/snapshot suitable for direct-submit consumers.
    ClusterTopologyChanged(ClusterTopologyEvent),
    /// Leader schedule diff/snapshot suitable for direct-submit consumers.
    LeaderScheduleUpdated(LeaderScheduleEvent),
    /// Slot lifecycle transition.
    SlotStatusChanged(SlotStatusChangedEvent),
    /// Canonical branch switch requiring consumer rollback/reconciliation.
    BranchReorged(BranchReorgedEvent),
    /// Transaction-derived account-touch metadata.
    AccountTouchObserved(AccountTouchObservedEvent),
    /// Consumer checkpoint barrier.
    CheckpointBarrier(CheckpointBarrierEvent),
}

/// Decoded transaction apply record for the derived-state feed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionAppliedEvent {
    /// Slot containing the transaction.
    pub slot: u64,
    /// Transaction position within the canonical derived-state stream for the slot.
    pub tx_index: u32,
    /// Transaction signature when present.
    pub signature: Option<Signature>,
    /// Transaction kind classification.
    pub kind: TxKind,
    /// Decoded versioned transaction payload.
    pub transaction: Arc<VersionedTransaction>,
    /// Commitment status at emission time.
    pub commitment_status: TxCommitmentStatus,
}

impl From<(u32, TransactionEvent)> for TransactionAppliedEvent {
    fn from((tx_index, event): (u32, TransactionEvent)) -> Self {
        Self {
            slot: event.slot,
            tx_index,
            signature: event.signature,
            kind: event.kind,
            transaction: event.tx,
            commitment_status: event.commitment_status,
        }
    }
}

/// Slot lifecycle transition record for the derived-state feed.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub struct SlotStatusChangedEvent {
    /// Slot whose state changed.
    pub slot: u64,
    /// Parent slot when known.
    pub parent_slot: Option<u64>,
    /// Previous status when known.
    pub previous_status: Option<ForkSlotStatus>,
    /// New runtime-visible status.
    pub status: ForkSlotStatus,
}

impl From<SlotStatusEvent> for SlotStatusChangedEvent {
    fn from(event: SlotStatusEvent) -> Self {
        Self {
            slot: event.slot,
            parent_slot: event.parent_slot,
            previous_status: event.previous_status,
            status: event.status,
        }
    }
}

/// Canonical branch switch record for the derived-state feed.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct BranchReorgedEvent {
    /// Previous canonical tip.
    pub old_tip: u64,
    /// New canonical tip.
    pub new_tip: u64,
    /// Lowest common ancestor when known.
    pub common_ancestor: Option<u64>,
    /// Slots detached from the old branch.
    pub detached_slots: Arc<[u64]>,
    /// Slots attached from the new branch.
    pub attached_slots: Arc<[u64]>,
}

impl From<ReorgEvent> for BranchReorgedEvent {
    fn from(event: ReorgEvent) -> Self {
        Self {
            old_tip: event.old_tip,
            new_tip: event.new_tip,
            common_ancestor: event.common_ancestor,
            detached_slots: Arc::from(event.detached_slots),
            attached_slots: Arc::from(event.attached_slots),
        }
    }
}

/// Transaction-derived account-touch metadata for stateful consumers.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountTouchObservedEvent {
    /// Slot containing the transaction.
    pub slot: u64,
    /// Transaction position within the canonical derived-state stream for the slot.
    pub tx_index: u32,
    /// Transaction signature when present.
    pub signature: Option<Signature>,
    /// All static message account keys touched by the transaction.
    pub account_keys: Arc<Vec<Pubkey>>,
    /// Writable static account keys touched by the transaction.
    pub writable_account_keys: Arc<Vec<Pubkey>>,
    /// Read-only static account keys touched by the transaction.
    pub readonly_account_keys: Arc<Vec<Pubkey>>,
    /// Lookup-table account keys referenced by the transaction.
    pub lookup_table_account_keys: Arc<Vec<Pubkey>>,
}

impl From<(u32, AccountTouchEvent)> for AccountTouchObservedEvent {
    fn from((tx_index, event): (u32, AccountTouchEvent)) -> Self {
        Self {
            slot: event.slot,
            tx_index,
            signature: event.signature,
            account_keys: event.account_keys,
            writable_account_keys: event.writable_account_keys,
            readonly_account_keys: event.readonly_account_keys,
            lookup_table_account_keys: event.lookup_table_account_keys,
        }
    }
}

/// Checkpoint barrier emitted by the derived-state feed.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub struct CheckpointBarrierEvent {
    /// Highest contiguous sequence fully covered by the barrier.
    pub barrier_sequence: FeedSequence,
    /// Why the barrier was emitted.
    pub reason: CheckpointBarrierReason,
}

/// Reasons for emitting a checkpoint barrier.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum CheckpointBarrierReason {
    /// Periodic background checkpoint opportunity.
    Periodic,
    /// Runtime is beginning graceful shutdown.
    ShutdownRequested,
    /// Replay catch-up hit a stable boundary.
    ReplayBoundary,
}

/// Durable checkpoint shape for one derived-state consumer.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct DerivedStateCheckpoint {
    /// Feed session against which this checkpoint was created.
    pub session_id: FeedSessionId,
    /// Highest contiguous sequence fully applied by the consumer.
    pub last_applied_sequence: FeedSequence,
    /// Runtime truth watermarks at the checkpoint boundary.
    pub watermarks: FeedWatermarks,
    /// Consumer-owned schema/state version.
    pub state_version: u32,
    /// Consumer implementation version string.
    pub extension_version: String,
}

impl DerivedStateCheckpoint {
    /// Returns the next sequence the consumer should request or apply.
    #[must_use]
    pub const fn next_sequence(&self) -> Option<FeedSequence> {
        self.last_applied_sequence.next()
    }
}

/// Structured fault categories for authoritative derived-state consumers.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DerivedStateConsumerFaultKind {
    /// Consumer lag exceeded the runtime policy threshold.
    LagExceeded,
    /// Consumer queue overflowed and live continuity was lost.
    QueueOverflow,
    /// Consumer failed to durably persist a checkpoint.
    CheckpointWriteFailed,
    /// Replay source could not provide the required contiguous sequence range.
    ReplayGap,
    /// Consumer failed to apply one envelope.
    ConsumerApplyFailed,
}

impl DerivedStateConsumerFaultKind {
    /// Returns whether this fault breaks live continuity for the affected consumer.
    #[must_use]
    pub const fn breaks_live_continuity(self) -> bool {
        matches!(
            self,
            Self::LagExceeded | Self::QueueOverflow | Self::ReplayGap | Self::ConsumerApplyFailed
        )
    }

    /// Returns the compact storage representation used in atomics.
    #[must_use]
    const fn into_u8(self) -> u8 {
        match self {
            Self::LagExceeded => 0,
            Self::QueueOverflow => 1,
            Self::CheckpointWriteFailed => 2,
            Self::ReplayGap => 3,
            Self::ConsumerApplyFailed => 4,
        }
    }

    /// Decodes a compact storage representation used in atomics.
    #[must_use]
    const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::QueueOverflow,
            2 => Self::CheckpointWriteFailed,
            3 => Self::ReplayGap,
            4 => Self::ConsumerApplyFailed,
            _ => Self::LagExceeded,
        }
    }
}

/// Structured consumer fault returned by the feed scaffold.
#[derive(Debug, Clone, Eq, PartialEq, Error)]
#[error("{kind:?}: {message}")]
pub struct DerivedStateConsumerFault {
    /// Fault category.
    pub kind: DerivedStateConsumerFaultKind,
    /// Last relevant feed sequence when known.
    pub sequence: Option<FeedSequence>,
    /// Diagnostic context for operators and tests.
    pub message: String,
}

impl DerivedStateConsumerFault {
    /// Creates a new structured fault with free-form diagnostic context.
    #[must_use]
    pub fn new(
        kind: DerivedStateConsumerFaultKind,
        sequence: Option<FeedSequence>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            sequence,
            message: message.into(),
        }
    }
}

/// Snapshot of one registered derived-state consumer's live-feed health and counters.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DerivedStateConsumerTelemetry {
    /// Stable consumer name used in logs and telemetry.
    pub name: &'static str,
    /// Whether live continuity has been lost for this consumer.
    pub unhealthy: bool,
    /// Recovery state for the consumer.
    pub recovery_state: DerivedStateConsumerRecoveryState,
    /// Total number of successfully applied envelopes.
    pub applied_events: u64,
    /// Total number of successfully flushed checkpoints.
    pub checkpoint_flushes: u64,
    /// Total number of structured faults recorded for the consumer.
    pub fault_count: u64,
    /// Highest applied sequence when known.
    pub last_applied_sequence: Option<FeedSequence>,
    /// Highest fault-associated sequence when known.
    pub last_fault_sequence: Option<FeedSequence>,
    /// Last structured fault kind when known.
    pub last_fault_kind: Option<DerivedStateConsumerFaultKind>,
}

/// Recovery state for one derived-state consumer.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DerivedStateConsumerRecoveryState {
    /// Consumer is live and receiving feed events.
    Live,
    /// Consumer should attempt replay-based recovery from its durable checkpoint.
    ReplayRecoveryPending,
    /// Consumer cannot be recovered from the retained replay tail and needs a rebuild.
    RebuildRequired,
}

impl DerivedStateConsumerRecoveryState {
    /// Returns the compact storage representation used in atomics.
    #[must_use]
    const fn into_u8(self) -> u8 {
        match self {
            Self::Live => 0,
            Self::ReplayRecoveryPending => 1,
            Self::RebuildRequired => 2,
        }
    }

    /// Decodes a compact storage representation used in atomics.
    #[must_use]
    const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::ReplayRecoveryPending,
            2 => Self::RebuildRequired,
            _ => Self::Live,
        }
    }
}

/// Replay backend telemetry snapshot exposed to runtime logs and tests.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct DerivedStateReplayTelemetry {
    /// Stable backend identifier.
    pub backend: DerivedStateReplayBackend,
    /// Number of retained sessions visible to the backend.
    pub retained_sessions: usize,
    /// Number of retained envelopes visible to the backend.
    pub retained_envelopes: usize,
    /// Number of envelopes truncated by retention policy.
    pub truncated_envelopes: u64,
    /// Number of backend append failures.
    pub append_failures: u64,
    /// Number of backend load/decode failures.
    pub load_failures: u64,
    /// Number of compaction runs performed by the backend.
    pub compactions: u64,
}

/// Backend used for retained derived-state replay.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum DerivedStateReplayBackend {
    /// In-process retained feed tail.
    #[default]
    Memory,
    /// Disk-backed retained feed tail.
    Disk,
}

impl DerivedStateReplayBackend {
    /// Returns the stable env/config representation for the backend.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Disk => "disk",
        }
    }

    /// Parses one env/config value into a backend.
    #[must_use]
    pub const fn from_config_value(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("memory") {
            Some(Self::Memory)
        } else if value.eq_ignore_ascii_case("disk") {
            Some(Self::Disk)
        } else {
            None
        }
    }
}

impl fmt::Display for DerivedStateReplayBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Recovery attempt summary returned by the derived-state host.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct DerivedStateRecoveryReport {
    /// Number of unhealthy consumers considered for recovery.
    pub attempted: u64,
    /// Number of consumers returned to live state.
    pub recovered: u64,
    /// Number of consumers still waiting for replay-based recovery.
    pub still_pending: u64,
    /// Number of consumers that require a full rebuild.
    pub rebuild_required: u64,
}

/// Replay errors returned by derived-state feed sources.
#[derive(Debug, Clone, Eq, PartialEq, Error)]
pub enum DerivedStateReplayError {
    /// No retained session was available for the requested checkpoint.
    #[error("replay session unavailable: {0:?}")]
    SessionUnavailable(FeedSessionId),
    /// The retained feed did not contain the expected contiguous next sequence.
    #[error("replay sequence gap at {sequence:?}")]
    SequenceGap {
        /// Session that contained the gap.
        session_id: FeedSessionId,
        /// First missing or mismatched sequence.
        sequence: FeedSequence,
    },
    /// The replay source truncated older envelopes before the requested sequence.
    #[error("replay truncated before {sequence:?}")]
    Truncated {
        /// Session that retained only a later tail.
        session_id: FeedSessionId,
        /// First requested sequence that was no longer retained.
        sequence: FeedSequence,
        /// Oldest sequence still retained for the session.
        oldest_retained_sequence: FeedSequence,
    },
    /// The replay backend could not load or decode the retained feed.
    #[error("replay backend failure for {session_id:?}: {message}")]
    BackendFailure {
        /// Session whose retained log could not be loaded.
        session_id: FeedSessionId,
        /// Free-form backend diagnostic.
        message: String,
    },
}

/// Ordered replay source for retained derived-state feed envelopes.
pub trait DerivedStateReplaySource: Send + Sync + 'static {
    /// Records one emitted envelope into the replay source.
    fn append(&self, envelope: DerivedStateFeedEnvelope);

    /// Returns retained envelopes starting at the requested sequence boundary.
    ///
    /// # Errors
    /// Returns a replay error when the retained stream cannot satisfy continuity.
    fn replay_from(
        &self,
        session_id: FeedSessionId,
        next_sequence: FeedSequence,
    ) -> Result<Vec<DerivedStateFeedEnvelope>, DerivedStateReplayError>;

    /// Returns operator-facing telemetry for this replay backend.
    #[must_use]
    fn telemetry(&self) -> DerivedStateReplayTelemetry {
        DerivedStateReplayTelemetry::default()
    }
}

/// In-memory replay source used by the scaffold and tests.
#[derive(Default)]
pub struct InMemoryDerivedStateReplaySource {
    /// Retained feed envelopes grouped by session id.
    sessions: Mutex<HashMap<FeedSessionId, Vec<DerivedStateFeedEnvelope>>>,
    /// Optional bounded retention policy applied per session.
    max_envelopes_per_session: Option<usize>,
    /// Total number of envelopes truncated by the retention policy.
    truncated_envelopes: AtomicU64,
}

impl InMemoryDerivedStateReplaySource {
    /// Creates an empty in-memory replay source.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            max_envelopes_per_session: None,
            truncated_envelopes: AtomicU64::new(0),
        }
    }

    /// Creates an in-memory replay source with bounded per-session retention.
    #[must_use]
    pub fn with_max_envelopes_per_session(max_envelopes_per_session: usize) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            max_envelopes_per_session: Some(max_envelopes_per_session),
            truncated_envelopes: AtomicU64::new(0),
        }
    }

    /// Returns the total number of envelopes truncated across all sessions.
    #[must_use]
    pub fn truncated_envelopes(&self) -> u64 {
        self.truncated_envelopes.load(Ordering::Relaxed)
    }

    /// Returns the number of envelopes currently retained for one session.
    #[must_use]
    pub fn retained_envelopes(&self, session_id: FeedSessionId) -> usize {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(&session_id).map(Vec::len))
            .unwrap_or(0)
    }
}

impl DerivedStateReplaySource for InMemoryDerivedStateReplaySource {
    fn append(&self, envelope: DerivedStateFeedEnvelope) {
        if let Ok(mut sessions) = self.sessions.lock() {
            let retained = sessions.entry(envelope.session_id).or_default();
            retained.push(envelope);
            if let Some(max_envelopes_per_session) = self.max_envelopes_per_session {
                let truncated = retained.len().saturating_sub(max_envelopes_per_session);
                if truncated > 0 {
                    retained.drain(..truncated);
                    let _ = self
                        .truncated_envelopes
                        .fetch_add(truncated as u64, Ordering::Relaxed);
                }
            }
        }
    }

    fn replay_from(
        &self,
        session_id: FeedSessionId,
        next_sequence: FeedSequence,
    ) -> Result<Vec<DerivedStateFeedEnvelope>, DerivedStateReplayError> {
        let Ok(sessions) = self.sessions.lock() else {
            return Err(DerivedStateReplayError::SessionUnavailable(session_id));
        };
        let Some(envelopes) = sessions.get(&session_id) else {
            return Err(DerivedStateReplayError::SessionUnavailable(session_id));
        };
        validate_replayed_envelopes(session_id, envelopes, next_sequence)
    }

    fn telemetry(&self) -> DerivedStateReplayTelemetry {
        let (retained_sessions, retained_envelopes) = self
            .sessions
            .lock()
            .map(|sessions| {
                let retained_sessions = sessions.len();
                let retained_envelopes = sessions.values().map(Vec::len).sum();
                (retained_sessions, retained_envelopes)
            })
            .unwrap_or((0, 0));
        DerivedStateReplayTelemetry {
            backend: DerivedStateReplayBackend::Memory,
            retained_sessions,
            retained_envelopes,
            truncated_envelopes: self.truncated_envelopes(),
            append_failures: 0,
            load_failures: 0,
            compactions: 0,
        }
    }
}

/// Disk-backed replay source for retained derived-state feed envelopes.
pub struct DiskDerivedStateReplaySource {
    /// Root directory that stores one retained log per session.
    root_dir: PathBuf,
    /// Bounded retention policy applied per session.
    max_envelopes_per_session: usize,
    /// Maximum number of retained session logs on disk.
    max_retained_sessions: usize,
    /// Durability policy used for disk writes.
    durability: DerivedStateReplayDurability,
    /// Retained feed envelopes loaded in-process by session id.
    sessions: Mutex<HashMap<FeedSessionId, Vec<DerivedStateFeedEnvelope>>>,
    /// Total number of envelopes truncated by the retention policy.
    truncated_envelopes: AtomicU64,
    /// Total number of backend append failures.
    append_failures: AtomicU64,
    /// Total number of backend load failures.
    load_failures: AtomicU64,
    /// Total number of disk compaction runs.
    compactions: AtomicU64,
}

/// Durability policy for the disk-backed replay source.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum DerivedStateReplayDurability {
    /// Flush buffered writes to the OS before returning.
    #[default]
    Flush,
    /// Flush buffered writes and issue `fsync`/`sync_all`.
    Fsync,
}

impl DerivedStateReplayDurability {
    /// Returns the stable env/config representation for the durability mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Flush => "flush",
            Self::Fsync => "fsync",
        }
    }

    /// Parses one env/config value into a durability mode.
    #[must_use]
    pub const fn from_config_value(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("flush") {
            Some(Self::Flush)
        } else if value.eq_ignore_ascii_case("fsync") {
            Some(Self::Fsync)
        } else {
            None
        }
    }
}

impl fmt::Display for DerivedStateReplayDurability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl DiskDerivedStateReplaySource {
    /// Creates a disk-backed replay source rooted at one directory.
    ///
    /// # Errors
    /// Returns an IO error when the replay directory cannot be created.
    pub fn new(root_dir: impl Into<PathBuf>, max_envelopes_per_session: usize) -> io::Result<Self> {
        Self::with_policy(
            root_dir,
            max_envelopes_per_session,
            4,
            DerivedStateReplayDurability::Flush,
        )
    }

    /// Creates a disk-backed replay source with explicit retention and durability policy.
    ///
    /// # Errors
    /// Returns an IO error when the replay directory cannot be created.
    pub fn with_policy(
        root_dir: impl Into<PathBuf>,
        max_envelopes_per_session: usize,
        max_retained_sessions: usize,
        durability: DerivedStateReplayDurability,
    ) -> io::Result<Self> {
        let root_dir = root_dir.into();
        fs::create_dir_all(&root_dir)?;
        Ok(Self {
            root_dir,
            max_envelopes_per_session,
            max_retained_sessions: max_retained_sessions.max(1),
            durability,
            sessions: Mutex::new(HashMap::new()),
            truncated_envelopes: AtomicU64::new(0),
            append_failures: AtomicU64::new(0),
            load_failures: AtomicU64::new(0),
            compactions: AtomicU64::new(0),
        })
    }

    /// Returns the replay directory root.
    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    /// Returns the total number of envelopes truncated across all sessions.
    #[must_use]
    pub fn truncated_envelopes(&self) -> u64 {
        self.truncated_envelopes.load(Ordering::Relaxed)
    }

    /// Returns the total number of append failures observed by the backend.
    #[must_use]
    pub fn append_failures(&self) -> u64 {
        self.append_failures.load(Ordering::Relaxed)
    }

    /// Returns the total number of load/decode failures observed by the backend.
    #[must_use]
    pub fn load_failures(&self) -> u64 {
        self.load_failures.load(Ordering::Relaxed)
    }

    /// Returns the total number of compaction runs observed by the backend.
    #[must_use]
    pub fn compactions(&self) -> u64 {
        self.compactions.load(Ordering::Relaxed)
    }

    /// Returns the number of envelopes currently retained for one session.
    #[must_use]
    pub fn retained_envelopes(&self, session_id: FeedSessionId) -> usize {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(&session_id).map(Vec::len))
            .unwrap_or_else(|| {
                self.load_session_from_disk(session_id)
                    .map_or(0, |envelopes| envelopes.len())
            })
    }

    /// Returns the file path for one retained session log.
    fn session_path(&self, session_id: FeedSessionId) -> PathBuf {
        self.root_dir.join(format!("{:032x}.replay", session_id.0))
    }

    /// Serializes one feed envelope into an on-disk record payload.
    fn encode_envelope(envelope: &DerivedStateFeedEnvelope) -> io::Result<Vec<u8>> {
        bincode::serialize(envelope)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
    }

    /// Deserializes one feed envelope from an on-disk record payload.
    fn decode_envelope(bytes: &[u8]) -> io::Result<DerivedStateFeedEnvelope> {
        bincode::deserialize(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
    }

    /// Appends one length-prefixed record to an existing session log.
    fn append_record(&self, path: &Path, envelope: &DerivedStateFeedEnvelope) -> io::Result<()> {
        let encoded = Self::encode_envelope(envelope)?;
        let encoded_len = u32::try_from(encoded.len()).map_err(|_error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "derived-state replay envelope exceeded u32 record length",
            )
        })?;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(&encoded_len.to_le_bytes())?;
        file.write_all(&encoded)?;
        file.flush()?;
        self.sync_file(&file)?;
        Ok(())
    }

    /// Rewrites one retained session log from the currently in-memory tail.
    fn rewrite_records(
        &self,
        path: &Path,
        envelopes: &[DerivedStateFeedEnvelope],
    ) -> io::Result<()> {
        let temp_path = path.with_extension("replay.tmp");
        {
            let mut file = File::create(&temp_path)?;
            for envelope in envelopes {
                let encoded = Self::encode_envelope(envelope)?;
                let encoded_len = u32::try_from(encoded.len()).map_err(|_error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "derived-state replay envelope exceeded u32 record length",
                    )
                })?;
                file.write_all(&encoded_len.to_le_bytes())?;
                file.write_all(&encoded)?;
            }
            file.flush()?;
            self.sync_file(&file)?;
        }
        fs::rename(temp_path, path)?;
        if let DerivedStateReplayDurability::Fsync = self.durability {
            let directory = File::open(&self.root_dir)?;
            directory.sync_all()?;
        }
        Ok(())
    }

    /// Loads one retained session log from disk into memory.
    fn load_session_from_disk(
        &self,
        session_id: FeedSessionId,
    ) -> io::Result<Vec<DerivedStateFeedEnvelope>> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut file = File::open(path)?;
        let mut envelopes = Vec::new();
        loop {
            let mut length_bytes = [0_u8; 4];
            match file.read_exact(&mut length_bytes) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error),
            }
            let encoded_len = u32::from_le_bytes(length_bytes);
            let mut encoded = vec![0_u8; encoded_len as usize];
            file.read_exact(&mut encoded)?;
            envelopes.push(Self::decode_envelope(&encoded)?);
        }
        Ok(envelopes)
    }

    /// Returns the number of retained session files currently visible on disk.
    fn retained_session_files(&self) -> usize {
        fs::read_dir(&self.root_dir)
            .ok()
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "replay"))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Returns one parsed session id from a retained replay filename when possible.
    fn parse_session_id(path: &Path) -> Option<FeedSessionId> {
        let stem = path.file_stem()?.to_str()?;
        u128::from_str_radix(stem, 16).ok().map(FeedSessionId)
    }

    /// Ensures one session tail is resident in the in-process cache.
    fn load_session(
        &self,
        sessions: &mut HashMap<FeedSessionId, Vec<DerivedStateFeedEnvelope>>,
        session_id: FeedSessionId,
    ) -> io::Result<()> {
        if sessions.contains_key(&session_id) {
            return Ok(());
        }
        let loaded = self.load_session_from_disk(session_id)?;
        sessions.insert(session_id, loaded);
        Ok(())
    }

    /// Applies the configured durability policy to one open file handle.
    fn sync_file(&self, file: &File) -> io::Result<()> {
        match self.durability {
            DerivedStateReplayDurability::Flush => Ok(()),
            DerivedStateReplayDurability::Fsync => file.sync_all(),
        }
    }

    /// Compacts old session logs when the backend exceeds its retention budget.
    fn compact_sessions(
        &self,
        sessions: &mut HashMap<FeedSessionId, Vec<DerivedStateFeedEnvelope>>,
        current_session_id: FeedSessionId,
    ) -> io::Result<()> {
        let mut retained_sessions = fs::read_dir(&self.root_dir)?
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                (path.extension().is_some_and(|ext| ext == "replay"))
                    .then(|| Self::parse_session_id(&path).map(|session_id| (session_id, path)))
                    .flatten()
            })
            .collect::<Vec<_>>();
        retained_sessions.sort_by_key(|(session_id, _path)| *session_id);
        let mut removed_any = false;
        while retained_sessions.len() > self.max_retained_sessions {
            let Some((session_id, path)) = retained_sessions.first().cloned() else {
                break;
            };
            if session_id == current_session_id {
                break;
            }
            fs::remove_file(&path)?;
            let _ = sessions.remove(&session_id);
            retained_sessions.remove(0);
            removed_any = true;
        }
        if removed_any {
            let _ = self.compactions.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

impl DerivedStateReplaySource for DiskDerivedStateReplaySource {
    fn append(&self, envelope: DerivedStateFeedEnvelope) {
        let session_id = envelope.session_id;
        let path = self.session_path(session_id);
        let append_result = (|| -> io::Result<()> {
            let Ok(mut sessions) = self.sessions.lock() else {
                return Err(io::Error::other(
                    "derived-state replay session mutex poisoned during append",
                ));
            };
            self.load_session(&mut sessions, session_id)?;
            let retained = sessions.entry(session_id).or_default();
            retained.push(envelope.clone());
            let truncated = retained
                .len()
                .saturating_sub(self.max_envelopes_per_session.max(1));
            if truncated > 0 {
                retained.drain(..truncated);
                let _ = self
                    .truncated_envelopes
                    .fetch_add(truncated as u64, Ordering::Relaxed);
                self.rewrite_records(&path, retained)?;
            } else {
                self.append_record(&path, &envelope)?;
            }
            self.compact_sessions(&mut sessions, session_id)
        })();
        if let Err(error) = append_result {
            let _ = self.append_failures.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                session_id = ?session_id,
                path = %path.display(),
                error = %error,
                "failed to append derived-state replay envelope to disk"
            );
        }
    }

    fn replay_from(
        &self,
        session_id: FeedSessionId,
        next_sequence: FeedSequence,
    ) -> Result<Vec<DerivedStateFeedEnvelope>, DerivedStateReplayError> {
        let Ok(mut sessions) = self.sessions.lock() else {
            return Err(DerivedStateReplayError::BackendFailure {
                session_id,
                message: "derived-state replay session mutex poisoned during replay".to_owned(),
            });
        };
        self.load_session(&mut sessions, session_id)
            .map_err(|error| {
                let _ = self.load_failures.fetch_add(1, Ordering::Relaxed);
                DerivedStateReplayError::BackendFailure {
                    session_id,
                    message: error.to_string(),
                }
            })?;
        let Some(envelopes) = sessions.get(&session_id) else {
            return Err(DerivedStateReplayError::SessionUnavailable(session_id));
        };
        if envelopes.is_empty() {
            return Err(DerivedStateReplayError::SessionUnavailable(session_id));
        }
        validate_replayed_envelopes(session_id, envelopes, next_sequence)
    }

    fn telemetry(&self) -> DerivedStateReplayTelemetry {
        let (retained_sessions, retained_envelopes) = self
            .sessions
            .lock()
            .map(|sessions| {
                let retained_envelopes = sessions.values().map(Vec::len).sum();
                (
                    self.retained_session_files().max(sessions.len()),
                    retained_envelopes,
                )
            })
            .unwrap_or_else(|_poison| (self.retained_session_files(), 0));
        DerivedStateReplayTelemetry {
            backend: DerivedStateReplayBackend::Disk,
            retained_sessions,
            retained_envelopes,
            truncated_envelopes: self.truncated_envelopes(),
            append_failures: self.append_failures(),
            load_failures: self.load_failures(),
            compactions: self.compactions(),
        }
    }
}

/// Validates that one retained session tail can satisfy replay continuity.
fn validate_replayed_envelopes(
    session_id: FeedSessionId,
    envelopes: &[DerivedStateFeedEnvelope],
    next_sequence: FeedSequence,
) -> Result<Vec<DerivedStateFeedEnvelope>, DerivedStateReplayError> {
    if let Some(last_retained_sequence) = envelopes.last().map(|envelope| envelope.sequence)
        && last_retained_sequence
            .next()
            .is_some_and(|expected_next| next_sequence == expected_next)
    {
        return Ok(Vec::new());
    }
    if let Some(oldest_retained_sequence) = envelopes.first().map(|envelope| envelope.sequence)
        && next_sequence < oldest_retained_sequence
    {
        return Err(DerivedStateReplayError::Truncated {
            session_id,
            sequence: next_sequence,
            oldest_retained_sequence,
        });
    }
    let Some(start_index) = envelopes
        .iter()
        .position(|envelope| envelope.sequence == next_sequence)
    else {
        if envelopes.is_empty() && next_sequence == FeedSequence(0) {
            return Ok(Vec::new());
        }
        return Err(DerivedStateReplayError::SequenceGap {
            session_id,
            sequence: next_sequence,
        });
    };
    let Some(replayed) = envelopes.get(start_index..) else {
        return Err(DerivedStateReplayError::SequenceGap {
            session_id,
            sequence: next_sequence,
        });
    };
    let replayed = replayed.to_vec();
    let mut expected_sequence = next_sequence;
    for envelope in &replayed {
        if envelope.sequence != expected_sequence {
            return Err(DerivedStateReplayError::SequenceGap {
                session_id,
                sequence: expected_sequence,
            });
        }
        let Some(next) = expected_sequence.next() else {
            break;
        };
        expected_sequence = next;
    }
    Ok(replayed)
}

/// Stateful consumer interface for the dedicated derived-state feed scaffold.
///
/// This trait is intentionally synchronous for the initial scaffold so implementers can model
/// deterministic state application and checkpointing before runtime dispatch details are fixed.
pub trait DerivedStateConsumer: Send + Sync + 'static {
    /// Stable consumer name used in logs and telemetry.
    fn name(&self) -> &'static str;

    /// Consumer-owned schema/state version written into durable checkpoints.
    fn state_version(&self) -> u32;

    /// Stable consumer implementation version written into durable checkpoints.
    fn extension_version(&self) -> &'static str;

    /// Loads the most recent durable checkpoint when present.
    ///
    /// # Errors
    /// Returns a structured fault when the checkpoint cannot be loaded or decoded.
    fn load_checkpoint(
        &mut self,
    ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault>;

    /// Applies one feed envelope in canonical sequence order.
    ///
    /// # Errors
    /// Returns a structured fault when the consumer cannot apply the event.
    fn apply(
        &mut self,
        envelope: DerivedStateFeedEnvelope,
    ) -> Result<(), DerivedStateConsumerFault>;

    /// Persists a durable checkpoint for later replay or recovery.
    ///
    /// # Errors
    /// Returns a structured fault when checkpoint persistence fails.
    fn flush_checkpoint(
        &mut self,
        checkpoint: DerivedStateCheckpoint,
    ) -> Result<(), DerivedStateConsumerFault>;
}

/// Builder for [`DerivedStateHost`].
#[derive(Default)]
pub struct DerivedStateHostBuilder {
    /// Registered consumers in dispatch order.
    consumers: Vec<RegisteredDerivedStateConsumer>,
    /// Optional retained replay source used during checkpoint resume.
    replay_source: Option<Arc<dyn DerivedStateReplaySource>>,
    /// Explicit session id override for replay/testing flows.
    session_id: Option<FeedSessionId>,
}

impl DerivedStateHostBuilder {
    /// Creates an empty host builder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            consumers: Vec::new(),
            replay_source: None,
            session_id: None,
        }
    }

    /// Registers one derived-state consumer.
    #[must_use]
    pub fn add_consumer<C>(mut self, consumer: C) -> Self
    where
        C: DerivedStateConsumer,
    {
        let name = consumer.name();
        self.consumers.push(RegisteredDerivedStateConsumer {
            name,
            consumer: Arc::new(Mutex::new(Box::new(consumer))),
            unhealthy: AtomicBool::new(false),
            recovery_state: AtomicU8::new(DerivedStateConsumerRecoveryState::Live.into_u8()),
            applied_events: AtomicU64::new(0),
            checkpoint_flushes: AtomicU64::new(0),
            fault_count: AtomicU64::new(0),
            last_applied_sequence: AtomicU64::new(0),
            has_last_applied_sequence: AtomicBool::new(false),
            last_fault_sequence: AtomicU64::new(0),
            has_last_fault_sequence: AtomicBool::new(false),
            last_fault_kind: AtomicU8::new(0),
            has_last_fault_kind: AtomicBool::new(false),
        });
        self
    }

    /// Registers one replay source used to resume from retained feed envelopes.
    #[must_use]
    pub fn with_replay_source(mut self, replay_source: Arc<dyn DerivedStateReplaySource>) -> Self {
        self.replay_source = Some(replay_source);
        self
    }

    /// Overrides the generated session id.
    #[must_use]
    pub const fn with_session_id(mut self, session_id: FeedSessionId) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Builds an immutable derived-state host.
    #[must_use]
    pub fn build(self) -> DerivedStateHost {
        DerivedStateHost {
            inner: Arc::new(DerivedStateHostInner {
                consumers: Arc::from(self.consumers),
                session_id: self.session_id.unwrap_or_else(generate_session_id),
                replay_source: self.replay_source,
                runtime_replay_source: OnceLock::new(),
                dispatch_state: Mutex::new(DerivedStateDispatchState::default()),
                fault_count: AtomicU64::new(0),
                initialized: AtomicBool::new(false),
                slot_tx_indexes: Mutex::new(HashMap::new()),
            }),
        }
    }
}

/// Immutable host for derived-state consumers.
#[derive(Clone)]
pub struct DerivedStateHost {
    /// Shared host state and dispatch bookkeeping.
    inner: Arc<DerivedStateHostInner>,
}

impl Default for DerivedStateHost {
    fn default() -> Self {
        DerivedStateHostBuilder::new().build()
    }
}

impl DerivedStateHost {
    /// Returns the configured replay source or the runtime-installed fallback when present.
    #[must_use]
    fn replay_source(&self) -> Option<&Arc<dyn DerivedStateReplaySource>> {
        self.inner
            .replay_source
            .as_ref()
            .or_else(|| self.inner.runtime_replay_source.get())
    }

    /// Maps replay-source failures into structured consumer faults.
    fn replay_fault(
        checkpoint: &DerivedStateCheckpoint,
        error: &DerivedStateReplayError,
    ) -> DerivedStateConsumerFault {
        match error {
            DerivedStateReplayError::SessionUnavailable(session_id) => {
                DerivedStateConsumerFault::new(
                    DerivedStateConsumerFaultKind::ReplayGap,
                    checkpoint
                        .next_sequence()
                        .or(Some(checkpoint.last_applied_sequence)),
                    format!(
                        "derived-state replay session unavailable for checkpoint session {:?}; requested {:?}",
                        session_id,
                        checkpoint.next_sequence()
                    ),
                )
            }
            DerivedStateReplayError::SequenceGap {
                session_id,
                sequence,
            } => DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::ReplayGap,
                Some(*sequence),
                format!(
                    "derived-state replay gap in session {:?} at sequence {:?}",
                    session_id, sequence
                ),
            ),
            DerivedStateReplayError::Truncated {
                session_id,
                sequence,
                oldest_retained_sequence,
            } => DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::ReplayGap,
                Some(*sequence),
                format!(
                    "derived-state replay truncated in session {:?} before sequence {:?}; oldest retained {:?}",
                    session_id, sequence, oldest_retained_sequence
                ),
            ),
            DerivedStateReplayError::BackendFailure {
                session_id,
                message,
            } => DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::ReplayGap,
                checkpoint
                    .next_sequence()
                    .or(Some(checkpoint.last_applied_sequence)),
                format!(
                    "derived-state replay backend failure in session {:?}: {}",
                    session_id, message
                ),
            ),
        }
    }

    /// Starts a new derived-state host builder.
    #[must_use]
    pub const fn builder() -> DerivedStateHostBuilder {
        DerivedStateHostBuilder::new()
    }

    /// Returns `true` when no consumers are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.consumers.is_empty()
    }

    /// Returns the number of registered consumers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.consumers.len()
    }

    /// Returns registered consumer names in registration order.
    #[must_use]
    pub fn consumer_names(&self) -> Vec<&'static str> {
        self.inner
            .consumers
            .iter()
            .map(|consumer| consumer.name)
            .collect()
    }

    /// Returns the current feed session id.
    #[must_use]
    pub fn session_id(&self) -> FeedSessionId {
        self.inner.session_id
    }

    /// Installs a runtime-owned replay source when the builder did not configure one.
    ///
    /// Returns `true` when the source was installed for this host. A builder-provided replay
    /// source always takes precedence.
    pub fn install_runtime_replay_source(
        &self,
        replay_source: Arc<dyn DerivedStateReplaySource>,
    ) -> bool {
        if self.inner.replay_source.is_some() {
            return false;
        }
        self.inner.runtime_replay_source.set(replay_source).is_ok()
    }

    /// Returns the total number of structured consumer faults observed by the host.
    #[must_use]
    pub fn fault_count(&self) -> u64 {
        self.inner.fault_count.load(Ordering::Relaxed)
    }

    /// Returns the number of registered consumers still considered healthy for live apply.
    #[must_use]
    pub fn healthy_consumer_count(&self) -> usize {
        self.inner
            .consumers
            .iter()
            .filter(|consumer| !consumer.is_unhealthy())
            .count()
    }

    /// Returns `true` when at least one consumer has lost live continuity.
    #[must_use]
    pub fn has_unhealthy_consumers(&self) -> bool {
        self.inner
            .consumers
            .iter()
            .any(RegisteredDerivedStateConsumer::is_unhealthy)
    }

    /// Returns the names of consumers that are no longer receiving live feed events.
    #[must_use]
    pub fn unhealthy_consumer_names(&self) -> Vec<&'static str> {
        self.inner
            .consumers
            .iter()
            .filter(|consumer| consumer.is_unhealthy())
            .map(|consumer| consumer.name)
            .collect()
    }

    /// Returns `true` when at least one consumer requires replay-based resync or rebuild.
    #[must_use]
    pub fn has_consumers_requiring_resync(&self) -> bool {
        self.has_unhealthy_consumers()
    }

    /// Returns the names of consumers that require replay-based resync or rebuild.
    #[must_use]
    pub fn consumers_requiring_resync(&self) -> Vec<&'static str> {
        self.unhealthy_consumer_names()
    }

    /// Returns the names of consumers that can still attempt replay-based recovery.
    #[must_use]
    pub fn consumers_pending_recovery(&self) -> Vec<&'static str> {
        self.inner
            .consumers
            .iter()
            .filter(|consumer| {
                consumer.recovery_state()
                    == DerivedStateConsumerRecoveryState::ReplayRecoveryPending
            })
            .map(|consumer| consumer.name)
            .collect()
    }

    /// Returns the names of consumers that now require a full rebuild.
    #[must_use]
    pub fn consumers_requiring_rebuild(&self) -> Vec<&'static str> {
        self.inner
            .consumers
            .iter()
            .filter(|consumer| {
                consumer.recovery_state() == DerivedStateConsumerRecoveryState::RebuildRequired
            })
            .map(|consumer| consumer.name)
            .collect()
    }

    /// Returns per-consumer live-feed telemetry snapshots in registration order.
    #[must_use]
    pub fn consumer_telemetry(&self) -> Vec<DerivedStateConsumerTelemetry> {
        self.inner
            .consumers
            .iter()
            .map(RegisteredDerivedStateConsumer::telemetry)
            .collect()
    }

    /// Returns operator-facing telemetry for the configured replay backend when present.
    #[must_use]
    pub fn replay_telemetry(&self) -> Option<DerivedStateReplayTelemetry> {
        self.replay_source()
            .map(|replay_source| replay_source.telemetry())
    }

    /// Returns the highest sequence emitted by this host when one exists.
    #[must_use]
    pub fn last_emitted_sequence(&self) -> Option<FeedSequence> {
        self.inner
            .dispatch_state
            .lock()
            .map(|state| state.last_sequence)
            .unwrap_or(None)
    }

    /// Returns the latest runtime watermarks recorded by this host.
    #[must_use]
    pub fn current_watermarks(&self) -> FeedWatermarks {
        self.inner
            .dispatch_state
            .lock()
            .map(|state| state.last_watermarks)
            .unwrap_or_default()
    }

    /// Initializes registered consumers by attempting to load their durable checkpoints once.
    pub fn initialize(&self) {
        let was_uninitialized = self
            .inner
            .initialized
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if !was_uninitialized {
            return;
        }

        for registered in self.inner.consumers.iter() {
            if registered.is_unhealthy() {
                continue;
            }
            let Ok(mut consumer) = registered.consumer.lock() else {
                self.record_consumer_fault(
                    registered,
                    &DerivedStateConsumerFault::new(
                        DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                        None,
                        "derived-state consumer mutex poisoned during initialization",
                    ),
                );
                continue;
            };
            match consumer.load_checkpoint() {
                Ok(Some(checkpoint)) => {
                    let Some(next_sequence) = checkpoint.next_sequence() else {
                        continue;
                    };
                    if let Some(replay_source) = self.replay_source() {
                        match replay_source.replay_from(checkpoint.session_id, next_sequence) {
                            Ok(replayed) => {
                                for envelope in replayed {
                                    let sequence = envelope.sequence;
                                    if let Err(fault) = consumer.apply(envelope) {
                                        self.record_consumer_fault(registered, &fault);
                                        break;
                                    }
                                    registered.note_applied(sequence);
                                }
                            }
                            Err(error) => {
                                let fault = Self::replay_fault(&checkpoint, &error);
                                self.record_consumer_fault(registered, &fault);
                            }
                        }
                    } else if checkpoint.session_id != self.inner.session_id {
                        let fault = DerivedStateConsumerFault::new(
                            DerivedStateConsumerFaultKind::ReplayGap,
                            Some(next_sequence),
                            format!(
                                "checkpoint session {:?} does not match current session {:?} and no replay source is configured",
                                checkpoint.session_id, self.inner.session_id
                            ),
                        );
                        self.record_consumer_fault(registered, &fault);
                    }
                }
                Ok(None) => {}
                Err(fault) => {
                    self.record_consumer_fault(registered, &fault);
                }
            }
        }
    }

    /// Attempts replay-based recovery for unhealthy consumers.
    #[must_use]
    pub fn recover_consumers(&self) -> DerivedStateRecoveryReport {
        let mut report = DerivedStateRecoveryReport::default();
        for registered in self.inner.consumers.iter() {
            if !registered.is_unhealthy() {
                continue;
            }
            report.attempted = report.attempted.saturating_add(1);

            let Ok(mut consumer) = registered.consumer.lock() else {
                self.record_consumer_fault(
                    registered,
                    &DerivedStateConsumerFault::new(
                        DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                        None,
                        "derived-state consumer mutex poisoned during recovery",
                    ),
                );
                report.still_pending = report.still_pending.saturating_add(1);
                continue;
            };
            let checkpoint = match consumer.load_checkpoint() {
                Ok(Some(checkpoint)) => checkpoint,
                Ok(None) => {
                    registered
                        .set_recovery_state(DerivedStateConsumerRecoveryState::RebuildRequired);
                    report.rebuild_required = report.rebuild_required.saturating_add(1);
                    continue;
                }
                Err(fault) => {
                    self.record_consumer_fault(registered, &fault);
                    report.still_pending = report.still_pending.saturating_add(1);
                    continue;
                }
            };
            let Some(next_sequence) = checkpoint.next_sequence() else {
                registered.note_recovered_checkpoint(checkpoint.last_applied_sequence);
                report.recovered = report.recovered.saturating_add(1);
                continue;
            };
            let Some(replay_source) = self.replay_source() else {
                registered.set_recovery_state(DerivedStateConsumerRecoveryState::RebuildRequired);
                report.rebuild_required = report.rebuild_required.saturating_add(1);
                continue;
            };
            match replay_source.replay_from(checkpoint.session_id, next_sequence) {
                Ok(replayed) => {
                    let mut recovered = true;
                    if replayed.is_empty() {
                        registered.note_recovered_checkpoint(checkpoint.last_applied_sequence);
                    } else {
                        for envelope in replayed {
                            let sequence = envelope.sequence;
                            if let Err(fault) = consumer.apply(envelope) {
                                self.record_consumer_fault(registered, &fault);
                                recovered = false;
                                break;
                            }
                            registered.note_applied(sequence);
                        }
                        if recovered {
                            registered.mark_live();
                        }
                    }
                    if recovered {
                        report.recovered = report.recovered.saturating_add(1);
                    } else {
                        match registered.recovery_state() {
                            DerivedStateConsumerRecoveryState::RebuildRequired => {
                                report.rebuild_required = report.rebuild_required.saturating_add(1);
                            }
                            DerivedStateConsumerRecoveryState::Live
                            | DerivedStateConsumerRecoveryState::ReplayRecoveryPending => {
                                report.still_pending = report.still_pending.saturating_add(1);
                            }
                        }
                    }
                }
                Err(error) => {
                    let fault = Self::replay_fault(&checkpoint, &error);
                    self.record_consumer_fault(registered, &fault);
                    match registered.recovery_state() {
                        DerivedStateConsumerRecoveryState::RebuildRequired => {
                            report.rebuild_required = report.rebuild_required.saturating_add(1);
                        }
                        DerivedStateConsumerRecoveryState::Live
                        | DerivedStateConsumerRecoveryState::ReplayRecoveryPending => {
                            report.still_pending = report.still_pending.saturating_add(1);
                        }
                    }
                }
            }
        }
        report
    }

    /// Allocates the next per-slot transaction index for feed events.
    #[must_use]
    pub fn next_slot_tx_index(&self, slot: u64) -> u32 {
        let Ok(mut indexes) = self.inner.slot_tx_indexes.lock() else {
            return 0;
        };
        let entry = indexes.entry(slot).or_insert(0);
        let current = *entry;
        *entry = entry.saturating_add(1);
        current
    }

    /// Emits one transaction-applied record into the derived-state feed.
    pub fn on_transaction(&self, tx_index: u32, event: TransactionEvent) {
        if self.is_empty() {
            return;
        }
        self.dispatch(
            FeedWatermarks {
                canonical_tip_slot: None,
                processed_slot: Some(event.slot),
                confirmed_slot: event.confirmed_slot,
                finalized_slot: event.finalized_slot,
            },
            DerivedStateFeedEvent::TransactionApplied((tx_index, event).into()),
        );
    }

    /// Emits one account-touch record into the derived-state feed.
    pub fn on_account_touch(&self, tx_index: u32, event: AccountTouchEvent) {
        if self.is_empty() {
            return;
        }
        self.dispatch(
            FeedWatermarks {
                canonical_tip_slot: None,
                processed_slot: Some(event.slot),
                confirmed_slot: event.confirmed_slot,
                finalized_slot: event.finalized_slot,
            },
            DerivedStateFeedEvent::AccountTouchObserved((tx_index, event).into()),
        );
    }

    /// Emits one observed recent blockhash record into the derived-state feed.
    pub fn on_recent_blockhash(&self, event: ObservedRecentBlockhashEvent) {
        if self.is_empty() {
            return;
        }
        self.dispatch(
            FeedWatermarks {
                canonical_tip_slot: Some(event.slot),
                processed_slot: Some(event.slot),
                confirmed_slot: None,
                finalized_slot: None,
            },
            DerivedStateFeedEvent::RecentBlockhashObserved(event),
        );
    }

    /// Emits one cluster topology record into the derived-state feed.
    pub fn on_cluster_topology(&self, event: ClusterTopologyEvent) {
        if self.is_empty() {
            return;
        }
        self.dispatch(
            FeedWatermarks {
                canonical_tip_slot: event.slot,
                processed_slot: event.slot,
                confirmed_slot: None,
                finalized_slot: None,
            },
            DerivedStateFeedEvent::ClusterTopologyChanged(event),
        );
    }

    /// Emits one leader schedule record into the derived-state feed.
    pub fn on_leader_schedule(&self, event: LeaderScheduleEvent) {
        if self.is_empty() {
            return;
        }
        self.dispatch(
            FeedWatermarks {
                canonical_tip_slot: event.slot,
                processed_slot: event.slot,
                confirmed_slot: None,
                finalized_slot: None,
            },
            DerivedStateFeedEvent::LeaderScheduleUpdated(event),
        );
    }

    /// Emits one slot-status change record into the derived-state feed.
    pub fn on_slot_status(&self, event: SlotStatusEvent) {
        if self.is_empty() {
            return;
        }
        if matches!(
            event.status,
            ForkSlotStatus::Finalized | ForkSlotStatus::Orphaned
        ) && let Ok(mut indexes) = self.inner.slot_tx_indexes.lock()
        {
            let _ = indexes.remove(&event.slot);
        }
        self.dispatch(
            FeedWatermarks::from_slot_status(event),
            DerivedStateFeedEvent::SlotStatusChanged(event.into()),
        );
    }

    /// Emits one canonical branch reorg record into the derived-state feed.
    pub fn on_reorg(&self, event: ReorgEvent) {
        if self.is_empty() {
            return;
        }
        self.dispatch(
            FeedWatermarks::from_reorg(&event),
            DerivedStateFeedEvent::BranchReorged(event.into()),
        );
    }

    /// Emits a checkpoint barrier and flushes durable checkpoints for all consumers.
    pub fn emit_checkpoint_barrier(
        &self,
        reason: CheckpointBarrierReason,
        watermarks: FeedWatermarks,
    ) {
        if self.is_empty() {
            return;
        }

        let Ok(mut dispatch_state) = self.inner.dispatch_state.lock() else {
            self.record_internal_fault("derived-state dispatch mutex poisoned during checkpoint");
            return;
        };
        let sequence = FeedSequence(dispatch_state.next_sequence);
        dispatch_state.next_sequence = dispatch_state.next_sequence.saturating_add(1);
        let envelope = DerivedStateFeedEnvelope {
            session_id: self.inner.session_id,
            sequence,
            emitted_at: SystemTime::now(),
            watermarks,
            event: DerivedStateFeedEvent::CheckpointBarrier(CheckpointBarrierEvent {
                barrier_sequence: sequence,
                reason,
            }),
        };
        if let Some(replay_source) = self.replay_source() {
            replay_source.append(envelope.clone());
        }

        for registered in self.inner.consumers.iter() {
            if registered.is_unhealthy() {
                continue;
            }
            let Ok(mut consumer) = registered.consumer.lock() else {
                self.record_consumer_fault(
                    registered,
                    &DerivedStateConsumerFault::new(
                        DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                        Some(sequence),
                        "derived-state consumer mutex poisoned during checkpoint apply",
                    ),
                );
                continue;
            };
            if let Err(fault) = consumer.apply(envelope.clone()) {
                self.record_consumer_fault(registered, &fault);
                continue;
            }
            registered.note_applied(sequence);
            let checkpoint = DerivedStateCheckpoint {
                session_id: self.inner.session_id,
                last_applied_sequence: sequence,
                watermarks,
                state_version: consumer.state_version(),
                extension_version: consumer.extension_version().to_owned(),
            };
            if let Err(fault) = consumer.flush_checkpoint(checkpoint) {
                self.record_consumer_fault(registered, &fault);
            } else {
                registered.note_checkpoint_flush();
            }
        }

        dispatch_state.last_sequence = Some(sequence);
        dispatch_state.last_watermarks = watermarks;
    }

    /// Emits a shutdown checkpoint barrier using the latest runtime watermarks.
    pub fn emit_shutdown_checkpoint_barrier(&self, watermarks: FeedWatermarks) {
        self.emit_checkpoint_barrier(CheckpointBarrierReason::ShutdownRequested, watermarks);
    }

    /// Builds one feed envelope and dispatches it to every registered consumer.
    fn dispatch(&self, watermarks: FeedWatermarks, event: DerivedStateFeedEvent) {
        let Ok(mut dispatch_state) = self.inner.dispatch_state.lock() else {
            self.record_internal_fault("derived-state dispatch mutex poisoned during apply");
            return;
        };
        let sequence = FeedSequence(dispatch_state.next_sequence);
        dispatch_state.next_sequence = dispatch_state.next_sequence.saturating_add(1);
        let envelope = DerivedStateFeedEnvelope {
            session_id: self.inner.session_id,
            sequence,
            emitted_at: SystemTime::now(),
            watermarks,
            event,
        };
        if let Some(replay_source) = self.replay_source() {
            replay_source.append(envelope.clone());
        }

        for registered in self.inner.consumers.iter() {
            if registered.is_unhealthy() {
                continue;
            }
            let Ok(mut consumer) = registered.consumer.lock() else {
                self.record_consumer_fault(
                    registered,
                    &DerivedStateConsumerFault::new(
                        DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                        Some(sequence),
                        "derived-state consumer mutex poisoned during apply",
                    ),
                );
                continue;
            };
            if let Err(fault) = consumer.apply(envelope.clone()) {
                self.record_consumer_fault(registered, &fault);
            } else {
                registered.note_applied(sequence);
            }
        }

        dispatch_state.last_sequence = Some(sequence);
        dispatch_state.last_watermarks = watermarks;
    }

    /// Records one consumer fault for telemetry and structured logs.
    fn record_consumer_fault(
        &self,
        registered: &RegisteredDerivedStateConsumer,
        fault: &DerivedStateConsumerFault,
    ) {
        let _ = self.inner.fault_count.fetch_add(1, Ordering::Relaxed);
        registered.note_fault(fault.sequence);
        registered.note_fault_kind(fault.kind);
        if fault.kind.breaks_live_continuity() {
            registered.set_recovery_state(match fault.kind {
                DerivedStateConsumerFaultKind::ReplayGap => {
                    DerivedStateConsumerRecoveryState::RebuildRequired
                }
                DerivedStateConsumerFaultKind::LagExceeded
                | DerivedStateConsumerFaultKind::QueueOverflow
                | DerivedStateConsumerFaultKind::ConsumerApplyFailed => {
                    DerivedStateConsumerRecoveryState::ReplayRecoveryPending
                }
                DerivedStateConsumerFaultKind::CheckpointWriteFailed => {
                    DerivedStateConsumerRecoveryState::Live
                }
            });
        }
        if fault.kind.breaks_live_continuity() && registered.mark_unhealthy() {
            tracing::warn!(
                consumer = registered.name,
                ?fault.kind,
                sequence = ?fault.sequence,
                recovery_state = ?registered.recovery_state(),
                "derived-state consumer marked unhealthy"
            );
        }
        tracing::warn!(
            consumer = registered.name,
            ?fault.kind,
            sequence = ?fault.sequence,
            message = %fault.message,
            "derived-state consumer fault"
        );
    }

    /// Records one host-internal fault that is not attributable to a single consumer.
    fn record_internal_fault(&self, message: &'static str) {
        let _ = self.inner.fault_count.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(message, "derived-state host fault");
    }
}

/// Shared dispatch state for one immutable derived-state host.
struct DerivedStateHostInner {
    /// Registered consumers in dispatch order.
    consumers: Arc<[RegisteredDerivedStateConsumer]>,
    /// Monotonic session id assigned when the host is built.
    session_id: FeedSessionId,
    /// Optional retained replay source used during checkpoint resume.
    replay_source: Option<Arc<dyn DerivedStateReplaySource>>,
    /// Runtime-installed replay source used when the builder did not configure one.
    runtime_replay_source: OnceLock<Arc<dyn DerivedStateReplaySource>>,
    /// Serialized feed cursor and watermark state.
    dispatch_state: Mutex<DerivedStateDispatchState>,
    /// Total number of structured faults recorded across all consumers.
    fault_count: AtomicU64,
    /// Ensures checkpoint loading runs only once per host.
    initialized: AtomicBool,
    /// Per-slot transaction indexes used to stabilize event ordering.
    slot_tx_indexes: Mutex<HashMap<u64, u32>>,
}

/// Serialized derived-state feed cursor shared by all producer paths.
#[derive(Default)]
struct DerivedStateDispatchState {
    /// Next sequence number assigned to an emitted feed envelope.
    next_sequence: u64,
    /// Highest emitted sequence when at least one envelope has been dispatched.
    last_sequence: Option<FeedSequence>,
    /// Latest runtime watermarks observed by the host.
    last_watermarks: FeedWatermarks,
}

/// Consumer registration entry stored by the host.
struct RegisteredDerivedStateConsumer {
    /// Stable consumer name used in logs and telemetry.
    name: &'static str,
    /// Boxed consumer behind a mutex so the host can serialize callbacks.
    consumer: Arc<Mutex<Box<dyn DerivedStateConsumer>>>,
    /// Whether the consumer has lost live continuity and should stop receiving events.
    unhealthy: AtomicBool,
    /// Current recovery state for this consumer.
    recovery_state: AtomicU8,
    /// Total number of successfully applied envelopes.
    applied_events: AtomicU64,
    /// Total number of successfully flushed checkpoints.
    checkpoint_flushes: AtomicU64,
    /// Total number of structured faults recorded for the consumer.
    fault_count: AtomicU64,
    /// Highest applied sequence number when one exists.
    last_applied_sequence: AtomicU64,
    /// Whether `last_applied_sequence` is initialized.
    has_last_applied_sequence: AtomicBool,
    /// Highest fault-associated sequence number when one exists.
    last_fault_sequence: AtomicU64,
    /// Whether `last_fault_sequence` is initialized.
    has_last_fault_sequence: AtomicBool,
    /// Last structured fault kind when one exists.
    last_fault_kind: AtomicU8,
    /// Whether `last_fault_kind` is initialized.
    has_last_fault_kind: AtomicBool,
}

impl RegisteredDerivedStateConsumer {
    /// Returns whether this consumer has lost live continuity.
    #[must_use]
    fn is_unhealthy(&self) -> bool {
        self.unhealthy.load(Ordering::Acquire)
    }

    /// Returns the current recovery state for this consumer.
    #[must_use]
    fn recovery_state(&self) -> DerivedStateConsumerRecoveryState {
        DerivedStateConsumerRecoveryState::from_u8(self.recovery_state.load(Ordering::Acquire))
    }

    /// Marks the consumer unhealthy and returns whether the state changed.
    #[must_use]
    fn mark_unhealthy(&self) -> bool {
        !self.unhealthy.swap(true, Ordering::AcqRel)
    }

    /// Marks the consumer healthy after successful recovery.
    fn mark_live(&self) {
        self.unhealthy.store(false, Ordering::Release);
        self.recovery_state.store(
            DerivedStateConsumerRecoveryState::Live.into_u8(),
            Ordering::Release,
        );
    }

    /// Records one recovery state transition.
    fn set_recovery_state(&self, state: DerivedStateConsumerRecoveryState) {
        self.recovery_state
            .store(state.into_u8(), Ordering::Release);
    }

    /// Records one successful envelope application.
    fn note_applied(&self, sequence: FeedSequence) {
        let _ = self.applied_events.fetch_add(1, Ordering::Relaxed);
        self.last_applied_sequence
            .store(sequence.0, Ordering::Release);
        self.has_last_applied_sequence
            .store(true, Ordering::Release);
        if !self.is_unhealthy() {
            self.set_recovery_state(DerivedStateConsumerRecoveryState::Live);
        }
    }

    /// Records one successful recovery to an already-applied checkpoint boundary.
    fn note_recovered_checkpoint(&self, sequence: FeedSequence) {
        self.last_applied_sequence
            .store(sequence.0, Ordering::Release);
        self.has_last_applied_sequence
            .store(true, Ordering::Release);
        self.mark_live();
    }

    /// Records one successful checkpoint flush.
    fn note_checkpoint_flush(&self) {
        let _ = self.checkpoint_flushes.fetch_add(1, Ordering::Relaxed);
    }

    /// Records one structured consumer fault.
    fn note_fault(&self, sequence: Option<FeedSequence>) {
        let _ = self.fault_count.fetch_add(1, Ordering::Relaxed);
        if let Some(sequence) = sequence {
            self.last_fault_sequence
                .store(sequence.0, Ordering::Release);
            self.has_last_fault_sequence.store(true, Ordering::Release);
        }
    }

    /// Records the last fault kind for telemetry.
    fn note_fault_kind(&self, kind: DerivedStateConsumerFaultKind) {
        self.last_fault_kind
            .store(kind.into_u8(), Ordering::Release);
        self.has_last_fault_kind.store(true, Ordering::Release);
    }

    /// Builds a point-in-time telemetry snapshot for this consumer.
    #[must_use]
    fn telemetry(&self) -> DerivedStateConsumerTelemetry {
        DerivedStateConsumerTelemetry {
            name: self.name,
            unhealthy: self.is_unhealthy(),
            recovery_state: self.recovery_state(),
            applied_events: self.applied_events.load(Ordering::Relaxed),
            checkpoint_flushes: self.checkpoint_flushes.load(Ordering::Relaxed),
            fault_count: self.fault_count.load(Ordering::Relaxed),
            last_applied_sequence: self
                .has_last_applied_sequence
                .load(Ordering::Acquire)
                .then(|| FeedSequence(self.last_applied_sequence.load(Ordering::Acquire))),
            last_fault_sequence: self
                .has_last_fault_sequence
                .load(Ordering::Acquire)
                .then(|| FeedSequence(self.last_fault_sequence.load(Ordering::Acquire))),
            last_fault_kind: self.has_last_fault_kind.load(Ordering::Acquire).then(|| {
                DerivedStateConsumerFaultKind::from_u8(self.last_fault_kind.load(Ordering::Acquire))
            }),
        }
    }
}

/// Generates a best-effort unique session id for one process lifetime.
fn generate_session_id() -> FeedSessionId {
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |duration| duration.as_nanos());
    let pid = u128::from(std::process::id());
    FeedSessionId(now_nanos ^ pid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framework::{ClusterNodeInfo, ControlPlaneSource, LeaderScheduleEntry};
    use std::{
        env, fs,
        net::SocketAddr,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering as AtomicOrdering},
        },
        thread,
    };

    fn unique_test_replay_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0_u128, |duration| duration.as_nanos());
        env::temp_dir().join(format!(
            "sof-derived-state-{name}-{}-{unique}",
            std::process::id()
        ))
    }

    #[test]
    fn feed_sequence_next_advances_by_one() {
        assert_eq!(FeedSequence(41).next(), Some(FeedSequence(42)));
    }

    #[test]
    fn watermarks_commitment_prefers_finalized() {
        let watermarks = FeedWatermarks {
            canonical_tip_slot: Some(200),
            processed_slot: Some(200),
            confirmed_slot: Some(150),
            finalized_slot: Some(120),
        };
        assert_eq!(
            watermarks.commitment_for_slot(100),
            TxCommitmentStatus::Finalized
        );
        assert_eq!(
            watermarks.commitment_for_slot(140),
            TxCommitmentStatus::Confirmed
        );
        assert_eq!(
            watermarks.commitment_for_slot(180),
            TxCommitmentStatus::Processed
        );
    }

    #[test]
    fn checkpoint_next_sequence_uses_last_applied_boundary() {
        let checkpoint = DerivedStateCheckpoint {
            session_id: FeedSessionId(7),
            last_applied_sequence: FeedSequence(99),
            watermarks: FeedWatermarks::default(),
            state_version: 1,
            extension_version: "test".to_owned(),
        };
        assert_eq!(checkpoint.next_sequence(), Some(FeedSequence(100)));
    }

    #[test]
    fn replay_backend_parses_case_insensitively() {
        assert_eq!(
            DerivedStateReplayBackend::from_config_value("memory"),
            Some(DerivedStateReplayBackend::Memory)
        );
        assert_eq!(
            DerivedStateReplayBackend::from_config_value("DISK"),
            Some(DerivedStateReplayBackend::Disk)
        );
        assert_eq!(DerivedStateReplayBackend::from_config_value("other"), None);
    }

    #[test]
    fn replay_durability_parses_case_insensitively() {
        assert_eq!(
            DerivedStateReplayDurability::from_config_value("flush"),
            Some(DerivedStateReplayDurability::Flush)
        );
        assert_eq!(
            DerivedStateReplayDurability::from_config_value("FSYNC"),
            Some(DerivedStateReplayDurability::Fsync)
        );
        assert_eq!(
            DerivedStateReplayDurability::from_config_value("other"),
            None
        );
    }

    #[test]
    fn watermarks_from_reorg_use_new_tip_and_commitment_fields() {
        let watermarks = FeedWatermarks::from_reorg(&ReorgEvent {
            old_tip: 100,
            new_tip: 120,
            common_ancestor: Some(95),
            detached_slots: vec![100, 99],
            attached_slots: vec![118, 119, 120],
            confirmed_slot: Some(110),
            finalized_slot: Some(90),
        });

        assert_eq!(watermarks.canonical_tip_slot, Some(120));
        assert_eq!(watermarks.processed_slot, Some(120));
        assert_eq!(watermarks.confirmed_slot, Some(110));
        assert_eq!(watermarks.finalized_slot, Some(90));
    }

    #[derive(Default)]
    struct RecordingState {
        envelopes: Vec<DerivedStateFeedEnvelope>,
        checkpoints: Vec<DerivedStateCheckpoint>,
    }

    struct RecordingConsumer {
        state: Arc<Mutex<RecordingState>>,
    }

    impl RecordingConsumer {
        fn new(state: Arc<Mutex<RecordingState>>) -> Self {
            Self { state }
        }
    }

    impl DerivedStateConsumer for RecordingConsumer {
        fn name(&self) -> &'static str {
            "recording-consumer"
        }

        fn state_version(&self) -> u32 {
            7
        }

        fn extension_version(&self) -> &'static str {
            "test-consumer"
        }

        fn load_checkpoint(
            &mut self,
        ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
            Ok(None)
        }

        fn apply(
            &mut self,
            envelope: DerivedStateFeedEnvelope,
        ) -> Result<(), DerivedStateConsumerFault> {
            self.state
                .lock()
                .map_err(|_poison| {
                    DerivedStateConsumerFault::new(
                        DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                        Some(envelope.sequence),
                        "recording state mutex poisoned during apply",
                    )
                })?
                .envelopes
                .push(envelope);
            Ok(())
        }

        fn flush_checkpoint(
            &mut self,
            checkpoint: DerivedStateCheckpoint,
        ) -> Result<(), DerivedStateConsumerFault> {
            self.state
                .lock()
                .map_err(|_poison| {
                    DerivedStateConsumerFault::new(
                        DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                        Some(checkpoint.last_applied_sequence),
                        "recording state mutex poisoned during checkpoint flush",
                    )
                })?
                .checkpoints
                .push(checkpoint);
            Ok(())
        }
    }

    #[test]
    fn host_assigns_monotonic_sequences() {
        let state = Arc::new(Mutex::new(RecordingState::default()));
        let host = DerivedStateHost::builder()
            .add_consumer(RecordingConsumer::new(Arc::clone(&state)))
            .build();

        host.on_slot_status(SlotStatusEvent {
            slot: 10,
            parent_slot: Some(9),
            previous_status: None,
            status: ForkSlotStatus::Processed,
            tip_slot: Some(10),
            confirmed_slot: None,
            finalized_slot: None,
        });
        host.on_reorg(ReorgEvent {
            old_tip: 10,
            new_tip: 12,
            common_ancestor: Some(8),
            detached_slots: vec![10],
            attached_slots: vec![11, 12],
            confirmed_slot: Some(7),
            finalized_slot: Some(6),
        });

        let state = state
            .lock()
            .expect("recording state mutex should not be poisoned");
        let sequences = state
            .envelopes
            .iter()
            .map(|envelope| envelope.sequence)
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![FeedSequence(0), FeedSequence(1)]);
        assert_eq!(host.last_emitted_sequence(), Some(FeedSequence(1)));
        assert_eq!(
            host.current_watermarks(),
            FeedWatermarks {
                canonical_tip_slot: Some(12),
                processed_slot: Some(12),
                confirmed_slot: Some(7),
                finalized_slot: Some(6),
            }
        );
        assert_eq!(host.fault_count(), 0);
    }

    #[test]
    fn host_dispatches_control_plane_events_into_feed() {
        let state = Arc::new(Mutex::new(RecordingState::default()));
        let host = DerivedStateHost::builder()
            .add_consumer(RecordingConsumer::new(Arc::clone(&state)))
            .build();

        host.on_recent_blockhash(ObservedRecentBlockhashEvent {
            slot: 70,
            recent_blockhash: [7_u8; 32],
            dataset_tx_count: 3,
        });
        host.on_cluster_topology(ClusterTopologyEvent {
            source: ControlPlaneSource::GossipBootstrap,
            slot: Some(71),
            epoch: None,
            active_entrypoint: None,
            total_nodes: 1,
            added_nodes: Vec::new(),
            removed_pubkeys: Vec::new(),
            updated_nodes: vec![ClusterNodeInfo {
                pubkey: [1_u8; 32].into(),
                wallclock: 1,
                shred_version: 1,
                gossip: None,
                tpu: Some(SocketAddr::from(([127, 0, 0, 1], 9000))),
                tpu_quic: Some(SocketAddr::from(([127, 0, 0, 1], 9006))),
                tpu_forwards: None,
                tpu_forwards_quic: None,
                tpu_vote: None,
                tvu: None,
                rpc: None,
            }],
            snapshot_nodes: Vec::new(),
        });
        host.on_leader_schedule(LeaderScheduleEvent {
            source: ControlPlaneSource::GossipBootstrap,
            slot: Some(72),
            epoch: None,
            added_leaders: Vec::new(),
            removed_slots: Vec::new(),
            updated_leaders: vec![LeaderScheduleEntry {
                slot: 72,
                leader: [2_u8; 32].into(),
            }],
            snapshot_leaders: Vec::new(),
        });

        let state = state
            .lock()
            .expect("recording state mutex should not be poisoned");
        assert_eq!(state.envelopes.len(), 3);
        assert!(matches!(
            state.envelopes[0].event,
            DerivedStateFeedEvent::RecentBlockhashObserved(_)
        ));
        assert_eq!(state.envelopes[0].watermarks.processed_slot, Some(70));
        assert!(matches!(
            state.envelopes[1].event,
            DerivedStateFeedEvent::ClusterTopologyChanged(_)
        ));
        assert_eq!(state.envelopes[1].watermarks.processed_slot, Some(71));
        assert!(matches!(
            state.envelopes[2].event,
            DerivedStateFeedEvent::LeaderScheduleUpdated(_)
        ));
        assert_eq!(state.envelopes[2].watermarks.processed_slot, Some(72));
        assert_eq!(host.last_emitted_sequence(), Some(FeedSequence(2)));
    }

    #[test]
    fn host_serializes_sequences_across_concurrent_producers() {
        let state = Arc::new(Mutex::new(RecordingState::default()));
        let host = DerivedStateHost::builder()
            .add_consumer(RecordingConsumer::new(Arc::clone(&state)))
            .build();

        thread::scope(|scope| {
            for slot in 0_u64..16 {
                let host = host.clone();
                scope.spawn(move || {
                    host.on_slot_status(SlotStatusEvent {
                        slot,
                        parent_slot: slot.checked_sub(1),
                        previous_status: None,
                        status: ForkSlotStatus::Processed,
                        tip_slot: Some(slot),
                        confirmed_slot: None,
                        finalized_slot: None,
                    });
                });
            }
        });

        let state = state
            .lock()
            .expect("recording state mutex should not be poisoned");
        let sequences = state
            .envelopes
            .iter()
            .map(|envelope| envelope.sequence.0)
            .collect::<Vec<_>>();
        assert_eq!(sequences.len(), 16);
        assert_eq!(sequences, (0_u64..16).collect::<Vec<_>>());
        assert_eq!(host.last_emitted_sequence(), Some(FeedSequence(15)));
        assert_eq!(host.fault_count(), 0);
    }

    #[test]
    fn checkpoint_barrier_flushes_consumer_checkpoint() {
        let state = Arc::new(Mutex::new(RecordingState::default()));
        let host = DerivedStateHost::builder()
            .add_consumer(RecordingConsumer::new(Arc::clone(&state)))
            .build();

        host.on_slot_status(SlotStatusEvent {
            slot: 44,
            parent_slot: Some(43),
            previous_status: None,
            status: ForkSlotStatus::Processed,
            tip_slot: Some(44),
            confirmed_slot: Some(40),
            finalized_slot: Some(39),
        });
        let watermarks = FeedWatermarks {
            canonical_tip_slot: Some(44),
            processed_slot: Some(44),
            confirmed_slot: Some(40),
            finalized_slot: Some(39),
        };
        host.emit_checkpoint_barrier(CheckpointBarrierReason::Periodic, watermarks);

        let state = state
            .lock()
            .expect("recording state mutex should not be poisoned");
        assert_eq!(state.envelopes.len(), 2);
        assert_eq!(state.checkpoints.len(), 1);
        let barrier_envelope = &state.envelopes[1];
        assert_eq!(barrier_envelope.sequence, FeedSequence(1));
        assert_eq!(barrier_envelope.watermarks, watermarks);
        assert!(matches!(
            barrier_envelope.event,
            DerivedStateFeedEvent::CheckpointBarrier(CheckpointBarrierEvent {
                barrier_sequence: FeedSequence(1),
                reason: CheckpointBarrierReason::Periodic,
            })
        ));
        assert_eq!(
            state.checkpoints[0],
            DerivedStateCheckpoint {
                session_id: host.session_id(),
                last_applied_sequence: FeedSequence(1),
                watermarks,
                state_version: 7,
                extension_version: "test-consumer".to_owned(),
            }
        );
        assert_eq!(host.last_emitted_sequence(), Some(FeedSequence(1)));
        assert_eq!(host.current_watermarks(), watermarks);
        assert_eq!(host.fault_count(), 0);
        assert_eq!(
            host.consumer_telemetry(),
            vec![DerivedStateConsumerTelemetry {
                name: "recording-consumer",
                unhealthy: false,
                recovery_state: DerivedStateConsumerRecoveryState::Live,
                applied_events: 2,
                checkpoint_flushes: 1,
                fault_count: 0,
                last_applied_sequence: Some(FeedSequence(1)),
                last_fault_sequence: None,
                last_fault_kind: None,
            }]
        );
    }

    struct FailingApplyConsumer {
        apply_calls: Arc<AtomicUsize>,
    }

    impl DerivedStateConsumer for FailingApplyConsumer {
        fn name(&self) -> &'static str {
            "failing-apply"
        }

        fn state_version(&self) -> u32 {
            1
        }

        fn extension_version(&self) -> &'static str {
            "failing-apply-test"
        }

        fn load_checkpoint(
            &mut self,
        ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
            Ok(None)
        }

        fn apply(
            &mut self,
            envelope: DerivedStateFeedEnvelope,
        ) -> Result<(), DerivedStateConsumerFault> {
            let _ = self.apply_calls.fetch_add(1, AtomicOrdering::Relaxed);
            Err(DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                Some(envelope.sequence),
                "intentional test failure",
            ))
        }

        fn flush_checkpoint(
            &mut self,
            _checkpoint: DerivedStateCheckpoint,
        ) -> Result<(), DerivedStateConsumerFault> {
            Ok(())
        }
    }

    #[test]
    fn continuity_fault_marks_consumer_unhealthy_and_stops_live_apply() {
        let apply_calls = Arc::new(AtomicUsize::new(0));
        let host = DerivedStateHost::builder()
            .add_consumer(FailingApplyConsumer {
                apply_calls: Arc::clone(&apply_calls),
            })
            .build();

        host.on_slot_status(SlotStatusEvent {
            slot: 1,
            parent_slot: None,
            previous_status: None,
            status: ForkSlotStatus::Processed,
            tip_slot: Some(1),
            confirmed_slot: None,
            finalized_slot: None,
        });
        host.on_reorg(ReorgEvent {
            old_tip: 1,
            new_tip: 2,
            common_ancestor: Some(0),
            detached_slots: vec![1],
            attached_slots: vec![2],
            confirmed_slot: None,
            finalized_slot: None,
        });

        assert_eq!(apply_calls.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(host.healthy_consumer_count(), 0);
        assert!(host.has_unhealthy_consumers());
        assert_eq!(host.unhealthy_consumer_names(), vec!["failing-apply"]);
        assert!(host.has_consumers_requiring_resync());
        assert_eq!(host.consumers_requiring_resync(), vec!["failing-apply"]);
        assert_eq!(host.consumers_pending_recovery(), vec!["failing-apply"]);
        assert!(host.consumers_requiring_rebuild().is_empty());
        assert_eq!(host.fault_count(), 1);
        assert_eq!(
            host.consumer_telemetry(),
            vec![DerivedStateConsumerTelemetry {
                name: "failing-apply",
                unhealthy: true,
                recovery_state: DerivedStateConsumerRecoveryState::ReplayRecoveryPending,
                applied_events: 0,
                checkpoint_flushes: 0,
                fault_count: 1,
                last_applied_sequence: None,
                last_fault_sequence: Some(FeedSequence(0)),
                last_fault_kind: Some(DerivedStateConsumerFaultKind::ConsumerApplyFailed),
            }]
        );
    }

    struct FailingCheckpointConsumer {
        apply_calls: Arc<AtomicUsize>,
        flush_calls: Arc<AtomicUsize>,
    }

    impl DerivedStateConsumer for FailingCheckpointConsumer {
        fn name(&self) -> &'static str {
            "failing-checkpoint"
        }

        fn state_version(&self) -> u32 {
            9
        }

        fn extension_version(&self) -> &'static str {
            "failing-checkpoint-test"
        }

        fn load_checkpoint(
            &mut self,
        ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
            Ok(None)
        }

        fn apply(
            &mut self,
            _envelope: DerivedStateFeedEnvelope,
        ) -> Result<(), DerivedStateConsumerFault> {
            let _ = self.apply_calls.fetch_add(1, AtomicOrdering::Relaxed);
            Ok(())
        }

        fn flush_checkpoint(
            &mut self,
            checkpoint: DerivedStateCheckpoint,
        ) -> Result<(), DerivedStateConsumerFault> {
            let _ = self.flush_calls.fetch_add(1, AtomicOrdering::Relaxed);
            Err(DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                Some(checkpoint.last_applied_sequence),
                "intentional checkpoint write failure",
            ))
        }
    }

    #[test]
    fn checkpoint_write_fault_does_not_mark_consumer_unhealthy() {
        let apply_calls = Arc::new(AtomicUsize::new(0));
        let flush_calls = Arc::new(AtomicUsize::new(0));
        let host = DerivedStateHost::builder()
            .add_consumer(FailingCheckpointConsumer {
                apply_calls: Arc::clone(&apply_calls),
                flush_calls: Arc::clone(&flush_calls),
            })
            .build();

        host.on_slot_status(SlotStatusEvent {
            slot: 11,
            parent_slot: Some(10),
            previous_status: None,
            status: ForkSlotStatus::Processed,
            tip_slot: Some(11),
            confirmed_slot: Some(9),
            finalized_slot: Some(8),
        });
        host.emit_checkpoint_barrier(
            CheckpointBarrierReason::Periodic,
            FeedWatermarks {
                canonical_tip_slot: Some(11),
                processed_slot: Some(11),
                confirmed_slot: Some(9),
                finalized_slot: Some(8),
            },
        );
        host.on_reorg(ReorgEvent {
            old_tip: 11,
            new_tip: 12,
            common_ancestor: Some(10),
            detached_slots: vec![11],
            attached_slots: vec![12],
            confirmed_slot: Some(9),
            finalized_slot: Some(8),
        });

        assert_eq!(apply_calls.load(AtomicOrdering::Relaxed), 3);
        assert_eq!(flush_calls.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(host.healthy_consumer_count(), 1);
        assert!(!host.has_unhealthy_consumers());
        assert!(host.unhealthy_consumer_names().is_empty());
        assert!(!host.has_consumers_requiring_resync());
        assert!(host.consumers_requiring_resync().is_empty());
        assert_eq!(host.fault_count(), 1);
        assert_eq!(
            host.consumer_telemetry(),
            vec![DerivedStateConsumerTelemetry {
                name: "failing-checkpoint",
                unhealthy: false,
                recovery_state: DerivedStateConsumerRecoveryState::Live,
                applied_events: 3,
                checkpoint_flushes: 0,
                fault_count: 1,
                last_applied_sequence: Some(FeedSequence(2)),
                last_fault_sequence: Some(FeedSequence(1)),
                last_fault_kind: Some(DerivedStateConsumerFaultKind::CheckpointWriteFailed),
            }]
        );
    }

    struct ReplayCheckpointConsumer {
        state: Arc<Mutex<RecordingState>>,
        checkpoint: Option<DerivedStateCheckpoint>,
    }

    impl DerivedStateConsumer for ReplayCheckpointConsumer {
        fn name(&self) -> &'static str {
            "replay-checkpoint"
        }

        fn state_version(&self) -> u32 {
            3
        }

        fn extension_version(&self) -> &'static str {
            "replay-checkpoint-test"
        }

        fn load_checkpoint(
            &mut self,
        ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
            Ok(self.checkpoint.take())
        }

        fn apply(
            &mut self,
            envelope: DerivedStateFeedEnvelope,
        ) -> Result<(), DerivedStateConsumerFault> {
            self.state
                .lock()
                .map_err(|_poison| {
                    DerivedStateConsumerFault::new(
                        DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                        Some(envelope.sequence),
                        "replay recording state mutex poisoned during apply",
                    )
                })?
                .envelopes
                .push(envelope);
            Ok(())
        }

        fn flush_checkpoint(
            &mut self,
            checkpoint: DerivedStateCheckpoint,
        ) -> Result<(), DerivedStateConsumerFault> {
            self.state
                .lock()
                .map_err(|_poison| {
                    DerivedStateConsumerFault::new(
                        DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                        Some(checkpoint.last_applied_sequence),
                        "replay recording state mutex poisoned during checkpoint flush",
                    )
                })?
                .checkpoints
                .push(checkpoint);
            Ok(())
        }
    }

    struct RecoverableConsumerState {
        checkpoint: Option<DerivedStateCheckpoint>,
        fail_sequence_once: Option<FeedSequence>,
        applied_sequences: Vec<FeedSequence>,
    }

    struct RecoverableConsumer {
        state: Arc<Mutex<RecoverableConsumerState>>,
    }

    impl DerivedStateConsumer for RecoverableConsumer {
        fn name(&self) -> &'static str {
            "recoverable-consumer"
        }

        fn state_version(&self) -> u32 {
            5
        }

        fn extension_version(&self) -> &'static str {
            "recoverable-consumer-test"
        }

        fn load_checkpoint(
            &mut self,
        ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
            let state = self.state.lock().map_err(|_poison| {
                DerivedStateConsumerFault::new(
                    DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                    None,
                    "recoverable consumer state mutex poisoned while loading checkpoint",
                )
            })?;
            Ok(state.checkpoint.clone())
        }

        fn apply(
            &mut self,
            envelope: DerivedStateFeedEnvelope,
        ) -> Result<(), DerivedStateConsumerFault> {
            let mut state = self.state.lock().map_err(|_poison| {
                DerivedStateConsumerFault::new(
                    DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                    Some(envelope.sequence),
                    "recoverable consumer state mutex poisoned while applying envelope",
                )
            })?;
            if state.fail_sequence_once == Some(envelope.sequence) {
                state.fail_sequence_once = None;
                return Err(DerivedStateConsumerFault::new(
                    DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                    Some(envelope.sequence),
                    "recoverable consumer injected one-shot apply failure",
                ));
            }
            state.applied_sequences.push(envelope.sequence);
            Ok(())
        }

        fn flush_checkpoint(
            &mut self,
            checkpoint: DerivedStateCheckpoint,
        ) -> Result<(), DerivedStateConsumerFault> {
            let sequence = checkpoint.last_applied_sequence;
            self.state
                .lock()
                .map_err(|_poison| {
                    DerivedStateConsumerFault::new(
                        DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                        Some(sequence),
                        "recoverable consumer state mutex poisoned while flushing checkpoint",
                    )
                })?
                .checkpoint = Some(checkpoint);
            Ok(())
        }
    }

    #[test]
    fn initialize_replays_retained_events_from_checkpoint() {
        let replay_source = Arc::new(InMemoryDerivedStateReplaySource::new());
        let session_id = FeedSessionId(42);

        replay_source.append(DerivedStateFeedEnvelope {
            session_id,
            sequence: FeedSequence(0),
            emitted_at: SystemTime::UNIX_EPOCH,
            watermarks: FeedWatermarks {
                canonical_tip_slot: Some(10),
                processed_slot: Some(10),
                confirmed_slot: Some(9),
                finalized_slot: Some(8),
            },
            event: DerivedStateFeedEvent::SlotStatusChanged(SlotStatusChangedEvent {
                slot: 10,
                parent_slot: Some(9),
                previous_status: None,
                status: ForkSlotStatus::Processed,
            }),
        });
        replay_source.append(DerivedStateFeedEnvelope {
            session_id,
            sequence: FeedSequence(1),
            emitted_at: SystemTime::UNIX_EPOCH,
            watermarks: FeedWatermarks {
                canonical_tip_slot: Some(11),
                processed_slot: Some(11),
                confirmed_slot: Some(9),
                finalized_slot: Some(8),
            },
            event: DerivedStateFeedEvent::BranchReorged(BranchReorgedEvent {
                old_tip: 10,
                new_tip: 11,
                common_ancestor: Some(9),
                detached_slots: Arc::from([10]),
                attached_slots: Arc::from([11]),
            }),
        });

        let state = Arc::new(Mutex::new(RecordingState::default()));
        let host = DerivedStateHost::builder()
            .with_session_id(session_id)
            .with_replay_source(replay_source)
            .add_consumer(ReplayCheckpointConsumer {
                state: Arc::clone(&state),
                checkpoint: Some(DerivedStateCheckpoint {
                    session_id,
                    last_applied_sequence: FeedSequence(0),
                    watermarks: FeedWatermarks {
                        canonical_tip_slot: Some(10),
                        processed_slot: Some(10),
                        confirmed_slot: Some(9),
                        finalized_slot: Some(8),
                    },
                    state_version: 3,
                    extension_version: "replay-checkpoint-test".to_owned(),
                }),
            })
            .build();

        host.initialize();

        let state = state
            .lock()
            .expect("replay recording state mutex should not be poisoned");
        assert_eq!(state.envelopes.len(), 1);
        assert_eq!(state.envelopes[0].sequence, FeedSequence(1));
        assert_eq!(host.healthy_consumer_count(), 1);
        assert_eq!(host.fault_count(), 0);
        assert_eq!(
            host.consumer_telemetry(),
            vec![DerivedStateConsumerTelemetry {
                name: "replay-checkpoint",
                unhealthy: false,
                recovery_state: DerivedStateConsumerRecoveryState::Live,
                applied_events: 1,
                checkpoint_flushes: 0,
                fault_count: 0,
                last_applied_sequence: Some(FeedSequence(1)),
                last_fault_sequence: None,
                last_fault_kind: None,
            }]
        );
    }

    #[test]
    fn initialize_accepts_checkpoint_already_at_retained_tail() {
        let replay_source = Arc::new(InMemoryDerivedStateReplaySource::new());
        let session_id = FeedSessionId(41);
        replay_source.append(DerivedStateFeedEnvelope {
            session_id,
            sequence: FeedSequence(0),
            emitted_at: SystemTime::UNIX_EPOCH,
            watermarks: FeedWatermarks::default(),
            event: DerivedStateFeedEvent::SlotStatusChanged(SlotStatusChangedEvent {
                slot: 5,
                parent_slot: Some(4),
                previous_status: None,
                status: ForkSlotStatus::Processed,
            }),
        });
        replay_source.append(DerivedStateFeedEnvelope {
            session_id,
            sequence: FeedSequence(1),
            emitted_at: SystemTime::UNIX_EPOCH,
            watermarks: FeedWatermarks::default(),
            event: DerivedStateFeedEvent::CheckpointBarrier(CheckpointBarrierEvent {
                barrier_sequence: FeedSequence(1),
                reason: CheckpointBarrierReason::Periodic,
            }),
        });

        let state = Arc::new(Mutex::new(RecordingState::default()));
        let host = DerivedStateHost::builder()
            .with_session_id(FeedSessionId(999))
            .with_replay_source(replay_source)
            .add_consumer(ReplayCheckpointConsumer {
                state: Arc::clone(&state),
                checkpoint: Some(DerivedStateCheckpoint {
                    session_id,
                    last_applied_sequence: FeedSequence(1),
                    watermarks: FeedWatermarks::default(),
                    state_version: 3,
                    extension_version: "replay-checkpoint-test".to_owned(),
                }),
            })
            .build();

        host.initialize();

        let state = state
            .lock()
            .expect("replay recording state mutex should not be poisoned");
        assert!(state.envelopes.is_empty());
        assert_eq!(host.healthy_consumer_count(), 1);
        assert!(!host.has_unhealthy_consumers());
    }

    #[test]
    fn runtime_installed_replay_source_replays_checkpoint_tail() {
        let replay_source = Arc::new(InMemoryDerivedStateReplaySource::new());
        let session_id = FeedSessionId(52);
        replay_source.append(DerivedStateFeedEnvelope {
            session_id,
            sequence: FeedSequence(1),
            emitted_at: SystemTime::UNIX_EPOCH,
            watermarks: FeedWatermarks {
                canonical_tip_slot: Some(12),
                processed_slot: Some(12),
                confirmed_slot: Some(11),
                finalized_slot: Some(10),
            },
            event: DerivedStateFeedEvent::SlotStatusChanged(SlotStatusChangedEvent {
                slot: 12,
                parent_slot: Some(11),
                previous_status: None,
                status: ForkSlotStatus::Processed,
            }),
        });

        let state = Arc::new(Mutex::new(RecordingState::default()));
        let host = DerivedStateHost::builder()
            .with_session_id(session_id)
            .add_consumer(ReplayCheckpointConsumer {
                state: Arc::clone(&state),
                checkpoint: Some(DerivedStateCheckpoint {
                    session_id,
                    last_applied_sequence: FeedSequence(0),
                    watermarks: FeedWatermarks {
                        canonical_tip_slot: Some(11),
                        processed_slot: Some(11),
                        confirmed_slot: Some(10),
                        finalized_slot: Some(9),
                    },
                    state_version: 3,
                    extension_version: "replay-checkpoint-test".to_owned(),
                }),
            })
            .build();

        assert!(host.install_runtime_replay_source(replay_source));
        host.initialize();

        let state = state
            .lock()
            .expect("replay recording state mutex should not be poisoned");
        assert_eq!(state.envelopes.len(), 1);
        assert_eq!(state.envelopes[0].sequence, FeedSequence(1));
        assert_eq!(host.healthy_consumer_count(), 1);
    }

    #[test]
    fn initialize_marks_consumer_unhealthy_when_replay_gap_exists() {
        let replay_source = Arc::new(InMemoryDerivedStateReplaySource::new());
        let session_id = FeedSessionId(77);
        replay_source.append(DerivedStateFeedEnvelope {
            session_id,
            sequence: FeedSequence(2),
            emitted_at: SystemTime::UNIX_EPOCH,
            watermarks: FeedWatermarks::default(),
            event: DerivedStateFeedEvent::SlotStatusChanged(SlotStatusChangedEvent {
                slot: 20,
                parent_slot: Some(19),
                previous_status: None,
                status: ForkSlotStatus::Processed,
            }),
        });

        let state = Arc::new(Mutex::new(RecordingState::default()));
        let host = DerivedStateHost::builder()
            .with_session_id(FeedSessionId(78))
            .with_replay_source(replay_source)
            .add_consumer(ReplayCheckpointConsumer {
                state,
                checkpoint: Some(DerivedStateCheckpoint {
                    session_id,
                    last_applied_sequence: FeedSequence(0),
                    watermarks: FeedWatermarks::default(),
                    state_version: 3,
                    extension_version: "replay-checkpoint-test".to_owned(),
                }),
            })
            .build();

        host.initialize();

        assert!(host.has_unhealthy_consumers());
        assert_eq!(host.unhealthy_consumer_names(), vec!["replay-checkpoint"]);
        assert!(host.has_consumers_requiring_resync());
        assert_eq!(host.consumers_requiring_resync(), vec!["replay-checkpoint"]);
        assert!(host.consumers_pending_recovery().is_empty());
        assert_eq!(
            host.consumers_requiring_rebuild(),
            vec!["replay-checkpoint"]
        );
        assert_eq!(host.fault_count(), 1);
        assert_eq!(
            host.consumer_telemetry(),
            vec![DerivedStateConsumerTelemetry {
                name: "replay-checkpoint",
                unhealthy: true,
                recovery_state: DerivedStateConsumerRecoveryState::RebuildRequired,
                applied_events: 0,
                checkpoint_flushes: 0,
                fault_count: 1,
                last_applied_sequence: None,
                last_fault_sequence: Some(FeedSequence(1)),
                last_fault_kind: Some(DerivedStateConsumerFaultKind::ReplayGap),
            }]
        );
    }

    #[test]
    fn builder_replay_source_takes_precedence_over_runtime_source() {
        let configured_replay_source = Arc::new(InMemoryDerivedStateReplaySource::new());
        let runtime_replay_source = Arc::new(InMemoryDerivedStateReplaySource::new());
        let host = DerivedStateHost::builder()
            .with_replay_source(configured_replay_source.clone())
            .add_consumer(ReplayCheckpointConsumer {
                state: Arc::new(Mutex::new(RecordingState::default())),
                checkpoint: None,
            })
            .build();

        assert!(!host.install_runtime_replay_source(runtime_replay_source.clone()));

        host.on_slot_status(SlotStatusEvent {
            slot: 40,
            tip_slot: Some(40),
            confirmed_slot: Some(39),
            finalized_slot: Some(38),
            parent_slot: Some(39),
            status: ForkSlotStatus::Processed,
            previous_status: None,
        });

        assert_eq!(
            configured_replay_source.retained_envelopes(host.session_id()),
            1
        );
        assert_eq!(
            runtime_replay_source.retained_envelopes(host.session_id()),
            0
        );
    }

    #[test]
    fn recover_consumers_replays_from_checkpoint_after_live_failure() {
        let replay_source = Arc::new(InMemoryDerivedStateReplaySource::new());
        let state = Arc::new(Mutex::new(RecoverableConsumerState {
            checkpoint: None,
            fail_sequence_once: Some(FeedSequence(2)),
            applied_sequences: Vec::new(),
        }));
        let host = DerivedStateHost::builder()
            .with_replay_source(replay_source)
            .add_consumer(RecoverableConsumer {
                state: Arc::clone(&state),
            })
            .build();

        host.on_slot_status(SlotStatusEvent {
            slot: 10,
            tip_slot: Some(10),
            confirmed_slot: Some(9),
            finalized_slot: Some(8),
            parent_slot: Some(9),
            status: ForkSlotStatus::Processed,
            previous_status: None,
        });
        host.emit_checkpoint_barrier(
            CheckpointBarrierReason::Periodic,
            FeedWatermarks {
                canonical_tip_slot: Some(10),
                processed_slot: Some(10),
                confirmed_slot: Some(9),
                finalized_slot: Some(8),
            },
        );
        host.on_slot_status(SlotStatusEvent {
            slot: 11,
            tip_slot: Some(11),
            confirmed_slot: Some(10),
            finalized_slot: Some(9),
            parent_slot: Some(10),
            status: ForkSlotStatus::Processed,
            previous_status: None,
        });

        assert_eq!(
            host.consumers_pending_recovery(),
            vec!["recoverable-consumer"]
        );
        assert!(host.consumers_requiring_rebuild().is_empty());

        let recovery_report = host.recover_consumers();
        assert_eq!(
            recovery_report,
            DerivedStateRecoveryReport {
                attempted: 1,
                recovered: 1,
                still_pending: 0,
                rebuild_required: 0,
            }
        );
        assert_eq!(host.healthy_consumer_count(), 1);
        assert!(!host.has_unhealthy_consumers());
        assert!(host.consumers_pending_recovery().is_empty());

        let state = state
            .lock()
            .expect("recoverable consumer state mutex should not be poisoned");
        assert_eq!(
            state.applied_sequences,
            vec![FeedSequence(0), FeedSequence(1), FeedSequence(2)]
        );
        assert_eq!(
            state
                .checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.last_applied_sequence),
            Some(FeedSequence(1))
        );
        assert_eq!(
            host.consumer_telemetry(),
            vec![DerivedStateConsumerTelemetry {
                name: "recoverable-consumer",
                unhealthy: false,
                recovery_state: DerivedStateConsumerRecoveryState::Live,
                applied_events: 3,
                checkpoint_flushes: 1,
                fault_count: 1,
                last_applied_sequence: Some(FeedSequence(2)),
                last_fault_sequence: Some(FeedSequence(2)),
                last_fault_kind: Some(DerivedStateConsumerFaultKind::ConsumerApplyFailed),
            }]
        );
    }

    #[test]
    fn disk_replay_source_replays_after_reopen() {
        let replay_dir = unique_test_replay_dir("reopen");
        let session_id = FeedSessionId(1337);
        let replay_source = DiskDerivedStateReplaySource::new(&replay_dir, 4)
            .expect("disk replay source should create its root directory");
        replay_source.append(DerivedStateFeedEnvelope {
            session_id,
            sequence: FeedSequence(0),
            emitted_at: SystemTime::UNIX_EPOCH,
            watermarks: FeedWatermarks {
                canonical_tip_slot: Some(60),
                processed_slot: Some(60),
                confirmed_slot: Some(59),
                finalized_slot: Some(58),
            },
            event: DerivedStateFeedEvent::SlotStatusChanged(SlotStatusChangedEvent {
                slot: 60,
                parent_slot: Some(59),
                previous_status: None,
                status: ForkSlotStatus::Processed,
            }),
        });
        replay_source.append(DerivedStateFeedEnvelope {
            session_id,
            sequence: FeedSequence(1),
            emitted_at: SystemTime::UNIX_EPOCH,
            watermarks: FeedWatermarks {
                canonical_tip_slot: Some(61),
                processed_slot: Some(61),
                confirmed_slot: Some(60),
                finalized_slot: Some(59),
            },
            event: DerivedStateFeedEvent::CheckpointBarrier(CheckpointBarrierEvent {
                barrier_sequence: FeedSequence(1),
                reason: CheckpointBarrierReason::Periodic,
            }),
        });
        drop(replay_source);

        let reopened = DiskDerivedStateReplaySource::new(&replay_dir, 4)
            .expect("disk replay source should reopen persisted logs");
        let replayed = reopened
            .replay_from(session_id, FeedSequence(0))
            .expect("reopened disk replay source should replay the retained session");
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0].sequence, FeedSequence(0));
        assert_eq!(replayed[1].sequence, FeedSequence(1));

        drop(fs::remove_dir_all(replay_dir));
    }

    #[test]
    fn disk_replay_source_persists_truncation_window() {
        let replay_dir = unique_test_replay_dir("truncate");
        let session_id = FeedSessionId(1440);
        let replay_source = DiskDerivedStateReplaySource::new(&replay_dir, 2)
            .expect("disk replay source should create its root directory");
        for (sequence, slot) in [
            (FeedSequence(0), 70),
            (FeedSequence(1), 71),
            (FeedSequence(2), 72),
        ] {
            replay_source.append(DerivedStateFeedEnvelope {
                session_id,
                sequence,
                emitted_at: SystemTime::UNIX_EPOCH,
                watermarks: FeedWatermarks {
                    canonical_tip_slot: Some(slot),
                    processed_slot: Some(slot),
                    confirmed_slot: Some(slot.saturating_sub(1)),
                    finalized_slot: Some(slot.saturating_sub(2)),
                },
                event: DerivedStateFeedEvent::SlotStatusChanged(SlotStatusChangedEvent {
                    slot,
                    parent_slot: slot.checked_sub(1),
                    previous_status: None,
                    status: ForkSlotStatus::Processed,
                }),
            });
        }
        drop(replay_source);

        let reopened = DiskDerivedStateReplaySource::new(&replay_dir, 2)
            .expect("disk replay source should reopen persisted logs");
        assert_eq!(reopened.retained_envelopes(session_id), 2);
        let replay_result = reopened.replay_from(session_id, FeedSequence(0));
        assert!(matches!(
            replay_result,
            Err(DerivedStateReplayError::Truncated {
                session_id: _,
                sequence: FeedSequence(0),
                oldest_retained_sequence: FeedSequence(1),
            })
        ));

        drop(fs::remove_dir_all(replay_dir));
    }

    #[test]
    fn bounded_replay_source_truncates_old_envelopes_and_marks_replay_gap() {
        let replay_source =
            Arc::new(InMemoryDerivedStateReplaySource::with_max_envelopes_per_session(1));
        let session_id = FeedSessionId(91);
        for (sequence, slot) in [
            (FeedSequence(0), 30),
            (FeedSequence(1), 31),
            (FeedSequence(2), 32),
        ] {
            replay_source.append(DerivedStateFeedEnvelope {
                session_id,
                sequence,
                emitted_at: SystemTime::UNIX_EPOCH,
                watermarks: FeedWatermarks {
                    canonical_tip_slot: Some(slot),
                    processed_slot: Some(slot),
                    confirmed_slot: Some(slot.saturating_sub(1)),
                    finalized_slot: Some(slot.saturating_sub(2)),
                },
                event: DerivedStateFeedEvent::SlotStatusChanged(SlotStatusChangedEvent {
                    slot,
                    parent_slot: slot.checked_sub(1),
                    previous_status: None,
                    status: ForkSlotStatus::Processed,
                }),
            });
        }

        assert_eq!(replay_source.retained_envelopes(session_id), 1);
        assert_eq!(replay_source.truncated_envelopes(), 2);
        let replayed = replay_source
            .replay_from(session_id, FeedSequence(2))
            .expect("bounded replay source should retain the requested tail");
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].sequence, FeedSequence(2));
        assert_eq!(replayed[0].watermarks.canonical_tip_slot, Some(32));

        let state = Arc::new(Mutex::new(RecordingState::default()));
        let host = DerivedStateHost::builder()
            .with_session_id(session_id)
            .with_replay_source(replay_source)
            .add_consumer(ReplayCheckpointConsumer {
                state,
                checkpoint: Some(DerivedStateCheckpoint {
                    session_id,
                    last_applied_sequence: FeedSequence(0),
                    watermarks: FeedWatermarks {
                        canonical_tip_slot: Some(30),
                        processed_slot: Some(30),
                        confirmed_slot: Some(29),
                        finalized_slot: Some(28),
                    },
                    state_version: 3,
                    extension_version: "replay-checkpoint-test".to_owned(),
                }),
            })
            .build();

        host.initialize();

        assert!(host.has_unhealthy_consumers());
        assert_eq!(host.unhealthy_consumer_names(), vec!["replay-checkpoint"]);
        assert!(host.has_consumers_requiring_resync());
        assert_eq!(host.consumers_requiring_resync(), vec!["replay-checkpoint"]);
        assert!(host.consumers_pending_recovery().is_empty());
        assert_eq!(
            host.consumers_requiring_rebuild(),
            vec!["replay-checkpoint"]
        );
        assert_eq!(host.fault_count(), 1);
        assert_eq!(
            host.consumer_telemetry(),
            vec![DerivedStateConsumerTelemetry {
                name: "replay-checkpoint",
                unhealthy: true,
                recovery_state: DerivedStateConsumerRecoveryState::RebuildRequired,
                applied_events: 0,
                checkpoint_flushes: 0,
                fault_count: 1,
                last_applied_sequence: None,
                last_fault_sequence: Some(FeedSequence(1)),
                last_fault_kind: Some(DerivedStateConsumerFaultKind::ReplayGap),
            }]
        );
    }
}
