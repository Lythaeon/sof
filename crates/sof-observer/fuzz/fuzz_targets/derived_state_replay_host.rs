#![no_main]

use std::sync::{Arc, Mutex};

use libfuzzer_sys::fuzz_target;
use sof::{
    event::ForkSlotStatus,
    framework::{
        CheckpointBarrierReason, DerivedStateCheckpoint, DerivedStateConsumer,
        DerivedStateConsumerFault, DerivedStateConsumerFaultKind, DerivedStateHost,
        DerivedStateReplaySource, FeedSequence, FeedSessionId, FeedWatermarks,
        InMemoryDerivedStateReplaySource, ReorgEvent, SlotStatusEvent,
    },
};

#[derive(Default)]
struct FuzzConsumerState {
    checkpoint: Option<DerivedStateCheckpoint>,
    fail_on_sequence: Option<FeedSequence>,
    applied_sequences: Vec<FeedSequence>,
}

struct FuzzConsumer {
    state: Arc<Mutex<FuzzConsumerState>>,
}

impl DerivedStateConsumer for FuzzConsumer {
    fn name(&self) -> &'static str {
        "fuzz-derived-state-consumer"
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn extension_version(&self) -> &'static str {
        "fuzz-derived-state-consumer-v1"
    }

    fn load_checkpoint(
        &mut self,
    ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
        let state = self.state.lock().map_err(|_poison| {
            DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                None,
                "fuzz consumer mutex poisoned while loading checkpoint",
            )
        })?;
        Ok(state.checkpoint.clone())
    }

    fn apply(
        &mut self,
        envelope: &sof::framework::DerivedStateFeedEnvelope,
    ) -> Result<(), DerivedStateConsumerFault> {
        let mut state = self.state.lock().map_err(|_poison| {
            DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                Some(envelope.sequence),
                "fuzz consumer mutex poisoned while applying envelope",
            )
        })?;
        if state.fail_on_sequence == Some(envelope.sequence) {
            state.fail_on_sequence = None;
            return Err(DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                Some(envelope.sequence),
                "fuzz consumer injected apply failure",
            ));
        }
        state.applied_sequences.push(envelope.sequence);
        Ok(())
    }

    fn flush_checkpoint(
        &mut self,
        checkpoint: DerivedStateCheckpoint,
    ) -> Result<(), DerivedStateConsumerFault> {
        let mut state = self.state.lock().map_err(|_poison| {
            DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                Some(checkpoint.last_applied_sequence),
                "fuzz consumer mutex poisoned while flushing checkpoint",
            )
        })?;
        state.checkpoint = Some(checkpoint);
        Ok(())
    }
}

fn take_bytes<'a>(input: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    if input.len() < len {
        return None;
    }
    let (head, tail) = input.split_at(len);
    *input = tail;
    Some(head)
}

fn take_u8(input: &mut &[u8]) -> Option<u8> {
    take_bytes(input, 1).map(|bytes| bytes[0])
}

