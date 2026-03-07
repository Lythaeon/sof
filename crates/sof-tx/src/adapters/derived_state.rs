//! `sof` derived-state adapter that bridges replayable control-plane state into `sof-tx` providers.
#![allow(clippy::missing_docs_in_private_items)]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sof::framework::{
    DerivedStateCheckpoint, DerivedStateConsumer, DerivedStateConsumerFault,
    DerivedStateConsumerFaultKind, DerivedStateFeedEnvelope, DerivedStateFeedEvent,
};

use crate::{
    adapters::common::{
        TxProviderAdapterConfig, TxProviderAdapterCore, TxProviderAdapterSnapshot,
        take_next_leader_identity_targets,
    },
    providers::{LeaderProvider, LeaderTarget, RecentBlockhashProvider},
};

const DERIVED_STATE_ADAPTER_NAME: &str = "sof-tx-derived-state-provider-adapter";
const DERIVED_STATE_ADAPTER_EXTENSION_VERSION: &str = "sof-tx-derived-state-provider-adapter-v1";
const DERIVED_STATE_ADAPTER_STATE_VERSION: u32 = 1;

/// Configuration for [`DerivedStateTxProviderAdapter`].
pub type DerivedStateTxProviderAdapterConfig = TxProviderAdapterConfig;

/// File-backed persistence for [`DerivedStateTxProviderAdapter`].
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DerivedStateTxProviderAdapterPersistence {
    /// Persisted checkpoint file path.
    checkpoint_path: PathBuf,
}

impl DerivedStateTxProviderAdapterPersistence {
    /// Creates a persistence target rooted at `checkpoint_path`.
    #[must_use]
    pub fn new(checkpoint_path: impl Into<PathBuf>) -> Self {
        Self {
            checkpoint_path: checkpoint_path.into(),
        }
    }

    /// Returns the persisted checkpoint path.
    #[must_use]
    pub fn checkpoint_path(&self) -> &Path {
        &self.checkpoint_path
    }
}

/// Replayable tx-provider adapter backed by the SOF derived-state feed.
#[derive(Debug, Clone)]
pub struct DerivedStateTxProviderAdapter {
    /// Shared tx-provider state and reduction logic.
    core: TxProviderAdapterCore,
    /// Optional on-disk checkpoint persistence.
    persistence: Option<DerivedStateTxProviderAdapterPersistence>,
    /// Latest checkpoint visible to recovery logic.
    checkpoint: Arc<Mutex<Option<DerivedStateCheckpoint>>>,
}

impl DerivedStateTxProviderAdapter {
    /// Creates a new in-memory adapter.
    #[must_use]
    pub fn new(config: DerivedStateTxProviderAdapterConfig) -> Self {
        Self {
            core: TxProviderAdapterCore::new(config),
            persistence: None,
            checkpoint: Arc::new(Mutex::new(None)),
        }
    }

    /// Creates a new adapter with file-backed checkpoint persistence.
    #[must_use]
    pub fn with_persistence(
        config: DerivedStateTxProviderAdapterConfig,
        persistence: DerivedStateTxProviderAdapterPersistence,
    ) -> Self {
        Self {
            core: TxProviderAdapterCore::new(config),
            persistence: Some(persistence),
            checkpoint: Arc::new(Mutex::new(None)),
        }
    }

    /// Creates a new adapter with file-backed checkpoint persistence at `checkpoint_path`.
    #[must_use]
    pub fn with_checkpoint_path(
        config: DerivedStateTxProviderAdapterConfig,
        checkpoint_path: impl Into<PathBuf>,
    ) -> Self {
        Self::with_persistence(
            config,
            DerivedStateTxProviderAdapterPersistence::new(checkpoint_path),
        )
    }

    /// Captures a snapshot of the current provider state.
    #[must_use]
    pub fn snapshot_state(&self) -> TxProviderAdapterSnapshot {
        self.core.snapshot_state()
    }

    /// Restores provider state from a snapshot.
    pub fn restore_snapshot(&self, snapshot: TxProviderAdapterSnapshot) {
        self.core.restore_snapshot(snapshot);
    }

    /// Returns configured persistence when file-backed checkpoints are enabled.
    #[must_use]
    pub const fn persistence(&self) -> Option<&DerivedStateTxProviderAdapterPersistence> {
        self.persistence.as_ref()
    }

