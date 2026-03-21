//! Example derived-state consumer that mirrors slot-status counts to local state.

use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use sof::{
    framework::{
        DerivedStateCheckpoint, DerivedStateConsumer, DerivedStateConsumerConfig,
        DerivedStateConsumerFault, DerivedStateConsumerFaultKind,
        DerivedStateConsumerShutdownContext, DerivedStateConsumerStartupContext,
        DerivedStateConsumerStartupError, DerivedStateFeedEnvelope, DerivedStateFeedEvent,
        DerivedStateHost, DerivedStateReplayBackend, DerivedStateReplayDurability,
    },
    runtime::{self, DerivedStateReplayConfig, DerivedStateRuntimeConfig, RuntimeSetup},
};

#[derive(Default)]
/// In-memory slot-status counters keyed by status name.
struct SlotMirrorState {
    /// Number of observed slot transitions per slot status.
    slots_by_status: BTreeMap<String, u64>,
}

/// Example consumer that persists checkpoints and derives slot-status aggregates.
struct SlotMirrorConsumer {
    /// Shared in-memory aggregate state updated from the derived-state feed.
    state: Arc<Mutex<SlotMirrorState>>,
    /// Filesystem path used to persist the consumer checkpoint.
    checkpoint_path: PathBuf,
}

impl SlotMirrorConsumer {
    /// Loads the last persisted checkpoint if one has already been written.
    fn persisted_checkpoint(&self) -> Option<DerivedStateCheckpoint> {
        let bytes = fs::read(&self.checkpoint_path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

impl DerivedStateConsumer for SlotMirrorConsumer {
    fn name(&self) -> &'static str {
        "slot-mirror"
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn extension_version(&self) -> &'static str {
        "slot-mirror-example"
    }

    fn load_checkpoint(
        &mut self,
    ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
        Ok(self.persisted_checkpoint())
    }

    fn config(&self) -> DerivedStateConsumerConfig {
        DerivedStateConsumerConfig::new().with_control_plane_observed()
    }

    fn on_startup(
        &mut self,
        ctx: DerivedStateConsumerStartupContext,
    ) -> Result<(), DerivedStateConsumerStartupError> {
        tracing::info!(
            consumer = ctx.consumer_name,
            "derived-state consumer startup completed"
        );
        Ok(())
    }

    fn on_shutdown(&mut self, ctx: DerivedStateConsumerShutdownContext) {
        tracing::info!(
            consumer = ctx.consumer_name,
            "derived-state consumer shutdown completed"
        );
    }

    fn apply(
        &mut self,
        envelope: &DerivedStateFeedEnvelope,
    ) -> Result<(), DerivedStateConsumerFault> {
        if let DerivedStateFeedEvent::SlotStatusChanged(event) = &envelope.event {
            let mut state = self.state.lock().map_err(|_poison| {
                DerivedStateConsumerFault::new(
                    DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                    Some(envelope.sequence),
                    "slot mirror state mutex poisoned during apply",
                )
            })?;
            let key = format!("{:?}", event.status);
            let entry = state.slots_by_status.entry(key).or_insert(0);
            *entry = entry.saturating_add(1);
        }
        Ok(())
    }

    fn flush_checkpoint(
        &mut self,
        checkpoint: DerivedStateCheckpoint,
    ) -> Result<(), DerivedStateConsumerFault> {
        let bytes = serde_json::to_vec_pretty(&checkpoint).map_err(|error| {
            DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                Some(checkpoint.last_applied_sequence),
                format!("failed to serialize slot-mirror checkpoint: {error}"),
            )
        })?;
        if let Some(parent) = self.checkpoint_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                DerivedStateConsumerFault::new(
                    DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                    Some(checkpoint.last_applied_sequence),
                    format!("failed to create slot-mirror checkpoint directory: {error}"),
                )
            })?;
        }
        fs::write(&self.checkpoint_path, bytes).map_err(|error| {
            DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                Some(checkpoint.last_applied_sequence),
                format!("failed to write slot-mirror checkpoint: {error}"),
            )
        })
    }
}

#[tokio::main(flavor = "multi_thread")]
/// Runs the slot-mirror example when `SOF_RUN_EXAMPLE=1` is set.
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("SOF_RUN_EXAMPLE").ok().as_deref() != Some("1") {
        println!(
            "Set SOF_RUN_EXAMPLE=1 and provide normal SOF runtime config to run this example."
        );
        println!(
            "It demonstrates a persisted derived-state consumer with disk replay and restart recovery."
        );
        return Ok(());
    }

    let state = Arc::new(Mutex::new(SlotMirrorState::default()));
    let derived_state_host = DerivedStateHost::builder()
        .add_consumer(SlotMirrorConsumer {
            state: Arc::clone(&state),
            checkpoint_path: PathBuf::from("./.sof-example/slot-mirror-checkpoint.json"),
        })
        .build();

    let setup = RuntimeSetup::new().with_derived_state_config(DerivedStateRuntimeConfig {
        checkpoint_interval_ms: 5_000,
        recovery_interval_ms: 2_000,
        replay: DerivedStateReplayConfig {
            backend: DerivedStateReplayBackend::Disk,
            replay_dir: PathBuf::from("./.sof-example/replay"),
            durability: DerivedStateReplayDurability::Flush,
            max_envelopes: 32_768,
            max_sessions: 4,
        },
    });

    runtime::run_async_with_derived_state_host_and_setup(derived_state_host, &setup).await?;
    Ok(())
}
