#![allow(clippy::assertions_on_constants, clippy::missing_const_for_fn)]
#![doc = "Runtime restart test for derived-state replay recovery."]
#![cfg(feature = "kernel-bypass")]

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use sof::{
    framework::{
        DerivedStateCheckpoint, DerivedStateConsumer, DerivedStateConsumerFault,
        DerivedStateConsumerFaultKind, DerivedStateFeedEnvelope, DerivedStateFeedEvent,
        DerivedStateHost, DerivedStateReplayBackend, DerivedStateReplayDurability, FeedSequence,
        FeedSessionId,
    },
    ingest::{RawPacket, RawPacketBatch},
    protocol::shred_wire::{SIZE_OF_DATA_SHRED_PAYLOAD, VARIANT_MERKLE_DATA},
    runtime::{self, DerivedStateReplayConfig, DerivedStateRuntimeConfig, RuntimeSetup},
    shred::wire::SIZE_OF_DATA_SHRED_HEADERS,
};
use tokio::sync::mpsc;

const SHRED_PAYLOAD_BYTES: usize = 128;
const SHRED_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum AppliedEventKind {
    SlotStatus,
    CheckpointBarrier,
}

#[derive(Default)]
struct AppliedEnvelopeState {
    envelopes: Vec<(FeedSessionId, FeedSequence, AppliedEventKind)>,
}

struct PersistedCheckpointConsumer {
    state: Arc<Mutex<AppliedEnvelopeState>>,
    checkpoint_path: PathBuf,
    lag_checkpoint_by_one: bool,
}

impl PersistedCheckpointConsumer {
    fn persisted_checkpoint(&self) -> Option<DerivedStateCheckpoint> {
        let bytes = std::fs::read(&self.checkpoint_path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

impl DerivedStateConsumer for PersistedCheckpointConsumer {
    fn name(&self) -> &'static str {
        "persisted-checkpoint-consumer"
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn extension_version(&self) -> &'static str {
        "persisted-checkpoint-consumer-e2e"
    }

    fn load_checkpoint(
        &mut self,
    ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
        Ok(self.persisted_checkpoint())
    }

    fn apply(
        &mut self,
        envelope: DerivedStateFeedEnvelope,
    ) -> Result<(), DerivedStateConsumerFault> {
        let event_kind = match envelope.event {
            DerivedStateFeedEvent::SlotStatusChanged(_) => AppliedEventKind::SlotStatus,
            DerivedStateFeedEvent::CheckpointBarrier(_) => AppliedEventKind::CheckpointBarrier,
            DerivedStateFeedEvent::TransactionApplied(_)
            | DerivedStateFeedEvent::BranchReorged(_)
            | DerivedStateFeedEvent::AccountTouchObserved(_) => return Ok(()),
        };
        self.state
            .lock()
            .map_err(|_poison| {
                DerivedStateConsumerFault::new(
                    DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                    Some(envelope.sequence),
                    "persisted-checkpoint-consumer state mutex poisoned during apply",
                )
            })?
            .envelopes
            .push((envelope.session_id, envelope.sequence, event_kind));
        Ok(())
    }

    fn flush_checkpoint(
        &mut self,
        checkpoint: DerivedStateCheckpoint,
    ) -> Result<(), DerivedStateConsumerFault> {
        let checkpoint = if self.lag_checkpoint_by_one {
            DerivedStateCheckpoint {
                last_applied_sequence: FeedSequence(
                    checkpoint.last_applied_sequence.0.saturating_sub(1),
                ),
                ..checkpoint
            }
        } else {
            checkpoint
        };
        let bytes = serde_json::to_vec(&checkpoint).map_err(|error| {
            DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                Some(checkpoint.last_applied_sequence),
                format!("failed to serialize persisted checkpoint: {error}"),
            )
        })?;
        std::fs::write(&self.checkpoint_path, bytes).map_err(|error| {
            DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                Some(checkpoint.last_applied_sequence),
                format!("failed to write persisted checkpoint: {error}"),
            )
        })
    }
}

fn unique_test_dir(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "sof-derived-state-runtime-{name}-{}-{unique}",
        std::process::id()
    ))
}

fn build_raw_packet(sequence: u64, source_port: u16) -> RawPacket {
    let slot = (sequence / 128).saturating_add(1);
    let index = u32::try_from(sequence % 128).unwrap_or(0);
    let fec_set_index = index;
    let declared_size =
        u16::try_from(SIZE_OF_DATA_SHRED_HEADERS.saturating_add(SHRED_PAYLOAD_BYTES))
            .unwrap_or(u16::MAX);
    let mut bytes = vec![0_u8; SIZE_OF_DATA_SHRED_PAYLOAD];

    write_bytes(&mut bytes, 0, &slot.to_le_bytes());
    write_bytes(&mut bytes, 8, &index.to_le_bytes());
    write_bytes(&mut bytes, 12, &fec_set_index.to_le_bytes());

    write_byte(&mut bytes, 64, VARIANT_MERKLE_DATA);
    write_bytes(&mut bytes, 65, &slot.to_le_bytes());
    write_bytes(&mut bytes, 73, &index.to_le_bytes());
    write_bytes(&mut bytes, 77, &SHRED_VERSION.to_le_bytes());
    write_bytes(&mut bytes, 79, &fec_set_index.to_le_bytes());
    write_bytes(&mut bytes, 83, &0_u16.to_le_bytes());
    write_byte(&mut bytes, 85, 0);
    write_bytes(&mut bytes, 86, &declared_size.to_le_bytes());
    let payload_end = SIZE_OF_DATA_SHRED_HEADERS.saturating_add(SHRED_PAYLOAD_BYTES);
    fill_bytes(&mut bytes, 88, payload_end, 0xAC);

    RawPacket {
        source: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), source_port),
        ingress: sof::ingest::RawPacketIngress::Udp,
        bytes,
    }
}

