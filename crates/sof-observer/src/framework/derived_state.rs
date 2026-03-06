//! Experimental derived-state feed types for official stateful extensions.
//!
//! This module is the first code scaffold for the architecture proposed in
//! `docs/architecture/adr/0010-dedicated-derived-state-feed.md`.
//! It defines the feed envelope, event families, checkpoints, and consumer-facing
//! fault types without yet wiring a runtime producer.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use thiserror::Error;

use crate::{
    event::{ForkSlotStatus, TxCommitmentStatus, TxKind},
    framework::{AccountTouchEvent, ReorgEvent, SlotStatusEvent, TransactionEvent},
};

/// One runtime feed session identity.
///
/// A new session id is expected whenever feed continuity cannot be assumed across process
/// lifetimes or replay sources.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FeedSessionId(pub u128);

/// Monotonic sequence number for the derived-state feed within one session.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
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
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub enum DerivedStateFeedEvent {
    /// Decoded transaction apply record.
    TransactionApplied(TransactionAppliedEvent),
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
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
#[derive(Debug, Clone, Eq, PartialEq)]
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
#[derive(Debug, Clone, Eq, PartialEq)]
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
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct CheckpointBarrierEvent {
    /// Highest contiguous sequence fully covered by the barrier.
    pub barrier_sequence: FeedSequence,
    /// Why the barrier was emitted.
    pub reason: CheckpointBarrierReason,
}

/// Reasons for emitting a checkpoint barrier.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CheckpointBarrierReason {
    /// Periodic background checkpoint opportunity.
    Periodic,
    /// Runtime is beginning graceful shutdown.
    ShutdownRequested,
    /// Replay catch-up hit a stable boundary.
    ReplayBoundary,
}

/// Durable checkpoint shape for one derived-state consumer.
#[derive(Debug, Clone, Eq, PartialEq)]
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
            applied_events: AtomicU64::new(0),
            checkpoint_flushes: AtomicU64::new(0),
            fault_count: AtomicU64::new(0),
            last_applied_sequence: AtomicU64::new(0),
            has_last_applied_sequence: AtomicBool::new(false),
            last_fault_sequence: AtomicU64::new(0),
            has_last_fault_sequence: AtomicBool::new(false),
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

    /// Returns per-consumer live-feed telemetry snapshots in registration order.
    #[must_use]
    pub fn consumer_telemetry(&self) -> Vec<DerivedStateConsumerTelemetry> {
        self.inner
            .consumers
            .iter()
            .map(RegisteredDerivedStateConsumer::telemetry)
            .collect()
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
        if fault.kind.breaks_live_continuity() && registered.mark_unhealthy() {
            tracing::warn!(
                consumer = registered.name,
                ?fault.kind,
                sequence = ?fault.sequence,
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
}

impl RegisteredDerivedStateConsumer {
    /// Returns whether this consumer has lost live continuity.
    #[must_use]
    fn is_unhealthy(&self) -> bool {
        self.unhealthy.load(Ordering::Acquire)
    }

    /// Marks the consumer unhealthy and returns whether the state changed.
    #[must_use]
    fn mark_unhealthy(&self) -> bool {
        !self.unhealthy.swap(true, Ordering::AcqRel)
    }

    /// Records one successful envelope application.
    fn note_applied(&self, sequence: FeedSequence) {
        let _ = self.applied_events.fetch_add(1, Ordering::Relaxed);
        self.last_applied_sequence
            .store(sequence.0, Ordering::Release);
        self.has_last_applied_sequence
            .store(true, Ordering::Release);
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

    /// Builds a point-in-time telemetry snapshot for this consumer.
    #[must_use]
    fn telemetry(&self) -> DerivedStateConsumerTelemetry {
        DerivedStateConsumerTelemetry {
            name: self.name,
            unhealthy: self.is_unhealthy(),
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
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering as AtomicOrdering},
        },
        thread,
    };

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
                applied_events: 2,
                checkpoint_flushes: 1,
                fault_count: 0,
                last_applied_sequence: Some(FeedSequence(1)),
                last_fault_sequence: None,
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
        assert_eq!(host.fault_count(), 1);
        assert_eq!(
            host.consumer_telemetry(),
            vec![DerivedStateConsumerTelemetry {
                name: "failing-apply",
                unhealthy: true,
                applied_events: 0,
                checkpoint_flushes: 0,
                fault_count: 1,
                last_applied_sequence: None,
                last_fault_sequence: Some(FeedSequence(0)),
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
                applied_events: 3,
                checkpoint_flushes: 0,
                fault_count: 1,
                last_applied_sequence: Some(FeedSequence(2)),
                last_fault_sequence: Some(FeedSequence(1)),
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
                applied_events: 1,
                checkpoint_flushes: 0,
                fault_count: 0,
                last_applied_sequence: Some(FeedSequence(1)),
                last_fault_sequence: None,
            }]
        );
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
        assert_eq!(host.fault_count(), 1);
        assert_eq!(
            host.consumer_telemetry(),
            vec![DerivedStateConsumerTelemetry {
                name: "replay-checkpoint",
                unhealthy: true,
                applied_events: 0,
                checkpoint_flushes: 0,
                fault_count: 1,
                last_applied_sequence: None,
                last_fault_sequence: Some(FeedSequence(1)),
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
        assert_eq!(host.fault_count(), 1);
        assert_eq!(
            host.consumer_telemetry(),
            vec![DerivedStateConsumerTelemetry {
                name: "replay-checkpoint",
                unhealthy: true,
                applied_events: 0,
                checkpoint_flushes: 0,
                fault_count: 1,
                last_applied_sequence: None,
                last_fault_sequence: Some(FeedSequence(1)),
            }]
        );
    }
}
