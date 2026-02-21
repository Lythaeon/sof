//! `sof` plugin adapter that bridges runtime observations into `sof-tx` providers.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::SocketAddr,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use sof::framework::{
    ClusterNodeInfo, ClusterTopologyEvent, LeaderScheduleEntry, LeaderScheduleEvent,
    ObservedRecentBlockhashEvent, ObserverPlugin, PluginHost,
};
use solana_pubkey::Pubkey;

use crate::providers::{LeaderProvider, LeaderTarget, RecentBlockhashProvider};

/// Configuration for [`PluginHostTxProviderAdapter`].
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PluginHostTxProviderAdapterConfig {
    /// Maximum number of slot-to-leader assignments retained in memory.
    pub max_leader_slots: usize,
    /// Maximum number of next leaders returned from [`LeaderProvider::next_leaders`].
    pub max_next_leaders: usize,
}

impl PluginHostTxProviderAdapterConfig {
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

impl Default for PluginHostTxProviderAdapterConfig {
    fn default() -> Self {
        Self {
            max_leader_slots: 2_048,
            max_next_leaders: 8,
        }
    }
}

/// Shared adapter that can be registered as a SOF plugin and used as tx providers.
///
/// This type ingests SOF control-plane hooks (`on_recent_blockhash`, `on_cluster_topology`,
/// `on_leader_schedule`) and exposes state through [`RecentBlockhashProvider`] and
/// [`LeaderProvider`].
#[derive(Debug, Clone)]
pub struct PluginHostTxProviderAdapter {
    /// Shared mutable adapter state updated by plugin callbacks.
    inner: Arc<RwLock<AdapterState>>,
    /// Adapter configuration.
    config: PluginHostTxProviderAdapterConfig,
}

impl PluginHostTxProviderAdapter {
    /// Creates a new adapter with the provided config.
    #[must_use]
    pub fn new(config: PluginHostTxProviderAdapterConfig) -> Self {
        Self {
            inner: Arc::new(RwLock::new(AdapterState::default())),
            config: config.normalized(),
        }
    }

    /// Seeds adapter state from already-observed values in `PluginHost`.
    ///
    /// This is useful when attaching the adapter after runtime state has already started
    /// accumulating.
    pub fn prime_from_plugin_host(&self, host: &PluginHost) {
        self.with_state_write(|state| {
            if let Some((_slot, recent_blockhash)) = host.latest_observed_recent_blockhash() {
                state.latest_recent_blockhash = Some(recent_blockhash);
            }
            if let Some(entry) = host.latest_observed_tpu_leader() {
                state.upsert_leader(entry);
                state.advance_cursor(entry.slot);
                cap_leader_slots(state, self.config.max_leader_slots);
            }
        });
    }

    /// Inserts or updates one TPU address mapping for a leader identity.
    pub fn set_leader_tpu_addr(&self, identity: Pubkey, tpu_addr: SocketAddr) {
        self.with_state_write(|state| {
            let _ = state.tpu_by_identity.insert(identity, tpu_addr);
        });
    }

    /// Removes one TPU address mapping for a leader identity.
    pub fn remove_leader_tpu_addr(&self, identity: Pubkey) {
        self.with_state_write(|state| {
            let _ = state.tpu_by_identity.remove(&identity);
        });
    }

    /// Returns current leader plus up to `next_leaders` future leaders, in slot order.
    #[must_use]
    fn leader_window(&self, next_leaders: usize) -> Vec<LeaderTarget> {
        let capped_next = next_leaders.min(self.config.max_next_leaders);
        let requested = capped_next.saturating_add(1);
        if requested == 0 {
            return Vec::new();
        }
        self.inner.read().ok().map_or_else(Vec::new, |state| {
            collect_leader_targets_from_state(&state, requested)
        })
    }

    /// Updates adapter state behind write lock when available.
    fn with_state_write<F>(&self, mut apply: F)
    where
        F: FnMut(&mut AdapterState),
    {
        if let Ok(mut state) = self.inner.write() {
            apply(&mut state);
        }
    }
}

impl Default for PluginHostTxProviderAdapter {
    fn default() -> Self {
        Self::new(PluginHostTxProviderAdapterConfig::default())
    }
}

impl RecentBlockhashProvider for PluginHostTxProviderAdapter {
    fn latest_blockhash(&self) -> Option<[u8; 32]> {
        self.inner
            .read()
            .ok()
            .and_then(|state| state.latest_recent_blockhash)
    }
}

impl LeaderProvider for PluginHostTxProviderAdapter {
    fn current_leader(&self) -> Option<LeaderTarget> {
        self.leader_window(0).into_iter().next()
    }

    fn next_leaders(&self, n: usize) -> Vec<LeaderTarget> {
        if n == 0 {
            return Vec::new();
        }
        self.leader_window(n).into_iter().skip(1).take(n).collect()
    }
}

#[async_trait]
impl ObserverPlugin for PluginHostTxProviderAdapter {
    fn name(&self) -> &'static str {
        "sof-tx-provider-adapter"
    }