fn write_bytes(buffer: &mut [u8], offset: usize, value: &[u8]) {
    let (_, tail) = buffer.split_at_mut(offset);
    let (slot, _) = tail.split_at_mut(value.len());
    slot.copy_from_slice(value);
}

fn write_byte(buffer: &mut [u8], offset: usize, value: u8) {
    let (_, tail) = buffer.split_at_mut(offset);
    let (slot, _) = tail.split_at_mut(1);
    if let Some(first) = slot.first_mut() {
        *first = value;
    }
}

fn fill_bytes(buffer: &mut [u8], start: usize, end: usize, value: u8) {
    let (_, tail) = buffer.split_at_mut(start);
    let len = end.saturating_sub(start);
    let (slot, _) = tail.split_at_mut(len);
    slot.fill(value);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn derived_state_runtime_restart_replays_retained_tail_from_disk()
-> Result<(), Box<dyn std::error::Error>> {
    let test_dir = unique_test_dir("restart");
    let replay_dir = test_dir.join("replay");
    let checkpoint_path = test_dir.join("checkpoint.json");
    std::fs::create_dir_all(&test_dir)?;

    let setup = RuntimeSetup::new().with_derived_state_config(DerivedStateRuntimeConfig {
        checkpoint_interval_ms: 0,
        recovery_interval_ms: 50,
        replay: DerivedStateReplayConfig {
            backend: DerivedStateReplayBackend::Disk,
            replay_dir: replay_dir.clone(),
            durability: DerivedStateReplayDurability::Flush,
            max_envelopes: 64,
            max_sessions: 4,
        },
    });

    let first_run_state = Arc::new(Mutex::new(AppliedEnvelopeState::default()));
    let first_run_host = DerivedStateHost::builder()
        .add_consumer(PersistedCheckpointConsumer {
            state: Arc::clone(&first_run_state),
            checkpoint_path: checkpoint_path.clone(),
            lag_checkpoint_by_one: true,
        })
        .build();
    let first_run_session = first_run_host.session_id();
    let (first_tx, first_rx) = mpsc::channel::<RawPacketBatch>(16);
    let first_run_setup = setup.clone();
    let first_task = tokio::spawn(async move {
        runtime::run_async_with_derived_state_host_and_kernel_bypass_ingress_and_setup(
            first_run_host,
            first_rx,
            &first_run_setup,
        )
        .await
    });

    first_tx.send(vec![build_raw_packet(0, 8_899)]).await?;
    drop(first_tx);

    let first_result = tokio::time::timeout(Duration::from_secs(10), first_task)
        .await
        .map_err(|error| {
            std::io::Error::other(format!(
                "first runtime run should finish before timeout: {error}"
            ))
        })??;
    if let Err(error) = first_result {
        return Err(std::io::Error::other(format!("first runtime run failed: {error}")).into());
    }

    let persisted_checkpoint_bytes = std::fs::read(&checkpoint_path)?;
    let persisted_checkpoint: DerivedStateCheckpoint =
        serde_json::from_slice(&persisted_checkpoint_bytes)?;
    if persisted_checkpoint.session_id != first_run_session {
        return Err(std::io::Error::other(format!(
            "unexpected checkpoint session id: expected {first_run_session:?}, got {:?}",
            persisted_checkpoint.session_id
        ))
        .into());
    }
    if persisted_checkpoint.last_applied_sequence != FeedSequence(0) {
        return Err(std::io::Error::other(format!(
            "unexpected checkpoint sequence: expected {:?}, got {:?}",
            FeedSequence(0),
            persisted_checkpoint.last_applied_sequence
        ))
        .into());
    }

    let second_run_state = Arc::new(Mutex::new(AppliedEnvelopeState::default()));
    let second_run_host = DerivedStateHost::builder()
        .add_consumer(PersistedCheckpointConsumer {
            state: Arc::clone(&second_run_state),
            checkpoint_path: checkpoint_path.clone(),
            lag_checkpoint_by_one: false,
        })
        .build();
    let second_run_session = second_run_host.session_id();
    let (second_tx, second_rx) = mpsc::channel::<RawPacketBatch>(16);
    drop(second_tx);
    let second_run_setup = setup.clone();
    let second_task = tokio::spawn(async move {
        runtime::run_async_with_derived_state_host_and_kernel_bypass_ingress_and_setup(
            second_run_host,
            second_rx,
            &second_run_setup,
        )
        .await
    });

    let second_result = tokio::time::timeout(Duration::from_secs(10), second_task)
        .await
        .map_err(|error| {
            std::io::Error::other(format!(
                "second runtime run should finish before timeout: {error}"
            ))
        })??;
    if let Err(error) = second_result {
        return Err(std::io::Error::other(format!("second runtime run failed: {error}")).into());
    }

    let second_run_envelopes = second_run_state
        .lock()
        .map_err(|poison| std::io::Error::other(format!("state mutex poisoned: {poison}")))?
        .envelopes
        .clone();
    if !second_run_envelopes.contains(&(
        first_run_session,
        FeedSequence(1),
        AppliedEventKind::CheckpointBarrier,
    )) {
        return Err(std::io::Error::other(
            "second run should replay the retained shutdown barrier tail from the first session",
        )
        .into());
    }
    if !second_run_envelopes.contains(&(
        second_run_session,
        FeedSequence(0),
        AppliedEventKind::CheckpointBarrier,
    )) {
        return Err(std::io::Error::other(
            "second run should still emit its own shutdown checkpoint barrier",
        )
        .into());
    }

    drop(std::fs::remove_dir_all(test_dir));
    Ok(())
}
