//! `sof` plugin adapter that bridges runtime observations into `sof-tx` providers.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::SocketAddr,
};

use arcshift::ArcShift;

use async_trait::async_trait;
use sof::framework::{
    ClusterNodeInfo, ClusterTopologyEvent, LeaderScheduleEntry, LeaderScheduleEvent,
    ObservedRecentBlockhashEvent, ObserverPlugin, PluginHost,
};
use solana_pubkey::Pubkey;

use crate::providers::{LeaderProvider, LeaderTarget, RecentBlockhashProvider};
/// Agave's TPU QUIC port is derived by adding this offset to the TPU UDP port.
const AGAVE_QUIC_PORT_OFFSET: u16 = 6;

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
            max_next_leaders: 128,
        }
    }
}

/// A boxed closure utilized to sequentially apply updates to `AdapterState` lock-free.
type AdapterUpdateFn = Box<dyn FnOnce(&mut AdapterState) + Send>;

/// Shared adapter that can be registered as a SOF plugin and used as tx providers.
///
/// This type ingests SOF control-plane hooks (`on_recent_blockhash`, `on_cluster_topology`,
/// `on_leader_schedule`) and exposes state through [`RecentBlockhashProvider`] and
/// [`LeaderProvider`].
#[derive(Debug, Clone)]
pub struct PluginHostTxProviderAdapter {
    /// Shared mutable adapter state updated by plugin callbacks.
    state: ArcShift<AdapterState>,
    /// Serialize updates without locks.
    update_tx: tokio::sync::mpsc::UnboundedSender<AdapterUpdateFn>,
    /// Adapter configuration.
    config: PluginHostTxProviderAdapterConfig,
}

impl PluginHostTxProviderAdapter {
    /// Creates a new adapter with the provided config.
    #[must_use]
    pub fn new(config: PluginHostTxProviderAdapterConfig) -> Self {
        let state = ArcShift::new(AdapterState::default());
        let (update_tx, mut update_rx) = tokio::sync::mpsc::unbounded_channel::<AdapterUpdateFn>();

        let mut worker_state = state.clone();
        tokio::spawn(async move {
            while let Some(apply) = update_rx.recv().await {
                let mut current = worker_state.get().clone();
                apply(&mut current);
                worker_state.update(current);
            }
        });

        Self {
            state,
            update_tx,
            config: config.normalized(),
        }
    }

