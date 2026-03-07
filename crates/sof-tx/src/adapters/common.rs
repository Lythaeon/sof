//! Shared state reduction for tx-provider adapters.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use sof::framework::{
    BranchReorgedEvent, ClusterNodeInfo, ClusterTopologyEvent, LeaderScheduleEntry,
    LeaderScheduleEvent, ObservedRecentBlockhashEvent, PluginHost, SlotStatusChangedEvent,
};
use solana_pubkey::Pubkey;

use crate::providers::{LeaderTarget, RecentBlockhashProvider};

/// Agave's TPU QUIC port is derived by adding this offset to the TPU UDP port.
pub(crate) const AGAVE_QUIC_PORT_OFFSET: u16 = 6;

/// Shared configuration for tx-provider adapters.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TxProviderAdapterConfig {
    /// Maximum number of slot-to-leader assignments retained in memory.
    pub max_leader_slots: usize,
    /// Maximum number of next leaders returned from the provider window.
    pub max_next_leaders: usize,
}

impl TxProviderAdapterConfig {
    /// Returns this config with bounded minimums.
    #[must_use]
    pub const fn normalized(self) -> Self {
        Self {
            max_leader_slots: if self.max_leader_slots == 0 {
                1
            } else {
                self.max_leader_slots
            },
            max_next_leaders: self.max_next_leaders,
        }
    }
}

impl Default for TxProviderAdapterConfig {
    fn default() -> Self {
        Self {
            max_leader_slots: 2_048,
            max_next_leaders: 128,
        }
    }
}

/// Snapshot of tx-provider control-plane freshness inputs.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub struct TxProviderControlPlaneSnapshot {
    /// Slot of the most recently observed recent blockhash.
    pub latest_recent_blockhash_slot: Option<u64>,
    /// Slot of the most recently applied cluster topology update.
    pub cluster_topology_slot: Option<u64>,
    /// Slot of the most recently applied leader schedule update.
    pub leader_schedule_slot: Option<u64>,
    /// Current canonical tip slot known to the adapter.
    pub tip_slot: Option<u64>,
    /// Number of retained ingress identities.
    pub known_ingress_nodes: usize,
    /// Number of retained leader-slot assignments.
    pub known_leader_slots: usize,
    /// Number of currently routable leader target addresses.
    pub known_leader_target_addrs: usize,
    /// Number of currently routable leader identities.
    pub known_leader_target_identities: usize,
}

/// Freshness classification for one control-plane input.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum TxProviderFreshnessState {
    /// No observation has been recorded yet.
    Missing,
    /// Freshness cannot be evaluated yet.
    Unknown,
    /// Observation is present and within the configured freshness budget.
    Fresh,
    /// Observation exceeded the configured freshness budget.
    Stale,
}

/// Freshness metadata for one control-plane input.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub struct TxProviderInputFreshness {
    /// Slot of the latest observation for this input.
    pub observed_slot: Option<u64>,
    /// Canonical tip slot used to evaluate freshness.
    pub tip_slot: Option<u64>,
    /// Evaluated lag between `tip_slot` and `observed_slot`.
    pub slot_lag: Option<u64>,
    /// Maximum slot lag allowed by the active policy.
    pub max_allowed_slot_lag: Option<u64>,
    /// Freshness classification for this input.
    pub state: TxProviderFreshnessState,
}

/// Coarse health classification for one control-plane snapshot.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum TxProviderControlPlaneQuality {
    /// Required inputs are present and within policy.
    Stable,
    /// Inputs exist, but they are not coherent enough to trust for routing.
    Degraded,
    /// Required inputs exist but at least one is stale.
    Stale,
    /// Required inputs are missing or routing targets cannot be formed yet.
    IncompleteControlPlane,
}

/// Slot-lag policy used to classify tx-provider control-plane state.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TxProviderFlowSafetyPolicy {
    /// Require a recent blockhash observation before treating the adapter as safe.
    pub require_recent_blockhash: bool,
    /// Require a cluster topology observation before treating the adapter as safe.
    pub require_cluster_topology: bool,
    /// Require a leader schedule observation before treating the adapter as safe.
    pub require_leader_schedule: bool,
    /// Require a known tip slot before evaluating lag-based freshness.
    pub require_tip_slot: bool,
    /// Require at least one routable leader target before treating the adapter as safe.
    pub require_routable_leader_targets: bool,
    /// Maximum allowed lag between current tip and recent blockhash slot.
    pub max_recent_blockhash_slot_lag: Option<u64>,
    /// Maximum allowed lag between current tip and topology slot.
    pub max_cluster_topology_slot_lag: Option<u64>,
    /// Maximum allowed lag between current tip and leader schedule slot.
    pub max_leader_schedule_slot_lag: Option<u64>,
    /// Maximum allowed spread between observed control-plane slots.
    pub max_control_plane_slot_spread: Option<u64>,
}

