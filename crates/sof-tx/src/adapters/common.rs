//! Shared state reduction for tx-provider adapters.
#![allow(clippy::missing_docs_in_private_items)]

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

/// Serializable snapshot of tx-provider adapter state.
#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TxProviderAdapterSnapshot {
    /// Most recent observed recent blockhash.
    pub latest_recent_blockhash: Option<[u8; 32]>,
    /// Slot-to-leader assignments.
    pub leader_by_slot: Vec<(u64, [u8; 32])>,
    /// Latest observed canonical tip slot.
    pub tip_slot: Option<u64>,
    /// Most recent leader schedule cursor.
    pub leader_slot_cursor: Option<u64>,
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
    state: Arc<Mutex<AdapterState>>,
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
            move |state| {
                state.latest_recent_blockhash = Some(recent_blockhash);
            }
        });
    }

    /// Applies one cluster topology update.
    pub(crate) fn apply_cluster_topology(&self, event: &ClusterTopologyEvent) {
        self.update({
            let event = event.clone();
            move |state| {
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
            state.leader_by_slot = snapshot
                .leader_by_slot
                .into_iter()
                .map(|(slot, identity)| (slot, Pubkey::new_from_array(identity)))
                .collect();
            state.tip_slot = snapshot.tip_slot;
            state.leader_slot_cursor = snapshot.leader_slot_cursor;
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
            leader_by_slot: state
                .leader_by_slot
                .iter()
                .map(|(slot, identity)| (*slot, identity.to_bytes()))
                .collect(),
            tip_slot: state.tip_slot,
            leader_slot_cursor: state.leader_slot_cursor,
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
    latest_recent_blockhash: Option<[u8; 32]>,
    leader_by_slot: BTreeMap<u64, Pubkey>,
    tip_slot: Option<u64>,
    leader_slot_cursor: Option<u64>,
    ingress_by_identity: HashMap<Pubkey, NodeIngress>,
}

impl AdapterState {
    fn upsert_leader(&mut self, entry: LeaderScheduleEntry) {
        let _ = self.leader_by_slot.insert(entry.slot, entry.leader);
    }

    const fn advance_cursor(&mut self, slot: u64) {
        match self.leader_slot_cursor {
            Some(current) if slot < current => {}
            Some(_) | None => {
                self.leader_slot_cursor = Some(slot);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct NodeIngress {
    tpu: Option<SocketAddr>,
    tpu_quic: Option<SocketAddr>,
    tpu_forwards: Option<SocketAddr>,
    tpu_forwards_quic: Option<SocketAddr>,
}

impl NodeIngress {
    const fn is_empty(self) -> bool {
        self.tpu.is_none()
            && self.tpu_quic.is_none()
            && self.tpu_forwards.is_none()
            && self.tpu_forwards_quic.is_none()
    }
}

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