    fn current_checkpoint(&self) -> Option<DerivedStateCheckpoint> {
        self.checkpoint
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_checkpoint(&self, checkpoint: Option<DerivedStateCheckpoint>) {
        *self
            .checkpoint
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = checkpoint;
    }

    fn load_persisted_state(
        &self,
    ) -> Result<Option<PersistedDerivedStateTxProviderAdapter>, DerivedStateConsumerFault> {
        let Some(persistence) = self.persistence() else {
            return Ok(None);
        };
        if !persistence.checkpoint_path().exists() {
            return Ok(None);
        }
        let bytes = fs::read(persistence.checkpoint_path()).map_err(|error| {
            DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                None,
                format!(
                    "failed to read derived-state tx adapter checkpoint {}: {error}",
                    persistence.checkpoint_path().display()
                ),
            )
        })?;
        let persisted = serde_json::from_slice::<PersistedDerivedStateTxProviderAdapter>(&bytes)
            .map_err(|error| {
                DerivedStateConsumerFault::new(
                    DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                    None,
                    format!(
                        "failed to parse derived-state tx adapter checkpoint {}: {error}",
                        persistence.checkpoint_path().display()
                    ),
                )
            })?;
        Ok(Some(persisted))
    }

    fn persist_state(
        &self,
        checkpoint: &DerivedStateCheckpoint,
    ) -> Result<(), DerivedStateConsumerFault> {
        let Some(persistence) = self.persistence() else {
            return Ok(());
        };
        let persisted = PersistedDerivedStateTxProviderAdapter {
            checkpoint: Some(checkpoint.clone()),
            snapshot: self.snapshot_state(),
        };
        let bytes = serde_json::to_vec_pretty(&persisted).map_err(|error| {
            DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                Some(checkpoint.last_applied_sequence),
                format!("failed to serialize derived-state tx adapter checkpoint: {error}"),
            )
        })?;
        if let Some(parent) = persistence.checkpoint_path().parent() {
            fs::create_dir_all(parent).map_err(|error| {
                DerivedStateConsumerFault::new(
                    DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                    Some(checkpoint.last_applied_sequence),
                    format!(
                        "failed to create derived-state tx adapter checkpoint directory {}: {error}",
                        parent.display()
                    ),
                )
            })?;
        }

        let tmp_path = temp_checkpoint_path(persistence.checkpoint_path());
        fs::write(&tmp_path, bytes).map_err(|error| {
            DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                Some(checkpoint.last_applied_sequence),
                format!(
                    "failed to write temporary derived-state tx adapter checkpoint {}: {error}",
                    tmp_path.display()
                ),
            )
        })?;
        fs::rename(&tmp_path, persistence.checkpoint_path()).map_err(|error| {
            if let Err(_error) = fs::remove_file(&tmp_path) {}
            DerivedStateConsumerFault::new(
                DerivedStateConsumerFaultKind::CheckpointWriteFailed,
                Some(checkpoint.last_applied_sequence),
                format!(
                    "failed to move derived-state tx adapter checkpoint into place {}: {error}",
                    persistence.checkpoint_path().display()
                ),
            )
        })?;
        Ok(())
    }

    fn checkpoint_is_compatible(checkpoint: &DerivedStateCheckpoint) -> bool {
        checkpoint.state_version == DERIVED_STATE_ADAPTER_STATE_VERSION
            && checkpoint.extension_version == DERIVED_STATE_ADAPTER_EXTENSION_VERSION
    }
}

impl Default for DerivedStateTxProviderAdapter {
    fn default() -> Self {
        Self::new(DerivedStateTxProviderAdapterConfig::default())
    }
}

impl RecentBlockhashProvider for DerivedStateTxProviderAdapter {
    fn latest_blockhash(&self) -> Option<[u8; 32]> {
        self.core.latest_blockhash()
    }
}

impl LeaderProvider for DerivedStateTxProviderAdapter {
    fn current_leader(&self) -> Option<LeaderTarget> {
        self.core.leader_window(0).into_iter().next()
    }

    fn next_leaders(&self, n: usize) -> Vec<LeaderTarget> {
        if n == 0 {
            return Vec::new();
        }
        take_next_leader_identity_targets(self.core.leader_window(n), n)
    }
}