impl Default for TxProviderFlowSafetyPolicy {
    fn default() -> Self {
        Self {
            require_recent_blockhash: true,
            require_cluster_topology: true,
            require_leader_schedule: true,
            require_tip_slot: true,
            require_routable_leader_targets: true,
            max_recent_blockhash_slot_lag: Some(32),
            max_cluster_topology_slot_lag: Some(64),
            max_leader_schedule_slot_lag: Some(128),
            max_control_plane_slot_spread: Some(32),
        }
    }
}

/// One reason a tx-provider adapter is not currently safe to drive submit decisions.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TxProviderFlowSafetyIssue {
    /// No recent blockhash has been observed yet.
    MissingRecentBlockhash,
    /// No topology event has been observed yet.
    MissingClusterTopology,
    /// No leader schedule event has been observed yet.
    MissingLeaderSchedule,
    /// No canonical tip slot is available, so lag cannot be evaluated safely.
    MissingTipSlot,
    /// No routable leader targets are currently available.
    MissingLeaderRoutingTargets,
    /// Recent blockhash data is older than the configured lag policy.
    StaleRecentBlockhash {
        /// Observed lag between the tip slot and the most recent blockhash slot.
        slot_lag: u64,
        /// Maximum allowed lag from the policy.
        max_allowed: u64,
    },
    /// Cluster topology data is older than the configured lag policy.
    StaleClusterTopology {
        /// Observed lag between the tip slot and the most recent topology slot.
        slot_lag: u64,
        /// Maximum allowed lag from the policy.
        max_allowed: u64,
    },
    /// Leader schedule data is older than the configured lag policy.
    StaleLeaderSchedule {
        /// Observed lag between the tip slot and the most recent leader schedule slot.
        slot_lag: u64,
        /// Maximum allowed lag from the policy.
        max_allowed: u64,
    },
    /// Control-plane sources are too far apart in slot terms to treat as one coherent view.
    IncoherentControlPlane {
        /// Spread between the oldest and newest observed control-plane slots.
        slot_spread: u64,
        /// Maximum allowed spread from the policy.
        max_allowed: u64,
    },
}

/// Evaluation result for one tx-provider control-plane snapshot.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TxProviderFlowSafetyReport {
    /// Snapshot used for the evaluation.
    pub snapshot: TxProviderControlPlaneSnapshot,
    /// Freshness metadata for the recent blockhash input.
    pub recent_blockhash_freshness: TxProviderInputFreshness,
    /// Freshness metadata for the cluster topology input.
    pub cluster_topology_freshness: TxProviderInputFreshness,
    /// Freshness metadata for the leader schedule input.
    pub leader_schedule_freshness: TxProviderInputFreshness,
    /// Observed spread across available control-plane slots.
    pub control_plane_slot_spread: Option<u64>,
    /// True when control-plane inputs are aligned under the active policy.
    pub inputs_aligned: bool,
    /// Coarse quality classification for the snapshot.
    pub quality: TxProviderControlPlaneQuality,
    /// All detected issues under the requested policy.
    pub issues: Vec<TxProviderFlowSafetyIssue>,
}

impl TxProviderFlowSafetyReport {
    /// Returns true when the control-plane state satisfies the policy.
    #[must_use]
    pub const fn is_safe(&self) -> bool {
        matches!(self.quality, TxProviderControlPlaneQuality::Stable)
    }
}

/// Serializable snapshot of tx-provider adapter state.
#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TxProviderAdapterSnapshot {
    /// Most recent observed recent blockhash.
    pub latest_recent_blockhash: Option<[u8; 32]>,
    /// Slot of the most recent recent-blockhash observation.
    #[serde(default)]
    pub latest_recent_blockhash_slot: Option<u64>,
    /// Slot-to-leader assignments.
    pub leader_by_slot: Vec<(u64, [u8; 32])>,
    /// Latest observed canonical tip slot.
    pub tip_slot: Option<u64>,
    /// Most recent leader schedule cursor.
    pub leader_slot_cursor: Option<u64>,
    /// Slot of the most recent topology update.
    #[serde(default)]
    pub cluster_topology_slot: Option<u64>,
    /// Slot of the most recent leader schedule update.
    #[serde(default)]
    pub leader_schedule_slot: Option<u64>,
    /// Cached ingress endpoints keyed by validator identity.
    pub ingress_by_identity: Vec<TxProviderIngressSnapshot>,
}

/// Serializable snapshot of one validator ingress mapping.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct TxProviderIngressSnapshot {
    /// Validator identity.
    pub identity: [u8; 32],
    /// TPU UDP endpoint.
    pub tpu: Option<SocketAddr>,
    /// TPU QUIC endpoint.
    pub tpu_quic: Option<SocketAddr>,
    /// TPU forwards UDP endpoint.
    pub tpu_forwards: Option<SocketAddr>,
    /// TPU forwards QUIC endpoint.
    pub tpu_forwards_quic: Option<SocketAddr>,
}