    async fn on_recent_blockhash(&self, event: ObservedRecentBlockhashEvent) {
        self.with_state_write(|state| {
            state.latest_recent_blockhash = Some(event.recent_blockhash);
        });
    }

    async fn on_cluster_topology(&self, event: ClusterTopologyEvent) {
        self.with_state_write(|state| {
            apply_cluster_topology(state, &event);
        });
    }

    async fn on_leader_schedule(&self, event: LeaderScheduleEvent) {
        self.with_state_write(|state| {
            apply_leader_schedule(state, &event, self.config.max_leader_slots);
        });
    }
}

/// Mutable adapter state populated from SOF plugin hooks.
#[derive(Debug, Default)]
struct AdapterState {
    /// Most recent observed blockhash.
    latest_recent_blockhash: Option<[u8; 32]>,
    /// Leader assignment indexed by slot.
    leader_by_slot: BTreeMap<u64, Pubkey>,
    /// Most recent slot cursor from leader-schedule events.
    leader_slot_cursor: Option<u64>,
    /// TPU endpoints keyed by validator identity.
    tpu_by_identity: HashMap<Pubkey, SocketAddr>,
}

impl AdapterState {
    /// Inserts or updates one leader assignment.
    fn upsert_leader(&mut self, entry: LeaderScheduleEntry) {
        let _ = self.leader_by_slot.insert(entry.slot, entry.leader);
    }

    /// Advances slot cursor when the provided slot is newer.
    const fn advance_cursor(&mut self, slot: u64) {
        match self.leader_slot_cursor {
            Some(current) if slot < current => {}
            Some(_) | None => {
                self.leader_slot_cursor = Some(slot);
            }
        }
    }
}

/// Applies one topology event to TPU endpoint state.
fn apply_cluster_topology(state: &mut AdapterState, event: &ClusterTopologyEvent) {
    if !event.snapshot_nodes.is_empty() {
        state.tpu_by_identity.clear();
        insert_node_tpus(&event.snapshot_nodes, &mut state.tpu_by_identity);
    }
    insert_node_tpus(&event.added_nodes, &mut state.tpu_by_identity);
    insert_node_tpus(&event.updated_nodes, &mut state.tpu_by_identity);
    for pubkey in &event.removed_pubkeys {
        let _ = state.tpu_by_identity.remove(pubkey);
    }
}

/// Inserts node TPU mappings into lookup map.
fn insert_node_tpus(nodes: &[ClusterNodeInfo], tpu_by_identity: &mut HashMap<Pubkey, SocketAddr>) {
    for node in nodes {
        match node.tpu {
            Some(tpu_addr) => {
                let _ = tpu_by_identity.insert(node.pubkey, tpu_addr);
            }
            None => {
                let _ = tpu_by_identity.remove(&node.pubkey);
            }
        }
    }
}

/// Applies one leader-schedule event to assignment state.
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

/// Trims retained leader assignments to bounded size.
fn cap_leader_slots(state: &mut AdapterState, max_leader_slots: usize) {
    while state.leader_by_slot.len() > max_leader_slots {
        let oldest_slot = state
            .leader_by_slot
            .first_key_value()
            .map(|(slot, _)| *slot);
        if let Some(slot) = oldest_slot {
            let _ = state.leader_by_slot.remove(&slot);
        } else {
            break;
        }
    }
}

/// Collects leader targets from adapter state.
fn collect_leader_targets_from_state(state: &AdapterState, requested: usize) -> Vec<LeaderTarget> {
    let mut selected = Vec::new();
    let mut seen_addrs = HashSet::new();

    if let Some(cursor) = state.leader_slot_cursor {
        append_targets(
            &mut selected,
            &mut seen_addrs,
            state,
            state
                .leader_by_slot
                .range(cursor..)
                .map(|(_, leader)| leader),
            requested,
        );
        if selected.len() < requested {
            append_targets(
                &mut selected,
                &mut seen_addrs,
                state,
                state
                    .leader_by_slot
                    .range(..cursor)
                    .map(|(_, leader)| leader),
                requested,
            );
        }
    } else {
        append_targets(
            &mut selected,
            &mut seen_addrs,
            state,
            state.leader_by_slot.values(),
            requested,
        );
    }

    selected
}