    /// Seeds adapter state from already-observed values in `PluginHost`.
    ///
    /// This is useful when attaching the adapter after runtime state has already started
    /// accumulating.
    pub fn prime_from_plugin_host(&self, host: &mut PluginHost) {
        let blockhash_opt = host.latest_observed_recent_blockhash();
        let leader_opt = host.latest_observed_tpu_leader();
        let max_leader_slots = self.config.max_leader_slots;

        self.with_state_write(move |state| {
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
    pub fn set_leader_tpu_addr(&self, identity: Pubkey, tpu_addr: SocketAddr) {
        self.with_state_write(move |state| {
            let ingress = state.ingress_by_identity.entry(identity).or_default();
            ingress.tpu = Some(tpu_addr);
            ingress.tpu_quic = with_agave_quic_fallback(Some(tpu_addr), ingress.tpu_quic);
        });
    }

    /// Removes one TPU address mapping for a leader identity.
    pub fn remove_leader_tpu_addr(&self, identity: Pubkey) {
        self.with_state_write(move |state| {
            let _ = state.ingress_by_identity.remove(&identity);
        });
    }

    /// Returns current leader plus up to `next_leaders` future leaders, in slot order.
    #[must_use]
    fn leader_window(&self, next_leaders: usize) -> Vec<LeaderTarget> {
        let capped_next = next_leaders.min(self.config.max_next_leaders);
        let requested_identities = capped_next.saturating_add(1);
        if requested_identities == 0 {
            return Vec::new();
        }
        let requested_targets = requested_identities.saturating_mul(4);
        collect_leader_targets_from_state(&self.state.shared_get(), requested_targets)
    }

    /// Updates adapter state behind write lock when available.
    fn with_state_write<F>(&self, apply: F) -> tokio::sync::oneshot::Receiver<()>
    where
        F: FnOnce(&mut AdapterState) + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.update_tx
            .send(Box::new(move |state| {
                apply(state);
                tx.send(()).ok();
            }))
            .ok();
        rx
    }

    /// Flushes all currently queued updates.
    #[cfg(test)]
    pub async fn flush(&self) {
        self.with_state_write(|_| {}).await.ok();
    }
}

impl Default for PluginHostTxProviderAdapter {
    fn default() -> Self {
        Self::new(PluginHostTxProviderAdapterConfig::default())
    }
}

impl RecentBlockhashProvider for PluginHostTxProviderAdapter {
    fn latest_blockhash(&self) -> Option<[u8; 32]> {
        self.state.shared_get().latest_recent_blockhash
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
        take_next_leader_identity_targets(self.leader_window(n), n)
    }
}

/// Returns targets for the next distinct leader identities after the current leader.
fn take_next_leader_identity_targets(
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

#[async_trait]
impl ObserverPlugin for PluginHostTxProviderAdapter {
    fn name(&self) -> &'static str {
        "sof-tx-provider-adapter"
    }

    async fn on_recent_blockhash(&self, event: ObservedRecentBlockhashEvent) {
        self.with_state_write(move |state| {
            state.latest_recent_blockhash = Some(event.recent_blockhash);
        })
        .await
        .ok();
    }

    async fn on_cluster_topology(&self, event: ClusterTopologyEvent) {
        self.with_state_write(move |state| {
            apply_cluster_topology(state, &event);
        })
        .await
        .ok();
    }

    async fn on_leader_schedule(&self, event: LeaderScheduleEvent) {
        let max_leader_slots = self.config.max_leader_slots;
        self.with_state_write(move |state| {
            apply_leader_schedule(state, &event, max_leader_slots);
        })
        .await
        .ok();
    }
}

/// Mutable adapter state populated from SOF plugin hooks.
#[derive(Debug, Default, Clone)]
struct AdapterState {
    /// Most recent observed blockhash.
    latest_recent_blockhash: Option<[u8; 32]>,
    /// Leader assignment indexed by slot.
    leader_by_slot: BTreeMap<u64, Pubkey>,
    /// Most recent slot cursor from leader-schedule events.
    leader_slot_cursor: Option<u64>,
    /// Ingress endpoints keyed by validator identity.
    ingress_by_identity: HashMap<Pubkey, NodeIngress>,
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

/// Cached ingress endpoints for one validator identity.
#[derive(Debug, Clone, Default)]
struct NodeIngress {
    /// TPU UDP ingress endpoint.
    tpu: Option<SocketAddr>,
    /// TPU QUIC ingress endpoint.
    tpu_quic: Option<SocketAddr>,
    /// TPU forwards UDP ingress endpoint.
    tpu_forwards: Option<SocketAddr>,
    /// TPU forwards QUIC ingress endpoint.
    tpu_forwards_quic: Option<SocketAddr>,
}

impl NodeIngress {
    /// Returns `true` when the node currently exposes no usable ingress address.
    const fn is_empty(&self) -> bool {
        self.tpu.is_none()
            && self.tpu_quic.is_none()
            && self.tpu_forwards.is_none()
            && self.tpu_forwards_quic.is_none()
    }
}

/// Applies one topology event to TPU endpoint state.
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

/// Inserts node ingress mappings into lookup map.
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
    if requested == 0 {
        return selected;
    }

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

    if selected.len() < requested && !state.ingress_by_identity.is_empty() {
        let mut topology_targets = state
            .ingress_by_identity
            .iter()
            .map(|(identity, ingress)| (*identity, ingress.clone()))
            .collect::<Vec<_>>();
        topology_targets.sort_unstable_by_key(|(identity, _ingress)| identity.to_bytes());
        for (identity, ingress) in topology_targets {
            if selected.len() >= requested {
                break;
            }
            for candidate in [
                ingress.tpu_quic,
                ingress.tpu_forwards_quic,
                ingress.tpu,
                ingress.tpu_forwards,
            ]
            .into_iter()
            .flatten()
            {
                if selected.len() >= requested {
                    break;
                }
                if !seen_addrs.insert(candidate) {
                    continue;
                }
                selected.push(LeaderTarget::new(Some(identity), candidate));
            }
        }
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
        let Some(ingress) = state.ingress_by_identity.get(leader) else {
            continue;
        };
        let candidate_addrs = [
            ingress.tpu_quic,
            ingress.tpu_forwards_quic,
            ingress.tpu,
            ingress.tpu_forwards,
        ];
        for candidate in candidate_addrs.into_iter().flatten() {
            if selected.len() >= requested {
                break;
            }
            if !seen_addrs.insert(candidate) {
                continue;
            }
            selected.push(LeaderTarget::new(Some(*leader), candidate));
        }
    }
}

/// Uses an explicit QUIC address when present, otherwise derives Agave's default.
fn with_agave_quic_fallback(
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
            tpu_quic: None,
            tpu_forwards: None,
            tpu_forwards_quic: None,
            tpu_vote: None,
            tvu: None,
            rpc: None,
        }
    }