/// Shared mutable core used by tx-provider adapters.
#[derive(Debug, Clone)]
pub(crate) struct TxProviderAdapterCore {
    /// Shared adapter state updated from plugin or derived-state events.
    state: Arc<Mutex<AdapterState>>,
    /// Normalized adapter limits used for leader-window and snapshot retention.
    config: TxProviderAdapterConfig,
}

impl TxProviderAdapterCore {
    /// Creates a new shared adapter core.
    #[must_use]
    pub(crate) fn new(config: TxProviderAdapterConfig) -> Self {
        Self {
            state: Arc::new(Mutex::new(AdapterState::default())),
            config: config.normalized(),
        }
    }

    /// Seeds adapter state from already-observed values in `PluginHost`.
    pub(crate) fn prime_from_plugin_host(&self, host: &mut PluginHost) {
        let blockhash_opt = host.latest_observed_recent_blockhash();
        let leader_opt = host.latest_observed_tpu_leader();
        let max_leader_slots = self.config.max_leader_slots;

        self.update(move |state| {
            if let Some((_slot, recent_blockhash)) = blockhash_opt {
                state.latest_recent_blockhash = Some(recent_blockhash);
            }
            if let Some(entry) = leader_opt {
                state.upsert_leader(entry);
                state.advance_cursor(entry.slot);
                cap_leader_slots(state, max_leader_slots);
            }
        });
    }

    /// Inserts or updates one TPU address mapping for a leader identity.
    pub(crate) fn set_leader_tpu_addr(&self, identity: Pubkey, tpu_addr: SocketAddr) {
        self.update(move |state| {
            let ingress = state.ingress_by_identity.entry(identity).or_default();
            ingress.tpu = Some(tpu_addr);
            ingress.tpu_quic = with_agave_quic_fallback(Some(tpu_addr), ingress.tpu_quic);
        });
    }

    /// Removes one TPU address mapping for a leader identity.
    pub(crate) fn remove_leader_tpu_addr(&self, identity: Pubkey) {
        self.update(move |state| {
            let _ = state.ingress_by_identity.remove(&identity);
        });
    }

    /// Applies one recent blockhash observation.
    pub(crate) fn apply_recent_blockhash(&self, event: &ObservedRecentBlockhashEvent) {
        self.update({
            let recent_blockhash = event.recent_blockhash;
            let slot = event.slot;
            move |state| {
                state.latest_recent_blockhash = Some(recent_blockhash);
                state.latest_recent_blockhash_slot = Some(slot);
            }
        });
    }

    /// Applies one cluster topology update.
    pub(crate) fn apply_cluster_topology(&self, event: &ClusterTopologyEvent) {
        self.update({
            let event = event.clone();
            move |state| {
                if let Some(slot) = event.slot {
                    state.cluster_topology_slot = Some(slot);
                }
                apply_cluster_topology(state, &event);
            }
        });
    }

    /// Applies one leader schedule update.
    pub(crate) fn apply_leader_schedule(&self, event: &LeaderScheduleEvent) {
        let max_leader_slots = self.config.max_leader_slots;
        self.update({
            let event = event.clone();
            move |state| {
                if let Some(slot) = event.slot {
                    state.leader_schedule_slot = Some(slot);
                }
                apply_leader_schedule(state, &event, max_leader_slots);
            }
        });
    }

    /// Applies one slot-status transition.
    pub(crate) fn apply_slot_status(&self, event: SlotStatusChangedEvent) {
        self.update(move |state| {
            state.tip_slot = Some(event.slot);
        });
    }

    /// Applies one branch reorg event.
    pub(crate) fn apply_reorg(&self, event: &BranchReorgedEvent) {
        self.update({
            let event = event.clone();
            move |state| {
                state.tip_slot = Some(event.new_tip);
            }
        });
    }

    /// Restores adapter state from a snapshot.
    pub(crate) fn restore_snapshot(&self, snapshot: TxProviderAdapterSnapshot) {
        self.update(move |state| {
            state.latest_recent_blockhash = snapshot.latest_recent_blockhash;
            state.latest_recent_blockhash_slot = snapshot.latest_recent_blockhash_slot;
            state.leader_by_slot = snapshot
                .leader_by_slot
                .into_iter()
                .map(|(slot, identity)| (slot, Pubkey::new_from_array(identity)))
                .collect();
            state.tip_slot = snapshot.tip_slot;
            state.leader_slot_cursor = snapshot.leader_slot_cursor;
            state.cluster_topology_slot = snapshot.cluster_topology_slot;
            state.leader_schedule_slot = snapshot.leader_schedule_slot;
            state.ingress_by_identity = snapshot
                .ingress_by_identity
                .into_iter()
                .map(|entry| {
                    (
                        Pubkey::new_from_array(entry.identity),
                        NodeIngress {
                            tpu: entry.tpu,
                            tpu_quic: entry.tpu_quic,
                            tpu_forwards: entry.tpu_forwards,
                            tpu_forwards_quic: entry.tpu_forwards_quic,
                        },
                    )
                })
                .collect();
            cap_leader_slots(state, self.config.max_leader_slots);
        });
    }

