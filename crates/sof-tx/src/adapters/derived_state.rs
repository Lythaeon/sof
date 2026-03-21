//! `sof` derived-state adapter that bridges replayable control-plane state into `sof-tx` providers.

use std::path::PathBuf;

use arcshift::ArcShift;
use sof::framework::{
    DerivedStateCheckpoint, DerivedStateCheckpointStore, DerivedStateConsumer,
    DerivedStateConsumerFault, DerivedStateControlPlaneQuality, DerivedStateControlPlaneStateEvent,
    DerivedStateFeedEnvelope, DerivedStateFeedEvent, DerivedStatePersistedCheckpoint,
};

use crate::{
    adapters::common::{
        TxProviderAdapterConfig, TxProviderAdapterCore, TxProviderAdapterSnapshot,
        TxProviderControlPlaneSnapshot, TxProviderFlowSafetyPolicy, TxProviderFlowSafetyReport,
        take_next_leader_identity_targets,
    },
    providers::{LeaderProvider, LeaderTarget, RecentBlockhashProvider},
    submit::{TxFlowSafetyIssue, TxFlowSafetyQuality, TxFlowSafetySnapshot, TxFlowSafetySource},
};

/// Stable derived-state consumer name exposed to the SOF host.
const DERIVED_STATE_ADAPTER_NAME: &str = "sof-tx-derived-state-provider-adapter";
/// Extension contract version for persisted compatibility checks.
const DERIVED_STATE_ADAPTER_EXTENSION_VERSION: &str = "sof-tx-derived-state-provider-adapter-v1";
/// State-schema version for persisted compatibility checks.
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
    pub fn checkpoint_path(&self) -> &std::path::Path {
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
    checkpoint: ArcShift<Option<DerivedStateCheckpoint>>,
    /// Latest canonical observer-side control-plane state when present in the feed.
    latest_control_plane_state: ArcShift<Option<DerivedStateControlPlaneStateEvent>>,
}