    fn node_with_forwards(
        pubkey: Pubkey,
        tpu_port: u16,
        tpu_forwards_port: u16,
    ) -> ClusterNodeInfo {
        ClusterNodeInfo {
            pubkey,
            wallclock: 0,
            shred_version: 0,
            gossip: None,
            tpu: Some(addr(tpu_port)),
            tpu_quic: None,
            tpu_forwards: Some(addr(tpu_forwards_port)),
            tpu_forwards_quic: None,
            tpu_vote: None,
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
        assert_eq!(current, Some(LeaderTarget::new(Some(leader_a), addr(9007))));

        let next = adapter.next_leaders(2);
        let expected_b = LeaderTarget::new(Some(leader_b), addr(9008));
        let expected_c = LeaderTarget::new(Some(leader_c), addr(9009));
        assert_eq!(next.first(), Some(&expected_b));
        assert!(next.contains(&expected_c));
    }

    #[tokio::test]
    async fn adapter_falls_back_to_topology_when_schedule_is_unmapped() {
        let adapter = PluginHostTxProviderAdapter::default();
        let unmapped_leader = Pubkey::new_unique();
        let topo_a = Pubkey::new_unique();
        let topo_b = Pubkey::new_unique();

        adapter
            .on_cluster_topology(topology_snapshot(vec![
                node(topo_b, 9122),
                node(topo_a, 9121),
            ]))
            .await;
        adapter
            .on_leader_schedule(leader_snapshot(
                100,
                vec![LeaderScheduleEntry {
                    slot: 100,
                    leader: unmapped_leader,
                }],
            ))
            .await;

        let current = adapter.current_leader();
        assert_eq!(current, Some(LeaderTarget::new(Some(topo_a), addr(9127))));

        let next = adapter.next_leaders(1);
        assert_eq!(
            next,
            vec![
                LeaderTarget::new(Some(topo_b), addr(9128)),
                LeaderTarget::new(Some(topo_b), addr(9122)),
            ]
        );
    }

    #[tokio::test]
    async fn adapter_next_leaders_skip_current_identity_and_return_next_identity() {
        let adapter = PluginHostTxProviderAdapter::default();
        let leader_a = Pubkey::new_unique();
        let leader_b = Pubkey::new_unique();

        adapter
            .on_cluster_topology(topology_snapshot(vec![
                node_with_forwards(leader_a, 9041, 9042),
                node(leader_b, 9043),
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
                ],
            ))
            .await;

        let current = adapter.current_leader();
        assert_eq!(current, Some(LeaderTarget::new(Some(leader_a), addr(9047))));

        let next = adapter.next_leaders(1);
        assert_eq!(
            next.first(),
            Some(&LeaderTarget::new(Some(leader_b), addr(9049)))
        );
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
        adapter.flush().await;

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
        assert_eq!(current, Some(LeaderTarget::new(Some(leader_c), addr(9019))));

        let next = adapter.next_leaders(1);
        assert_eq!(
            next.first(),
            Some(&LeaderTarget::new(Some(leader_b), addr(9018)))
        );
    }

    #[tokio::test]
    async fn adapter_can_be_primed_from_plugin_host_state() {
        let mut host = PluginHost::builder().build();
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
        adapter.prime_from_plugin_host(&mut host);
        adapter.flush().await;
        assert_eq!(adapter.latest_blockhash(), Some([11_u8; 32]));
        assert_eq!(adapter.current_leader(), None);

        adapter.set_leader_tpu_addr(leader, addr(9021));
        adapter.flush().await;
        assert_eq!(
            adapter.current_leader(),
            Some(LeaderTarget::new(Some(leader), addr(9027)))
        );
    }
}