    /// Captures a snapshot of the current adapter state.
    #[must_use]
    pub(crate) fn snapshot_state(&self) -> TxProviderAdapterSnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        TxProviderAdapterSnapshot {
            latest_recent_blockhash: state.latest_recent_blockhash,
            latest_recent_blockhash_slot: state.latest_recent_blockhash_slot,
            leader_by_slot: state
                .leader_by_slot
                .iter()
                .map(|(slot, identity)| (*slot, identity.to_bytes()))
                .collect(),
            tip_slot: state.tip_slot,
            leader_slot_cursor: state.leader_slot_cursor,
            cluster_topology_slot: state.cluster_topology_slot,
            leader_schedule_slot: state.leader_schedule_slot,
            ingress_by_identity: state
                .ingress_by_identity
                .iter()
                .map(|(identity, ingress)| TxProviderIngressSnapshot {
                    identity: identity.to_bytes(),
                    tpu: ingress.tpu,
                    tpu_quic: ingress.tpu_quic,
                    tpu_forwards: ingress.tpu_forwards,
                    tpu_forwards_quic: ingress.tpu_forwards_quic,
                })
                .collect(),
        }
    }

    /// Returns the latest observed recent blockhash.
    #[must_use]
    pub(crate) fn latest_blockhash(&self) -> Option<[u8; 32]> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .latest_recent_blockhash
    }

    /// Captures control-plane freshness state for downstream submit policy checks.
    #[must_use]
    pub(crate) fn control_plane_snapshot(&self) -> TxProviderControlPlaneSnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let routable_targets = collect_leader_targets_from_state(
            &state,
            self.config.max_next_leaders.saturating_add(1),
        );
        let known_leader_target_identities = routable_targets
            .iter()
            .filter_map(|target| target.identity)
            .collect::<HashSet<_>>()
            .len();
        TxProviderControlPlaneSnapshot {
            latest_recent_blockhash_slot: state.latest_recent_blockhash_slot,
            cluster_topology_slot: state.cluster_topology_slot,
            leader_schedule_slot: state.leader_schedule_slot,
            tip_slot: state.tip_slot,
            known_ingress_nodes: state.ingress_by_identity.len(),
            known_leader_slots: state.leader_by_slot.len(),
            known_leader_target_addrs: routable_targets.len(),
            known_leader_target_identities,
        }
    }

    /// Evaluates the current control-plane snapshot against one typed safety policy.
    #[must_use]
    pub(crate) fn evaluate_flow_safety(
        &self,
        policy: TxProviderFlowSafetyPolicy,
    ) -> TxProviderFlowSafetyReport {
        evaluate_flow_safety(self.control_plane_snapshot(), policy)
    }

    /// Returns current leader plus up to `next_leaders` future leaders, in slot order.
    #[must_use]
    pub(crate) fn leader_window(&self, next_leaders: usize) -> Vec<LeaderTarget> {
        let capped_next = next_leaders.min(self.config.max_next_leaders);
        let requested_identities = capped_next.saturating_add(1);
        if requested_identities == 0 {
            return Vec::new();
        }
        let requested_targets = requested_identities.saturating_mul(4);
        collect_leader_targets_from_state(
            &self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            requested_targets,
        )
    }

    /// Applies one serialized state mutation.
    fn update<F>(&self, apply: F)
    where
        F: FnOnce(&mut AdapterState),
    {
        let mut next = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        apply(&mut next);
    }
}

impl RecentBlockhashProvider for TxProviderAdapterCore {
    fn latest_blockhash(&self) -> Option<[u8; 32]> {
        self.latest_blockhash()
    }
}

/// Returns targets for the next distinct leader identities after the current leader.
#[must_use]
pub(crate) fn take_next_leader_identity_targets(
    window: Vec<LeaderTarget>,
    requested_identities: usize,
) -> Vec<LeaderTarget> {
    if requested_identities == 0 || window.is_empty() {
        return Vec::new();
    }

    let current_identity = window.first().and_then(|target| target.identity);
    let mut seen_identities = HashSet::new();
    let mut out = Vec::new();

    for target in window {
        let Some(identity) = target.identity else {
            continue;
        };
        if current_identity.is_some() && Some(identity) == current_identity {
            continue;
        }

        let is_new_identity = seen_identities.insert(identity);
        if is_new_identity && seen_identities.len() > requested_identities {
            break;
        }
        if seen_identities.len() <= requested_identities {
            out.push(target);
        }
    }

    out
}

