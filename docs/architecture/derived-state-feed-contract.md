# Derived-State Feed Contract

This document defines the concrete target contract for the dedicated derived-state feed proposed
by ADR-0010.

It is written as the minimum contract an official stateful extension, such as a local bank or
geyser-like materializer, should be able to depend on.

## Scope

This feed is for authoritative state derivation.

It is not the replacement for:

1. `ObserverPlugin` observational hooks, or
2. `RuntimeExtension` capability/resource management.

It is the stateful-consumer substrate that sits beside those systems.

## Design Goals

1. deterministic ordering,
2. explicit rollback handling,
3. replayability from checkpoints/logs,
4. no silent event loss for authoritative consumers,
5. bounded runtime behavior with explicit lag/fault semantics.

## Feed Model

The feed is a totally ordered stream within one runtime session.

Every event is wrapped in one envelope carrying:

1. session identity,
2. monotonic sequence number,
3. watermark snapshot,
4. event payload.

Suggested shape:

```rust
pub struct DerivedStateFeedEnvelope {
    pub session_id: FeedSessionId,
    pub sequence: FeedSequence,
    pub emitted_at: std::time::SystemTime,
    pub watermarks: FeedWatermarks,
    pub event: DerivedStateFeedEvent,
}
```

```rust
pub struct FeedSessionId(pub uuid::Uuid);

pub struct FeedSequence(pub u64);

pub struct FeedWatermarks {
    pub canonical_tip_slot: Option<u64>,
    pub processed_slot: Option<u64>,
    pub confirmed_slot: Option<u64>,
    pub finalized_slot: Option<u64>,
}
```

## Event Families

The feed should carry the following event families.

### 1. `TransactionApplied`

Primary decoded-state input for transaction-derived materialization.

Suggested shape:

```rust
pub struct TransactionApplied {
    pub slot: u64,
    pub tx_index: u32,
    pub signature: Option<solana_signature::Signature>,
    pub kind: sof::event::TxKind,
    pub transaction: Arc<solana_transaction::versioned::VersionedTransaction>,
    pub commitment_status: sof::event::TxCommitmentStatus,
}
```

Semantics:

1. ordered by `(sequence)` globally and `(slot, tx_index)` semantically,
2. emitted only after transaction decode succeeds,
3. initially provisional unless already emitted with stronger commitment metadata,
4. later slot/reorg events may invalidate downstream derived state that consumed it.

### 2. `SlotStatusChanged`

Primary slot lifecycle input for state transitions.

Suggested shape:

```rust
pub struct SlotStatusChanged {
    pub slot: u64,
    pub parent_slot: Option<u64>,
    pub status: sof::framework::SlotStatus,
    pub canonical_tip_slot: Option<u64>,
    pub confirmed_slot: Option<u64>,
    pub finalized_slot: Option<u64>,
}
```

Semantics:

1. signals slot progression and commitment movement,
2. defines when state can be considered processed, confirmed, finalized, or orphaned,
3. may trigger output promotion or invalidation in consumers.

### 3. `BranchReorged`

Primary rollback signal.

Suggested shape:

```rust
pub struct BranchReorged {
    pub old_tip: u64,
    pub new_tip: u64,
    pub common_ancestor: Option<u64>,
    pub detached_slots: Arc<Vec<u64>>,
    pub attached_slots: Arc<Vec<u64>>,
}
```

Semantics:

1. explicitly informs the consumer that previously observed provisional branch state changed,
2. requires the consumer to rewind, revoke, or rebuild affected state,
3. is authoritative over prior provisional derived outputs.

### 4. `AccountTouchObserved`

Optional derivative signal for index/invalidation systems.

Suggested shape:

```rust
pub struct AccountTouchObserved {
    pub slot: u64,
    pub tx_index: u32,
    pub signature: Option<solana_signature::Signature>,
    pub account_keys: Arc<Vec<solana_pubkey::Pubkey>>,
    pub writable_account_keys: Arc<Vec<solana_pubkey::Pubkey>>,
    pub readonly_account_keys: Arc<Vec<solana_pubkey::Pubkey>>,
    pub lookup_table_account_keys: Arc<Vec<solana_pubkey::Pubkey>>,
}
```

Semantics:

1. derivative from transaction decode,
2. useful for invalidation/discovery/index updates,
3. not a substitute for account-write semantics,
4. not a primary truth source for bank-like state.

### 5. `CheckpointBarrier`

Explicit consumer checkpoint opportunity.

Suggested shape:

```rust
pub struct CheckpointBarrier {
    pub barrier_sequence: FeedSequence,
    pub reason: CheckpointBarrierReason,
}

pub enum CheckpointBarrierReason {
    Periodic,
    ShutdownRequested,
    ReplayBoundary,
}
```

Semantics:

1. allows the consumer to persist a durable checkpoint at a known contiguous position,
2. may be emitted periodically and during clean shutdown,
3. may also mark replay boundaries.

## Ordering Contract

The feed ordering rules should be:

1. `sequence` is the sole canonical ordering key for consumers,
2. `sequence` is contiguous within a session unless the runtime enters a declared fault state,
3. consumers must not infer ordering from wall-clock time,
4. `(slot, tx_index)` is meaningful domain metadata but not the transport ordering key.

Implication:

1. replay is correct when events are re-applied in `sequence` order,
2. checkpoint correctness is defined in terms of the highest contiguous `sequence` fully applied.

## Replay Model

Replay should be modeled around:

1. session identity,
2. feed sequence,
3. checkpoints,
4. a retained ordered event source.

Suggested consumer checkpoint shape:

```rust
pub struct DerivedStateCheckpoint {
    pub session_id: FeedSessionId,
    pub last_applied_sequence: FeedSequence,
    pub watermarks: FeedWatermarks,
    pub state_version: u32,
    pub extension_version: String,
}
```

### Recovery Rules

1. If the runtime can provide retained events after `last_applied_sequence`, the consumer may
   resume by replaying from the next sequence.
2. If the runtime cannot provide retained events for that point, the consumer must perform a full
   rebuild or restore from an older compatible snapshot plus replay.
3. A checkpoint from one `session_id` must not be resumed against another session without an
   explicit replay source that bridges the sessions.

### Exactness

An official derived-state extension should declare one of:

1. exact replay,
2. checkpoint + log exact replay,
3. best-effort rebuild only.

For a bank-like official extension, the bar should be:

1. exact replay, or
2. checkpoint + log exact replay.

## Watermark Semantics

Watermarks are consumer-visible runtime truth snapshots.

They are not just metrics. They tell the consumer what the runtime currently believes about slot
progress.

Required watermarks:

1. `canonical_tip_slot`
2. `processed_slot`
3. `confirmed_slot`
4. `finalized_slot`

Consumer expectations:

1. provisional outputs may exist above `confirmed_slot`,
2. outputs at or below `finalized_slot` should be considered stable unless the runtime itself
   declares a deeper integrity fault,
3. reorg handling primarily affects data above the durable commitment floor.

## Lag and Backpressure Policy

Authoritative consumers must not silently drop feed events.

Therefore the feed policy should be:

1. bounded queues are still allowed,
2. queue exhaustion is a declared consumer fault,
3. the runtime records the fault and stops normal live apply for that consumer,
4. the consumer must resync from checkpoint or rebuild.

This is different from `ObserverPlugin`, where event drops are acceptable to protect ingest
latency.

### Required Runtime Signals

At minimum, runtime should expose:

1. `derived_state_consumer_queue_depth`
2. `derived_state_consumer_lag_events`
3. `derived_state_consumer_last_applied_sequence`
4. `derived_state_consumer_fault_total`
5. `derived_state_consumer_resync_total`

Suggested structured fault categories:

1. `LagExceeded`
2. `QueueOverflow`
3. `CheckpointWriteFailed`
4. `ReplayGap`
5. `ConsumerApplyFailed`

## Failure Model

The consumer failure policy should be explicit.

### Non-Terminal

1. transient checkpoint write retry,
2. slow-but-bounded lag below fault threshold,
3. temporary output backpressure handled entirely within the extension.

### Terminal for Live Feed Continuity

1. queue overflow,
2. replay gap after checkpoint,
3. unrecoverable apply failure,
4. corrupted or incompatible checkpoint,
5. lag beyond configured fault threshold.

When a terminal continuity failure occurs:

1. runtime marks the consumer unhealthy,
2. runtime stops pretending live continuity exists,
3. extension must rebuild or resync before it can resume authoritative operation.

## Consumer API Expectations

The current SOF consumer surface supports this lifecycle:

```rust
trait DerivedStateConsumer {
    fn config(&self) -> DerivedStateConsumerConfig;
    fn on_startup(&mut self, ctx: DerivedStateConsumerStartupContext)
        -> Result<(), DerivedStateConsumerStartupError>;
    fn load_checkpoint(&mut self) -> Result<Option<DerivedStateCheckpoint>, ConsumerError>;
    fn apply(&mut self, event: &DerivedStateFeedEnvelope) -> Result<(), ConsumerError>;
    fn flush_checkpoint(
        &mut self,
        checkpoint: DerivedStateCheckpoint,
    ) -> Result<(), ConsumerError>;
    fn on_shutdown(&mut self, ctx: DerivedStateConsumerShutdownContext);
}
```

Important details:

1. `config()` is static host-construction metadata for optional feed families,
2. `load_checkpoint()` and `flush_checkpoint()` remain the durability boundary,
3. `on_startup()` and `on_shutdown()` are optional lifecycle hooks,
4. slot status, reorg, and checkpoint barrier events remain part of the authoritative core feed.

The lifecycle should remain:

1. load durable state,
2. apply ordered events,
3. persist checkpoints,
4. re-enter replay/resync when continuity is broken.

## Test Requirements

Any implementation of the feed should prove:

1. live apply == replay apply from the same ordered source,
2. reorgs produce identical end state in live and replay modes,
3. checkpoint + resume is identical to uninterrupted live apply,
4. lag overflow transitions the consumer into explicit fault state,
5. no silent loss occurs in the authoritative feed path.