/// Appends mapped leader targets from an iterator until capacity is reached.
fn append_targets<'leader, I>(
    selected: &mut Vec<LeaderTarget>,
    seen_addrs: &mut HashSet<SocketAddr>,
    state: &AdapterState,
    leaders: I,
    requested: usize,
) where
    I: Iterator<Item = &'leader Pubkey>,
{
    for leader in leaders {
        if selected.len() >= requested {
            break;
        }
        let tpu_addr = state.tpu_by_identity.get(leader).copied();
        if let Some(tpu_addr) = tpu_addr
            && seen_addrs.insert(tpu_addr)
        {
            selected.push(LeaderTarget::new(Some(*leader), tpu_addr));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sof::framework::{ControlPlaneSource, PluginHost};

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
            tvu: None,
            rpc: None,
        }
    }

    fn topology_snapshot(nodes: Vec<ClusterNodeInfo>) -> ClusterTopologyEvent {
        ClusterTopologyEvent {
            source: ControlPlaneSource::Direct,
            slot: Some(100),
            epoch: None,
            active_entrypoint: None,
            total_nodes: nodes.len(),
            added_nodes: Vec::new(),
            removed_pubkeys: Vec::new(),
            updated_nodes: Vec::new(),
            snapshot_nodes: nodes,
        }
    }

    fn leader_snapshot(
        slot: u64,
        snapshot_leaders: Vec<LeaderScheduleEntry>,
    ) -> LeaderScheduleEvent {
        LeaderScheduleEvent {
            source: ControlPlaneSource::Direct,
            slot: Some(slot),
            epoch: None,
            added_leaders: Vec::new(),
            removed_slots: Vec::new(),
            updated_leaders: Vec::new(),
            snapshot_leaders,
        }
    }

    #[tokio::test]
    async fn adapter_updates_recent_blockhash_provider() {
        let adapter = PluginHostTxProviderAdapter::default();
        assert_eq!(adapter.latest_blockhash(), None);

        adapter
            .on_recent_blockhash(ObservedRecentBlockhashEvent {
                slot: 10,
                recent_blockhash: [7_u8; 32],
                dataset_tx_count: 1,
            })
            .await;

        assert_eq!(adapter.latest_blockhash(), Some([7_u8; 32]));
    }

    #[tokio::test]
    async fn adapter_maps_topology_and_leaders_into_targets() {
        let adapter = PluginHostTxProviderAdapter::default();
        let leader_a = Pubkey::new_unique();
        let leader_b = Pubkey::new_unique();
        let leader_c = Pubkey::new_unique();

        adapter
            .on_cluster_topology(topology_snapshot(vec![
                node(leader_a, 9001),
                node(leader_b, 9002),
                node(leader_c, 9003),
            ]))
            .await;
        adapter
            .on_leader_schedule(leader_snapshot(
                100,
                vec![
                    LeaderScheduleEntry {
                        slot: 100,
                        leader: leader_a,
                    },
                    LeaderScheduleEntry {
                        slot: 101,
                        leader: leader_b,
                    },
                    LeaderScheduleEntry {
                        slot: 102,
                        leader: leader_c,
                    },
                ],
            ))
            .await;

        let current = adapter.current_leader();
        assert_eq!(current, Some(LeaderTarget::new(Some(leader_a), addr(9001))));

        let next = adapter.next_leaders(2);
        let expected_b = LeaderTarget::new(Some(leader_b), addr(9002));
        let expected_c = LeaderTarget::new(Some(leader_c), addr(9003));
        assert_eq!(next.first(), Some(&expected_b));
        assert_eq!(next.get(1), Some(&expected_c));
    }

    #[tokio::test]
    async fn adapter_retains_bounded_leader_slots() {
        let adapter = PluginHostTxProviderAdapter::new(PluginHostTxProviderAdapterConfig {
            max_leader_slots: 2,
            max_next_leaders: 8,
        });
        let leader_a = Pubkey::new_unique();
        let leader_b = Pubkey::new_unique();
        let leader_c = Pubkey::new_unique();

        adapter.set_leader_tpu_addr(leader_a, addr(9011));
        adapter.set_leader_tpu_addr(leader_b, addr(9012));
        adapter.set_leader_tpu_addr(leader_c, addr(9013));

        adapter
            .on_leader_schedule(leader_snapshot(
                22,
                vec![
                    LeaderScheduleEntry {
                        slot: 20,
                        leader: leader_a,
                    },
                    LeaderScheduleEntry {
                        slot: 21,
                        leader: leader_b,
                    },
                    LeaderScheduleEntry {
                        slot: 22,
                        leader: leader_c,
                    },
                ],
            ))
            .await;

        let current = adapter.current_leader();
        assert_eq!(current, Some(LeaderTarget::new(Some(leader_c), addr(9013))));

        let next = adapter.next_leaders(1);
        assert_eq!(
            next.first(),
            Some(&LeaderTarget::new(Some(leader_b), addr(9012)))
        );
    }

    #[test]
    fn adapter_can_be_primed_from_plugin_host_state() {
        let host = PluginHost::builder().build();
        let leader = Pubkey::new_unique();
        host.on_recent_blockhash(ObservedRecentBlockhashEvent {
            slot: 42,
            recent_blockhash: [11_u8; 32],
            dataset_tx_count: 3,
        });
        host.on_leader_schedule(LeaderScheduleEvent {
            source: ControlPlaneSource::Direct,
            slot: Some(42),
            epoch: None,
            added_leaders: vec![LeaderScheduleEntry { slot: 42, leader }],
            removed_slots: Vec::new(),
            updated_leaders: Vec::new(),
            snapshot_leaders: Vec::new(),
        });

        let adapter = PluginHostTxProviderAdapter::default();
        adapter.prime_from_plugin_host(&host);
        assert_eq!(adapter.latest_blockhash(), Some([11_u8; 32]));
        assert_eq!(adapter.current_leader(), None);

        adapter.set_leader_tpu_addr(leader, addr(9021));
        assert_eq!(
            adapter.current_leader(),
            Some(LeaderTarget::new(Some(leader), addr(9021)))
        );
    }
}