impl DerivedStateTxProviderAdapter {
    /// Creates a new in-memory adapter.
    #[must_use]
    pub fn new(config: DerivedStateTxProviderAdapterConfig) -> Self {
        Self {
            core: TxProviderAdapterCore::new(config),
            persistence: None,
            checkpoint: ArcShift::new(None),
            latest_control_plane_state: ArcShift::new(None),
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
            checkpoint: ArcShift::new(None),
            latest_control_plane_state: ArcShift::new(None),
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

    /// Returns the current control-plane freshness snapshot.
    #[must_use]
    pub fn control_plane_snapshot(&self) -> TxProviderControlPlaneSnapshot {
        self.core.control_plane_snapshot()
    }

    /// Evaluates the current control-plane state against one flow-safety policy.
    #[must_use]
    pub fn evaluate_flow_safety(
        &self,
        policy: TxProviderFlowSafetyPolicy,
    ) -> TxProviderFlowSafetyReport {
        self.core.evaluate_flow_safety(policy)
    }

    /// Returns configured persistence when file-backed checkpoints are enabled.
    #[must_use]
    pub const fn persistence(&self) -> Option<&DerivedStateTxProviderAdapterPersistence> {
        self.persistence.as_ref()
    }

    /// Returns the latest in-memory checkpoint observed by the adapter.
    fn current_checkpoint(&self) -> Option<DerivedStateCheckpoint> {
        self.checkpoint.shared_get().clone()
    }

    /// Replaces the latest in-memory checkpoint visible to recovery logic.
    fn set_checkpoint(&mut self, checkpoint: Option<DerivedStateCheckpoint>) {
        self.checkpoint.update(checkpoint);
    }

    /// Loads one compatible persisted checkpoint and state snapshot, when configured.
    fn load_persisted_state(
        &self,
    ) -> Result<
        Option<DerivedStatePersistedCheckpoint<TxProviderAdapterSnapshot>>,
        DerivedStateConsumerFault,
    > {
        let Some(persistence) = self.persistence() else {
            return Ok(None);
        };
        DerivedStateCheckpointStore::new(persistence.checkpoint_path()).load_compatible(
            DERIVED_STATE_ADAPTER_STATE_VERSION,
            DERIVED_STATE_ADAPTER_EXTENSION_VERSION,
        )
    }

    /// Persists the current adapter snapshot alongside one checkpoint, when configured.
    fn persist_state(
        &self,
        checkpoint: &DerivedStateCheckpoint,
    ) -> Result<(), DerivedStateConsumerFault> {
        let Some(persistence) = self.persistence() else {
            return Ok(());
        };
        DerivedStateCheckpointStore::new(persistence.checkpoint_path())
            .store(checkpoint, &self.snapshot_state())
    }

    /// Returns the latest observer-side control-plane snapshot when the feed has emitted one.
    fn latest_control_plane_state(&self) -> Option<DerivedStateControlPlaneStateEvent> {
        *self.latest_control_plane_state.shared_get()
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

impl TxFlowSafetySource for DerivedStateTxProviderAdapter {
    fn toxic_flow_snapshot(&self) -> TxFlowSafetySnapshot {
        self.latest_control_plane_state().map_or_else(
            || {
                let report = self.evaluate_flow_safety(TxProviderFlowSafetyPolicy::default());
                TxFlowSafetySnapshot {
                    quality: match report.quality {
                        crate::adapters::TxProviderControlPlaneQuality::Stable => {
                            TxFlowSafetyQuality::Stable
                        }
                        crate::adapters::TxProviderControlPlaneQuality::Degraded => {
                            TxFlowSafetyQuality::Degraded
                        }
                        crate::adapters::TxProviderControlPlaneQuality::Stale => {
                            TxFlowSafetyQuality::Stale
                        }
                        crate::adapters::TxProviderControlPlaneQuality::IncompleteControlPlane => {
                            TxFlowSafetyQuality::IncompleteControlPlane
                        }
                    },
                    issues: if report.is_safe() {
                        Vec::new()
                    } else {
                        vec![TxFlowSafetyIssue::MissingControlPlane]
                    },
                    current_state_version: report.snapshot.tip_slot,
                    replay_recovery_pending: false,
                }
            },
            |control_plane_state| {
                let quality = match control_plane_state.quality {
                    DerivedStateControlPlaneQuality::Stable => TxFlowSafetyQuality::Stable,
                    DerivedStateControlPlaneQuality::Provisional => {
                        TxFlowSafetyQuality::Provisional
                    }
                    DerivedStateControlPlaneQuality::ReorgRisk => TxFlowSafetyQuality::ReorgRisk,
                    DerivedStateControlPlaneQuality::Stale => TxFlowSafetyQuality::Stale,
                    DerivedStateControlPlaneQuality::Degraded => TxFlowSafetyQuality::Degraded,
                    DerivedStateControlPlaneQuality::IncompleteControlPlane => {
                        TxFlowSafetyQuality::IncompleteControlPlane
                    }
                };
                let mut issues = Vec::new();
                if !control_plane_state.strategy_safe {
                    match quality {
                        TxFlowSafetyQuality::Stable => {}
                        TxFlowSafetyQuality::Provisional => {
                            issues.push(TxFlowSafetyIssue::Provisional)
                        }
                        TxFlowSafetyQuality::ReorgRisk => issues.push(TxFlowSafetyIssue::ReorgRisk),
                        TxFlowSafetyQuality::Stale => {
                            issues.push(TxFlowSafetyIssue::StaleControlPlane)
                        }
                        TxFlowSafetyQuality::Degraded => {
                            issues.push(TxFlowSafetyIssue::DegradedControlPlane)
                        }
                        TxFlowSafetyQuality::IncompleteControlPlane => {
                            issues.push(TxFlowSafetyIssue::MissingControlPlane)
                        }
                    }
                }
                TxFlowSafetySnapshot {
                    quality,
                    issues,
                    current_state_version: control_plane_state.tip_slot,
                    replay_recovery_pending: false,
                }
            },
        )
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
        let (checkpoint, snapshot) = persisted.into_parts();
        self.restore_snapshot(snapshot);
        self.set_checkpoint(Some(checkpoint.clone()));
        Ok(Some(checkpoint))
    }

    fn config(&self) -> sof::framework::DerivedStateConsumerConfig {
        sof::framework::DerivedStateConsumerConfig::new().with_control_plane_observed()
    }

    fn apply(
        &mut self,
        envelope: &DerivedStateFeedEnvelope,
    ) -> Result<(), DerivedStateConsumerFault> {
        match &envelope.event {
            DerivedStateFeedEvent::RecentBlockhashObserved(event) => {
                self.core.apply_recent_blockhash(event);
            }
            DerivedStateFeedEvent::ClusterTopologyChanged(event) => {
                self.core.apply_cluster_topology(event);
            }
            DerivedStateFeedEvent::LeaderScheduleUpdated(event) => {
                self.core.apply_leader_schedule(event);
            }
            DerivedStateFeedEvent::SlotStatusChanged(event) => {
                self.core.apply_slot_status(*event);
            }
            DerivedStateFeedEvent::BranchReorged(event) => {
                self.core.apply_reorg(event);
            }
            DerivedStateFeedEvent::ControlPlaneStateUpdated(event) => {
                self.latest_control_plane_state.update(Some(*event));
            }
            DerivedStateFeedEvent::StateInvalidated(_)
            | DerivedStateFeedEvent::TxOutcomeObserved(_)
            | DerivedStateFeedEvent::TransactionApplied(_)
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

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use std::{
        env, fs,
        net::SocketAddr,
        path::PathBuf,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

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
    fn flow_safety_reports_missing_inputs_before_feed_replay() {
        let adapter = DerivedStateTxProviderAdapter::default();
        let report = adapter.evaluate_flow_safety(TxProviderFlowSafetyPolicy::default());

        assert!(!report.is_safe());
        assert!(
            report
                .issues
                .contains(&crate::adapters::TxProviderFlowSafetyIssue::MissingRecentBlockhash)
        );
        assert!(
            report
                .issues
                .contains(&crate::adapters::TxProviderFlowSafetyIssue::MissingClusterTopology)
        );
        assert!(
            report
                .issues
                .contains(&crate::adapters::TxProviderFlowSafetyIssue::MissingLeaderSchedule)
        );
    }

    #[test]
    fn derived_state_adapter_applies_replayable_control_plane_events() {
        let mut adapter = DerivedStateTxProviderAdapter::default();
        let leader_a = Pubkey::new_unique();
        let leader_b = Pubkey::new_unique();

        must(
            adapter.apply(&envelope(DerivedStateFeedEvent::ClusterTopologyChanged(
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
            adapter.apply(&envelope(DerivedStateFeedEvent::LeaderScheduleUpdated(
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
            adapter.apply(&envelope(DerivedStateFeedEvent::RecentBlockhashObserved(
                ObservedRecentBlockhashEvent {
                    slot: 40,
                    recent_blockhash: [5_u8; 32],
                    dataset_tx_count: 1,
                },
            ))),
        );
        must(
            adapter.apply(&envelope(DerivedStateFeedEvent::SlotStatusChanged(
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
            adapter.apply(&envelope(DerivedStateFeedEvent::ClusterTopologyChanged(
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
            adapter.apply(&envelope(DerivedStateFeedEvent::LeaderScheduleUpdated(
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
            adapter.apply(&envelope(DerivedStateFeedEvent::ClusterTopologyChanged(
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
            adapter.apply(&envelope(DerivedStateFeedEvent::LeaderScheduleUpdated(
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
            adapter.apply(&envelope(DerivedStateFeedEvent::RecentBlockhashObserved(
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
        let persisted = DerivedStatePersistedCheckpoint::new(
            DerivedStateCheckpoint {
                session_id: FeedSessionId(77),
                last_applied_sequence: FeedSequence(3),
                watermarks: FeedWatermarks::default(),
                state_version: 999,
                extension_version: "other".to_owned(),
            },
            TxProviderAdapterSnapshot::default(),
        );
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
            adapter.apply(&envelope(DerivedStateFeedEvent::CheckpointBarrier(
                CheckpointBarrierEvent {
                    barrier_sequence: FeedSequence(1),
                    reason: CheckpointBarrierReason::Periodic,
                },
            ))),
        );
        must(
            adapter.apply(&envelope(DerivedStateFeedEvent::BranchReorged(
                BranchReorgedEvent {
                    old_tip: 80,
                    new_tip: 81,
                    common_ancestor: Some(79),
                    detached_slots: Arc::from(vec![80]),
                    attached_slots: Arc::from(vec![81]),
                },
            ))),
        );
        assert_eq!(adapter.current_leader(), None);
    }
}