/// Mutable adapter state populated from SOF hooks.
#[derive(Debug, Default, Clone)]
struct AdapterState {
    /// Most recent blockhash observed from SOF.
    latest_recent_blockhash: Option<[u8; 32]>,
    /// Slot that produced `latest_recent_blockhash`.
    latest_recent_blockhash_slot: Option<u64>,
    /// Slot-indexed leader identities used for leader-window lookups.
    leader_by_slot: BTreeMap<u64, Pubkey>,
    /// Latest processed tip slot seen by the adapter.
    tip_slot: Option<u64>,
    /// Cursor used to keep leader-window iteration moving forward with the tip.
    leader_slot_cursor: Option<u64>,
    /// Slot of the latest topology update.
    cluster_topology_slot: Option<u64>,
    /// Slot of the latest leader-schedule update.
    leader_schedule_slot: Option<u64>,
    /// Known TPU ingress endpoints keyed by validator identity.
    ingress_by_identity: HashMap<Pubkey, NodeIngress>,
}

impl AdapterState {
    /// Inserts or updates the leader assigned to one slot.
    fn upsert_leader(&mut self, entry: LeaderScheduleEntry) {
        let _ = self.leader_by_slot.insert(entry.slot, entry.leader);
    }

    /// Advances the leader cursor unless the new slot is older than the current cursor.
    const fn advance_cursor(&mut self, slot: u64) {
        match self.leader_slot_cursor {
            Some(current) if slot < current => {}
            Some(_) | None => {
                self.leader_slot_cursor = Some(slot);
            }
        }
    }
}

/// TPU ingress endpoints known for one validator identity.
#[derive(Debug, Clone, Copy, Default)]
struct NodeIngress {
    /// TPU UDP endpoint.
    tpu: Option<SocketAddr>,
    /// TPU QUIC endpoint.
    tpu_quic: Option<SocketAddr>,
    /// TPU forwards UDP endpoint.
    tpu_forwards: Option<SocketAddr>,
    /// TPU forwards QUIC endpoint.
    tpu_forwards_quic: Option<SocketAddr>,
}

impl NodeIngress {
    /// Returns `true` when no usable ingress endpoints remain.
    const fn is_empty(self) -> bool {
        self.tpu.is_none()
            && self.tpu_quic.is_none()
            && self.tpu_forwards.is_none()
            && self.tpu_forwards_quic.is_none()
    }
}

/// Applies one topology delta to the ingress map.
fn apply_cluster_topology(state: &mut AdapterState, event: &ClusterTopologyEvent) {
    if !event.snapshot_nodes.is_empty() {
        state.ingress_by_identity.clear();
        insert_node_ingresses(&event.snapshot_nodes, &mut state.ingress_by_identity);
    }
    insert_node_ingresses(&event.added_nodes, &mut state.ingress_by_identity);
    insert_node_ingresses(&event.updated_nodes, &mut state.ingress_by_identity);
    for pubkey in &event.removed_pubkeys {
        let _ = state.ingress_by_identity.remove(pubkey);
    }
}

/// Inserts or replaces ingress endpoints from one topology batch.
fn insert_node_ingresses(
    nodes: &[ClusterNodeInfo],
    ingress_by_identity: &mut HashMap<Pubkey, NodeIngress>,
) {
    for node in nodes {
        let ingress = NodeIngress {
            tpu: node.tpu,
            tpu_quic: with_agave_quic_fallback(node.tpu, node.tpu_quic),
            tpu_forwards: node.tpu_forwards,
            tpu_forwards_quic: with_agave_quic_fallback(node.tpu_forwards, node.tpu_forwards_quic),
        };
        if ingress.is_empty() {
            let _ = ingress_by_identity.remove(&node.pubkey);
        } else {
            let _ = ingress_by_identity.insert(node.pubkey, ingress);
        }
    }
}

/// Applies one leader-schedule delta and trims retained slots.
fn apply_leader_schedule(
    state: &mut AdapterState,
    event: &LeaderScheduleEvent,
    max_leader_slots: usize,
) {
    if !event.snapshot_leaders.is_empty() {
        state.leader_by_slot.clear();
        for entry in &event.snapshot_leaders {
            state.upsert_leader(*entry);
        }
    }

    for slot in &event.removed_slots {
        let _ = state.leader_by_slot.remove(slot);
    }
    for entry in &event.added_leaders {
        state.upsert_leader(*entry);
    }
    for entry in &event.updated_leaders {
        state.upsert_leader(*entry);
    }

    let slot_for_cursor = event.slot.or_else(|| {
        event
            .snapshot_leaders
            .iter()
            .chain(event.added_leaders.iter())
            .chain(event.updated_leaders.iter())
            .map(|entry| entry.slot)
            .max()
    });
    if let Some(slot) = slot_for_cursor {
        state.advance_cursor(slot);
    }

    cap_leader_slots(state, max_leader_slots.max(1));
}