impl DerivedStateConsumer for DerivedStateTxProviderAdapter {
    fn name(&self) -> &'static str {
        DERIVED_STATE_ADAPTER_NAME
    }

    fn state_version(&self) -> u32 {
        DERIVED_STATE_ADAPTER_STATE_VERSION
    }

    fn extension_version(&self) -> &'static str {
        DERIVED_STATE_ADAPTER_EXTENSION_VERSION
    }

    fn load_checkpoint(
        &mut self,
    ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
        if let Some(checkpoint) = self.current_checkpoint() {
            return Ok(Some(checkpoint));
        }

        let Some(persisted) = self.load_persisted_state()? else {
            return Ok(None);
        };
        let checkpoint = persisted.checkpoint.filter(Self::checkpoint_is_compatible);
        if checkpoint.is_none() {
            return Ok(None);
        }
        self.restore_snapshot(persisted.snapshot);
        self.set_checkpoint(checkpoint.clone());
        Ok(checkpoint)
    }

    fn apply(
        &mut self,
        envelope: DerivedStateFeedEnvelope,
    ) -> Result<(), DerivedStateConsumerFault> {
        match envelope.event {
            DerivedStateFeedEvent::RecentBlockhashObserved(event) => {
                self.core.apply_recent_blockhash(&event);
            }
            DerivedStateFeedEvent::ClusterTopologyChanged(event) => {
                self.core.apply_cluster_topology(&event);
            }
            DerivedStateFeedEvent::LeaderScheduleUpdated(event) => {
                self.core.apply_leader_schedule(&event);
            }
            DerivedStateFeedEvent::SlotStatusChanged(event) => {
                self.core.apply_slot_status(event);
            }
            DerivedStateFeedEvent::BranchReorged(event) => {
                self.core.apply_reorg(&event);
            }
            DerivedStateFeedEvent::TransactionApplied(_)
            | DerivedStateFeedEvent::AccountTouchObserved(_)
            | DerivedStateFeedEvent::CheckpointBarrier(_) => {}
        }
        Ok(())
    }

    fn flush_checkpoint(
        &mut self,
        checkpoint: DerivedStateCheckpoint,
    ) -> Result<(), DerivedStateConsumerFault> {
        self.set_checkpoint(Some(checkpoint.clone()));
        self.persist_state(&checkpoint)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedDerivedStateTxProviderAdapter {
    checkpoint: Option<DerivedStateCheckpoint>,
    snapshot: TxProviderAdapterSnapshot,
}

fn temp_checkpoint_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let mut tmp = path.to_path_buf();
    tmp.set_extension(format!("{}.{}.tmp", std::process::id(), nanos));
    tmp
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use std::{env, net::SocketAddr, path::PathBuf, sync::Arc};

    use sof::framework::{
        BranchReorgedEvent, CheckpointBarrierEvent, CheckpointBarrierReason, ClusterNodeInfo,
        ClusterTopologyEvent, ControlPlaneSource, DerivedStateConsumer, DerivedStateFeedEnvelope,
        DerivedStateFeedEvent, FeedSequence, FeedSessionId, FeedWatermarks, LeaderScheduleEntry,
        LeaderScheduleEvent, ObservedRecentBlockhashEvent, SlotStatusChangedEvent,
    };
    use solana_pubkey::Pubkey;

    use super::*;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn node(pubkey: Pubkey, tpu_port: u16) -> ClusterNodeInfo {
        ClusterNodeInfo {
            pubkey,
            wallclock: 0,
            shred_version: 0,
            gossip: None,
            tpu: Some(addr(tpu_port)),
            tpu_quic: None,
            tpu_forwards: None,
            tpu_forwards_quic: None,
            tpu_vote: None,
            tvu: None,
            rpc: None,
        }
    }

    fn envelope(event: DerivedStateFeedEvent) -> DerivedStateFeedEnvelope {
        DerivedStateFeedEnvelope {
            session_id: FeedSessionId(9),
            sequence: FeedSequence(1),
            emitted_at: UNIX_EPOCH,
            watermarks: FeedWatermarks {
                canonical_tip_slot: Some(100),
                processed_slot: Some(100),
                confirmed_slot: Some(100),
                finalized_slot: Some(99),
            },
            event,
        }
    }

    fn unique_temp_path(label: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "sof-tx-{label}-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ))
    }

    fn must<T, E: core::fmt::Debug>(result: Result<T, E>) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("unexpected error: {error:?}"),
        }
    }

    #[test]
    fn derived_state_adapter_applies_replayable_control_plane_events() {
        let mut adapter = DerivedStateTxProviderAdapter::default();
        let leader_a = Pubkey::new_unique();
        let leader_b = Pubkey::new_unique();

        must(
            adapter.apply(envelope(DerivedStateFeedEvent::ClusterTopologyChanged(
                ClusterTopologyEvent {
                    source: ControlPlaneSource::Direct,
                    slot: Some(40),
                    epoch: None,
                    active_entrypoint: None,
                    total_nodes: 2,
                    added_nodes: Vec::new(),
                    removed_pubkeys: Vec::new(),
                    updated_nodes: Vec::new(),
                    snapshot_nodes: vec![node(leader_a, 9001), node(leader_b, 9002)],
                },
            ))),
        );
        must(
            adapter.apply(envelope(DerivedStateFeedEvent::LeaderScheduleUpdated(
                LeaderScheduleEvent {
                    source: ControlPlaneSource::Direct,
                    slot: Some(40),
                    epoch: None,
                    added_leaders: Vec::new(),
                    removed_slots: Vec::new(),
                    updated_leaders: Vec::new(),
                    snapshot_leaders: vec![
                        LeaderScheduleEntry {
                            slot: 40,
                            leader: leader_a,
                        },
                        LeaderScheduleEntry {
                            slot: 41,
                            leader: leader_b,
                        },
                    ],
                },
            ))),
        );
        must(
            adapter.apply(envelope(DerivedStateFeedEvent::RecentBlockhashObserved(
                ObservedRecentBlockhashEvent {
                    slot: 40,
                    recent_blockhash: [5_u8; 32],
                    dataset_tx_count: 1,
                },
            ))),
        );
        must(
            adapter.apply(envelope(DerivedStateFeedEvent::SlotStatusChanged(
                SlotStatusChangedEvent {
                    slot: 41,
                    parent_slot: Some(40),
                    previous_status: None,
                    status: sof::framework::ForkSlotStatus::Processed,
                },
            ))),
        );

        assert_eq!(adapter.latest_blockhash(), Some([5_u8; 32]));
        assert_eq!(
            adapter.current_leader(),
            Some(LeaderTarget::new(Some(leader_b), addr(9008)))
        );
    }

    #[test]
    fn derived_state_adapter_snapshot_round_trip_restores_state() {
        let mut adapter = DerivedStateTxProviderAdapter::default();
        let leader = Pubkey::new_unique();
        must(
            adapter.apply(envelope(DerivedStateFeedEvent::ClusterTopologyChanged(
                ClusterTopologyEvent {
                    source: ControlPlaneSource::Direct,
                    slot: Some(77),
                    epoch: None,
                    active_entrypoint: None,
                    total_nodes: 1,
                    added_nodes: Vec::new(),
                    removed_pubkeys: Vec::new(),
                    updated_nodes: Vec::new(),
                    snapshot_nodes: vec![node(leader, 9101)],
                },
            ))),
        );
        must(
            adapter.apply(envelope(DerivedStateFeedEvent::LeaderScheduleUpdated(
                LeaderScheduleEvent {
                    source: ControlPlaneSource::Direct,
                    slot: Some(77),
                    epoch: None,
                    added_leaders: Vec::new(),
                    removed_slots: Vec::new(),
                    updated_leaders: Vec::new(),
                    snapshot_leaders: vec![LeaderScheduleEntry { slot: 77, leader }],
                },
            ))),
        );

        let snapshot = adapter.snapshot_state();
        let restored = DerivedStateTxProviderAdapter::default();
        restored.restore_snapshot(snapshot);

        assert_eq!(
            restored.current_leader(),
            Some(LeaderTarget::new(Some(leader), addr(9107)))
        );
    }

    #[test]
    fn derived_state_adapter_persists_and_restores_checkpointed_snapshot() {
        let checkpoint_path = unique_temp_path("derived-state-adapter-checkpoint");
        let leader = Pubkey::new_unique();
        let mut adapter = DerivedStateTxProviderAdapter::with_checkpoint_path(
            DerivedStateTxProviderAdapterConfig::default(),
            &checkpoint_path,
        );
        must(
            adapter.apply(envelope(DerivedStateFeedEvent::ClusterTopologyChanged(
                ClusterTopologyEvent {
                    source: ControlPlaneSource::Direct,
                    slot: Some(88),
                    epoch: None,
                    active_entrypoint: None,
                    total_nodes: 1,
                    added_nodes: Vec::new(),
                    removed_pubkeys: Vec::new(),
                    updated_nodes: Vec::new(),
                    snapshot_nodes: vec![node(leader, 9201)],
                },
            ))),
        );
        must(
            adapter.apply(envelope(DerivedStateFeedEvent::LeaderScheduleUpdated(
                LeaderScheduleEvent {
                    source: ControlPlaneSource::Direct,
                    slot: Some(88),
                    epoch: None,
                    added_leaders: Vec::new(),
                    removed_slots: Vec::new(),
                    updated_leaders: Vec::new(),
                    snapshot_leaders: vec![LeaderScheduleEntry { slot: 88, leader }],
                },
            ))),
        );
        must(
            adapter.apply(envelope(DerivedStateFeedEvent::RecentBlockhashObserved(
                ObservedRecentBlockhashEvent {
                    slot: 88,
                    recent_blockhash: [9_u8; 32],
                    dataset_tx_count: 3,
                },
            ))),
        );
        must(adapter.flush_checkpoint(DerivedStateCheckpoint {
            session_id: FeedSessionId(42),
            last_applied_sequence: FeedSequence(17),
            watermarks: FeedWatermarks {
                canonical_tip_slot: Some(88),
                processed_slot: Some(88),
                confirmed_slot: Some(88),
                finalized_slot: Some(87),
            },
            state_version: DERIVED_STATE_ADAPTER_STATE_VERSION,
            extension_version: DERIVED_STATE_ADAPTER_EXTENSION_VERSION.to_owned(),
        }));

        let mut restored = DerivedStateTxProviderAdapter::with_checkpoint_path(
            DerivedStateTxProviderAdapterConfig::default(),
            &checkpoint_path,
        );
        let checkpoint = must(restored.load_checkpoint());

        assert_eq!(
            checkpoint.map(|value| value.last_applied_sequence),
            Some(FeedSequence(17))
        );
        assert_eq!(restored.latest_blockhash(), Some([9_u8; 32]));
        assert_eq!(
            restored.current_leader(),
            Some(LeaderTarget::new(Some(leader), addr(9207)))
        );

        if let Err(_error) = fs::remove_file(checkpoint_path) {}
    }

    #[test]
    fn derived_state_adapter_ignores_incompatible_persisted_checkpoints() {
        let checkpoint_path = unique_temp_path("derived-state-adapter-incompatible");
        let persisted = PersistedDerivedStateTxProviderAdapter {
            checkpoint: Some(DerivedStateCheckpoint {
                session_id: FeedSessionId(77),
                last_applied_sequence: FeedSequence(3),
                watermarks: FeedWatermarks::default(),
                state_version: 999,
                extension_version: "other".to_owned(),
            }),
            snapshot: TxProviderAdapterSnapshot::default(),
        };
        must(fs::write(
            &checkpoint_path,
            must(serde_json::to_vec(&persisted)),
        ));

        let mut restored = DerivedStateTxProviderAdapter::with_checkpoint_path(
            DerivedStateTxProviderAdapterConfig::default(),
            &checkpoint_path,
        );
        assert_eq!(must(restored.load_checkpoint()), None);
        assert_eq!(restored.latest_blockhash(), None);

        if let Err(_error) = fs::remove_file(checkpoint_path) {}
    }

    #[test]
    fn derived_state_adapter_ignores_non_control_plane_events() {
        let mut adapter = DerivedStateTxProviderAdapter::default();
        must(
            adapter.apply(envelope(DerivedStateFeedEvent::CheckpointBarrier(
                CheckpointBarrierEvent {
                    barrier_sequence: FeedSequence(1),
                    reason: CheckpointBarrierReason::Periodic,
                },
            ))),
        );
        must(adapter.apply(envelope(DerivedStateFeedEvent::BranchReorged(
            BranchReorgedEvent {
                old_tip: 80,
                new_tip: 81,
                common_ancestor: Some(79),
                detached_slots: Arc::from(vec![80]),
                attached_slots: Arc::from(vec![81]),
            },
        ))));
        assert_eq!(adapter.current_leader(), None);
    }
}