fn take_u64(input: &mut &[u8]) -> Option<u64> {
    let bytes = take_bytes(input, 8)?;
    let bytes: [u8; 8] = bytes.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

fn take_status(input: &mut &[u8]) -> ForkSlotStatus {
    match take_u8(input).unwrap_or(0) % 4 {
        0 => ForkSlotStatus::Processed,
        1 => ForkSlotStatus::Confirmed,
        2 => ForkSlotStatus::Finalized,
        _ => ForkSlotStatus::Orphaned,
    }
}

fn take_reason(input: &mut &[u8]) -> CheckpointBarrierReason {
    match take_u8(input).unwrap_or(0) % 3 {
        0 => CheckpointBarrierReason::Periodic,
        1 => CheckpointBarrierReason::ShutdownRequested,
        _ => CheckpointBarrierReason::ReplayBoundary,
    }
}

fn take_watermarks(input: &mut &[u8]) -> FeedWatermarks {
    let canonical_tip_slot = take_u64(input);
    let processed_slot = take_u64(input);
    let confirmed_slot = take_u64(input);
    let finalized_slot = take_u64(input);
    FeedWatermarks {
        canonical_tip_slot,
        processed_slot,
        confirmed_slot,
        finalized_slot,
    }
}

fn build_host(
    session_id: FeedSessionId,
    state: Arc<Mutex<FuzzConsumerState>>,
    replay_source: Arc<InMemoryDerivedStateReplaySource>,
) -> DerivedStateHost {
    DerivedStateHost::builder()
        .with_session_id(session_id)
        .with_replay_source(replay_source)
        .add_consumer(FuzzConsumer { state })
        .build()
}

fuzz_target!(|bytes: &[u8]| {
    let mut input = bytes;
    let retained = usize::from(take_u8(&mut input).unwrap_or(0)).max(1);
    let replay_source = Arc::new(InMemoryDerivedStateReplaySource::with_max_envelopes_per_session(
        retained,
    ));
    let state = Arc::new(Mutex::new(FuzzConsumerState::default()));
    let mut next_session_id = u128::from(take_u64(&mut input).unwrap_or(1)).max(1);
    let mut host = build_host(FeedSessionId(next_session_id), Arc::clone(&state), Arc::clone(&replay_source));
    host.initialize();

    let op_count = usize::from(take_u8(&mut input).unwrap_or(0));
    for _ in 0..op_count {
        let Some(op) = take_u8(&mut input) else {
            break;
        };
        match op % 7 {
            0 => {
                let slot = take_u64(&mut input).unwrap_or(0);
                let tip_slot = take_u64(&mut input);
                let confirmed_slot = take_u64(&mut input);
                let finalized_slot = take_u64(&mut input);
                host.on_slot_status(SlotStatusEvent {
                    slot,
                    parent_slot: take_u64(&mut input),
                    previous_status: Some(take_status(&mut input)),
                    status: take_status(&mut input),
                    tip_slot,
                    confirmed_slot,
                    finalized_slot,
                    provider_source: None,
                });
            }
            1 => {
                let old_tip = take_u64(&mut input).unwrap_or(0);
                let new_tip = take_u64(&mut input).unwrap_or(old_tip);
                let detached_len = usize::from(take_u8(&mut input).unwrap_or(0) % 4);
                let attached_len = usize::from(take_u8(&mut input).unwrap_or(0) % 4);
                let mut detached_slots = Vec::with_capacity(detached_len);
                let mut attached_slots = Vec::with_capacity(attached_len);
                for _ in 0..detached_len {
                    detached_slots.push(take_u64(&mut input).unwrap_or(old_tip));
                }
                for _ in 0..attached_len {
                    attached_slots.push(take_u64(&mut input).unwrap_or(new_tip));
                }
                host.on_reorg(ReorgEvent {
                    old_tip,
                    new_tip,
                    common_ancestor: take_u64(&mut input),
                    detached_slots,
                    attached_slots,
                    confirmed_slot: take_u64(&mut input),
                    finalized_slot: take_u64(&mut input),
                    provider_source: None,
                });
            }
            2 => {
                host.emit_checkpoint_barrier(take_reason(&mut input), take_watermarks(&mut input));
            }
            3 => {
                if let Some(last_sequence) = host.last_emitted_sequence().and_then(FeedSequence::next)
                    && let Ok(mut guard) = state.lock()
                {
                    guard.fail_on_sequence = Some(last_sequence);
                }
            }
            4 => {
                let _ = host.recover_consumers();
            }
            5 => {
                next_session_id = next_session_id.saturating_add(1);
                host = build_host(
                    FeedSessionId(next_session_id),
                    Arc::clone(&state),
                    Arc::clone(&replay_source),
                );
                host.initialize();
            }
            _ => {
                let _ = host.consumer_telemetry();
                let _ = host.replay_telemetry();
                let _ = host.unhealthy_consumer_names();
                let _ = host.consumers_pending_recovery();
                let _ = host.consumers_requiring_rebuild();
                let _ = host.last_emitted_sequence();
            }
        }
    }

    let _ = replay_source.telemetry();
    let _ = replay_source.retained_envelopes(host.session_id());
    if let Ok(guard) = state.lock() {
        let _ = guard.applied_sequences.len();
        let _ = guard.checkpoint.clone();
    }
});