/// Trims the retained leader-slot map down to `max_leader_slots`.
fn cap_leader_slots(state: &mut AdapterState, max_leader_slots: usize) {
    while state.leader_by_slot.len() > max_leader_slots {
        let oldest_slot = state
            .leader_by_slot
            .first_key_value()
            .map(|(slot, _)| *slot);
        let Some(oldest_slot) = oldest_slot else {
            break;
        };
        let _ = state.leader_by_slot.remove(&oldest_slot);
    }
}

/// Builds a routable leader-target window from current state.
fn collect_leader_targets_from_state(state: &AdapterState, requested: usize) -> Vec<LeaderTarget> {
    let mut output = Vec::new();
    let mut seen_addrs = HashSet::new();
    if requested == 0 {
        return output;
    }

    let start_slot = state
        .tip_slot
        .or(state.leader_slot_cursor)
        .or_else(|| {
            state
                .leader_by_slot
                .first_key_value()
                .map(|(slot, _)| *slot)
        })
        .unwrap_or(0);

    for (_slot, identity) in state.leader_by_slot.range(start_slot..) {
        let Some(ingress) = state.ingress_by_identity.get(identity).copied() else {
            continue;
        };
        append_ingress_targets(&mut output, &mut seen_addrs, *identity, ingress, requested);
        if output.len() >= requested {
            break;
        }
    }

    if output.len() < requested && start_slot > 0 {
        for (_slot, identity) in state.leader_by_slot.range(..start_slot).rev() {
            let Some(ingress) = state.ingress_by_identity.get(identity).copied() else {
                continue;
            };
            append_ingress_targets(&mut output, &mut seen_addrs, *identity, ingress, requested);
            if output.len() >= requested {
                break;
            }
        }
    }

    if output.len() < requested && !state.ingress_by_identity.is_empty() {
        let mut topology_targets = state
            .ingress_by_identity
            .iter()
            .map(|(identity, ingress)| (*identity, *ingress))
            .collect::<Vec<_>>();
        topology_targets.sort_unstable_by_key(|(identity, _)| identity.to_bytes());
        for (identity, ingress) in topology_targets {
            append_ingress_targets(&mut output, &mut seen_addrs, identity, ingress, requested);
            if output.len() >= requested {
                break;
            }
        }
    }

    output
}

/// Appends ingress candidates in transport-priority order while avoiding duplicates.
fn append_ingress_targets(
    output: &mut Vec<LeaderTarget>,
    seen_addrs: &mut HashSet<SocketAddr>,
    identity: Pubkey,
    ingress: NodeIngress,
    requested: usize,
) {
    for candidate in [
        ingress.tpu_quic,
        ingress.tpu_forwards_quic,
        ingress.tpu,
        ingress.tpu_forwards,
    ]
    .into_iter()
    .flatten()
    {
        if output.len() >= requested {
            break;
        }
        if !seen_addrs.insert(candidate) {
            continue;
        }
        output.push(LeaderTarget::new(Some(identity), candidate));
    }
}

