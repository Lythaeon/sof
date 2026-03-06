//! Experimental derived-state feed types for official stateful extensions.
//!
//! This module is the first code scaffold for the architecture proposed in
//! `docs/architecture/adr/0010-dedicated-derived-state-feed.md`.
//! It defines the feed envelope, event families, checkpoints, and consumer-facing
//! fault types without yet wiring a runtime producer.

use std::{sync::Arc, time::SystemTime};

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

/// Stateful consumer interface for the dedicated derived-state feed scaffold.
///
/// This trait is intentionally synchronous for the initial scaffold so implementers can model
/// deterministic state application and checkpointing before runtime dispatch details are fixed.
pub trait DerivedStateConsumer: Send + Sync + 'static {
    /// Stable consumer name used in logs and telemetry.
    fn name(&self) -> &'static str;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