/// Evaluates one control-plane snapshot against the current send-safety policy.
fn evaluate_flow_safety(
    snapshot: TxProviderControlPlaneSnapshot,
    policy: TxProviderFlowSafetyPolicy,
) -> TxProviderFlowSafetyReport {
    let recent_blockhash_freshness = classify_input_freshness(
        snapshot.latest_recent_blockhash_slot,
        snapshot.tip_slot,
        policy.max_recent_blockhash_slot_lag,
    );
    let cluster_topology_freshness = classify_input_freshness(
        snapshot.cluster_topology_slot,
        snapshot.tip_slot,
        policy.max_cluster_topology_slot_lag,
    );
    let leader_schedule_freshness = classify_input_freshness(
        snapshot.leader_schedule_slot,
        snapshot.tip_slot,
        policy.max_leader_schedule_slot_lag,
    );
    let control_plane_slot_spread = observed_control_plane_slot_spread(snapshot);
    let mut issues = Vec::new();

    if policy.require_recent_blockhash && snapshot.latest_recent_blockhash_slot.is_none() {
        issues.push(TxProviderFlowSafetyIssue::MissingRecentBlockhash);
    }
    if policy.require_cluster_topology && snapshot.cluster_topology_slot.is_none() {
        issues.push(TxProviderFlowSafetyIssue::MissingClusterTopology);
    }
    if policy.require_leader_schedule && snapshot.leader_schedule_slot.is_none() {
        issues.push(TxProviderFlowSafetyIssue::MissingLeaderSchedule);
    }
    if policy.require_tip_slot && snapshot.tip_slot.is_none() {
        issues.push(TxProviderFlowSafetyIssue::MissingTipSlot);
    }
    if policy.require_routable_leader_targets && snapshot.known_leader_target_addrs == 0 {
        issues.push(TxProviderFlowSafetyIssue::MissingLeaderRoutingTargets);
    }

    if let Some(tip_slot) = snapshot.tip_slot {
        apply_slot_lag_issue(
            &mut issues,
            snapshot.latest_recent_blockhash_slot,
            policy.max_recent_blockhash_slot_lag,
            |slot_lag, max_allowed| TxProviderFlowSafetyIssue::StaleRecentBlockhash {
                slot_lag,
                max_allowed,
            },
            tip_slot,
        );
        apply_slot_lag_issue(
            &mut issues,
            snapshot.cluster_topology_slot,
            policy.max_cluster_topology_slot_lag,
            |slot_lag, max_allowed| TxProviderFlowSafetyIssue::StaleClusterTopology {
                slot_lag,
                max_allowed,
            },
            tip_slot,
        );
        apply_slot_lag_issue(
            &mut issues,
            snapshot.leader_schedule_slot,
            policy.max_leader_schedule_slot_lag,
            |slot_lag, max_allowed| TxProviderFlowSafetyIssue::StaleLeaderSchedule {
                slot_lag,
                max_allowed,
            },
            tip_slot,
        );
    }

    let inputs_aligned = match (
        control_plane_slot_spread,
        policy.max_control_plane_slot_spread,
    ) {
        (Some(slot_spread), Some(max_allowed)) => {
            if slot_spread > max_allowed {
                issues.push(TxProviderFlowSafetyIssue::IncoherentControlPlane {
                    slot_spread,
                    max_allowed,
                });
                false
            } else {
                true
            }
        }
        _ => true,
    };

    let quality = classify_flow_quality(&issues);

    TxProviderFlowSafetyReport {
        snapshot,
        recent_blockhash_freshness,
        cluster_topology_freshness,
        leader_schedule_freshness,
        control_plane_slot_spread,
        inputs_aligned,
        quality,
        issues,
    }
}

/// Classifies one observed input against tip-slot freshness requirements.
fn classify_input_freshness(
    observed_slot: Option<u64>,
    tip_slot: Option<u64>,
    max_allowed_slot_lag: Option<u64>,
) -> TxProviderInputFreshness {
    let slot_lag = observed_slot
        .zip(tip_slot)
        .map(|(observed, tip)| tip.saturating_sub(observed));
    let state = match (observed_slot, tip_slot, max_allowed_slot_lag, slot_lag) {
        (None, _, _, _) => TxProviderFreshnessState::Missing,
        (Some(_), None, _, _) => TxProviderFreshnessState::Unknown,
        (Some(_), Some(_), Some(max_allowed), Some(slot_lag)) if slot_lag > max_allowed => {
            TxProviderFreshnessState::Stale
        }
        (Some(_), Some(_), _, _) => TxProviderFreshnessState::Fresh,
    };

    TxProviderInputFreshness {
        observed_slot,
        tip_slot,
        slot_lag,
        max_allowed_slot_lag,
        state,
    }
}

/// Returns the slot spread across currently observed control-plane inputs.
fn observed_control_plane_slot_spread(snapshot: TxProviderControlPlaneSnapshot) -> Option<u64> {
    let mut slots = [
        snapshot.latest_recent_blockhash_slot,
        snapshot.cluster_topology_slot,
        snapshot.leader_schedule_slot,
    ]
    .into_iter()
    .flatten();
    let first = slots.next()?;
    let (min_slot, max_slot) = slots.fold((first, first), |(min_slot, max_slot), slot| {
        (min_slot.min(slot), max_slot.max(slot))
    });
    Some(max_slot.saturating_sub(min_slot))
}

/// Reduces concrete issues into one coarse control-plane quality class.
fn classify_flow_quality(issues: &[TxProviderFlowSafetyIssue]) -> TxProviderControlPlaneQuality {
    if issues.iter().any(|issue| {
        matches!(
            issue,
            TxProviderFlowSafetyIssue::MissingRecentBlockhash
                | TxProviderFlowSafetyIssue::MissingClusterTopology
                | TxProviderFlowSafetyIssue::MissingLeaderSchedule
                | TxProviderFlowSafetyIssue::MissingTipSlot
                | TxProviderFlowSafetyIssue::MissingLeaderRoutingTargets
        )
    }) {
        return TxProviderControlPlaneQuality::IncompleteControlPlane;
    }

    if issues.iter().any(|issue| {
        matches!(
            issue,
            TxProviderFlowSafetyIssue::StaleRecentBlockhash { .. }
                | TxProviderFlowSafetyIssue::StaleClusterTopology { .. }
                | TxProviderFlowSafetyIssue::StaleLeaderSchedule { .. }
        )
    }) {
        return TxProviderControlPlaneQuality::Stale;
    }

    if issues.iter().any(|issue| {
        matches!(
            issue,
            TxProviderFlowSafetyIssue::IncoherentControlPlane { .. }
        )
    }) {
        return TxProviderControlPlaneQuality::Degraded;
    }

    TxProviderControlPlaneQuality::Stable
}

/// Adds one stale-input issue when the observed slot lags the tip beyond policy.
fn apply_slot_lag_issue<F>(
    issues: &mut Vec<TxProviderFlowSafetyIssue>,
    observed_slot: Option<u64>,
    max_allowed: Option<u64>,
    build_issue: F,
    tip_slot: u64,
) where
    F: FnOnce(u64, u64) -> TxProviderFlowSafetyIssue,
{
    let Some(observed_slot) = observed_slot else {
        return;
    };
    let Some(max_allowed) = max_allowed else {
        return;
    };

    let slot_lag = tip_slot.saturating_sub(observed_slot);
    if slot_lag > max_allowed {
        issues.push(build_issue(slot_lag, max_allowed));
    }
}

/// Synthesizes Agave's default TPU QUIC port when QUIC is absent but UDP is known.
pub(crate) fn with_agave_quic_fallback(
    udp_addr: Option<SocketAddr>,
    quic_addr: Option<SocketAddr>,
) -> Option<SocketAddr> {
    quic_addr.or_else(|| {
        let mut addr = udp_addr?;
        let port = addr.port().checked_add(AGAVE_QUIC_PORT_OFFSET)?;
        addr.set_port(port);
        Some(addr)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flow_safety_marks_missing_inputs_as_incomplete() {
        let report = evaluate_flow_safety(
            TxProviderControlPlaneSnapshot {
                latest_recent_blockhash_slot: None,
                cluster_topology_slot: None,
                leader_schedule_slot: None,
                tip_slot: None,
                known_ingress_nodes: 0,
                known_leader_slots: 0,
                known_leader_target_addrs: 0,
                known_leader_target_identities: 0,
            },
            TxProviderFlowSafetyPolicy::default(),
        );

        assert_eq!(
            report.quality,
            TxProviderControlPlaneQuality::IncompleteControlPlane
        );
        assert_eq!(
            report.recent_blockhash_freshness.state,
            TxProviderFreshnessState::Missing
        );
        assert!(!report.is_safe());
    }

    #[test]
    fn flow_safety_marks_incoherent_inputs_as_degraded() {
        let report = evaluate_flow_safety(
            TxProviderControlPlaneSnapshot {
                latest_recent_blockhash_slot: Some(100),
                cluster_topology_slot: Some(120),
                leader_schedule_slot: Some(110),
                tip_slot: Some(121),
                known_ingress_nodes: 2,
                known_leader_slots: 4,
                known_leader_target_addrs: 2,
                known_leader_target_identities: 1,
            },
            TxProviderFlowSafetyPolicy {
                require_recent_blockhash: true,
                require_cluster_topology: true,
                require_leader_schedule: true,
                require_tip_slot: true,
                require_routable_leader_targets: true,
                max_recent_blockhash_slot_lag: Some(64),
                max_cluster_topology_slot_lag: Some(64),
                max_leader_schedule_slot_lag: Some(64),
                max_control_plane_slot_spread: Some(8),
            },
        );

        assert_eq!(report.control_plane_slot_spread, Some(20));
        assert!(!report.inputs_aligned);
        assert_eq!(report.quality, TxProviderControlPlaneQuality::Degraded);
        assert!(
            report
                .issues
                .contains(&TxProviderFlowSafetyIssue::IncoherentControlPlane {
                    slot_spread: 20,
                    max_allowed: 8,
                })
        );
    }

    #[test]
    fn flow_safety_marks_stale_inputs_as_stale() {
        let report = evaluate_flow_safety(
            TxProviderControlPlaneSnapshot {
                latest_recent_blockhash_slot: Some(10),
                cluster_topology_slot: Some(200),
                leader_schedule_slot: Some(200),
                tip_slot: Some(200),
                known_ingress_nodes: 1,
                known_leader_slots: 1,
                known_leader_target_addrs: 1,
                known_leader_target_identities: 1,
            },
            TxProviderFlowSafetyPolicy::default(),
        );

        assert_eq!(report.quality, TxProviderControlPlaneQuality::Stale);
        assert_eq!(
            report.recent_blockhash_freshness.state,
            TxProviderFreshnessState::Stale
        );
        assert!(
            report
                .issues
                .contains(&TxProviderFlowSafetyIssue::StaleRecentBlockhash {
                    slot_lag: 190,
                    max_allowed: 32,
                })
        );
    }
}
