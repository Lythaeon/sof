#[cfg(feature = "gossip-bootstrap")]
use super::*;

#[cfg(feature = "gossip-bootstrap")]
pub(super) struct ClusterTopologyTracker {
    last_nodes: HashMap<Pubkey, ClusterNodeInfo>,
    last_polled_at: Option<Instant>,
    last_snapshot_at: Option<Instant>,
    poll_interval: Duration,
    snapshot_interval: Duration,
}

#[cfg(feature = "gossip-bootstrap")]
impl ClusterTopologyTracker {
    pub(super) fn new(poll_interval: Duration, snapshot_interval: Duration) -> Self {
        Self {
            last_nodes: HashMap::new(),
            last_polled_at: None,
            last_snapshot_at: None,
            poll_interval,
            snapshot_interval,
        }
    }

    pub(super) fn maybe_build_event(
        &mut self,
        cluster_info: &ClusterInfo,
        latest_slot: Option<u64>,
        active_entrypoint: Option<String>,
        now: Instant,
    ) -> Option<ClusterTopologyEvent> {
        if !self.should_poll(now) {
            return None;
        }
        self.last_polled_at = Some(now);

        let mut current_nodes: HashMap<Pubkey, ClusterNodeInfo> = HashMap::new();
        for (contact_info, _) in cluster_info.all_peers() {
            let node = cluster_node_info_from_contact(&contact_info);
            let _ = current_nodes.insert(node.pubkey, node);
        }

        let mut added_nodes = Vec::new();
        let mut updated_nodes = Vec::new();
        let mut removed_pubkeys = Vec::new();

        for (pubkey, node) in &current_nodes {
            match self.last_nodes.get(pubkey) {
                None => added_nodes.push(node.clone()),
                Some(previous) if previous != node => updated_nodes.push(node.clone()),
                Some(_) => {}
            }
        }
        for pubkey in self.last_nodes.keys() {
            if !current_nodes.contains_key(pubkey) {
                removed_pubkeys.push(*pubkey);
            }
        }

        sort_cluster_nodes(&mut added_nodes);
        sort_cluster_nodes(&mut updated_nodes);
        removed_pubkeys.sort_unstable_by_key(Pubkey::to_bytes);

        let emit_snapshot = self
            .last_snapshot_at
            .is_none_or(|last| now.saturating_duration_since(last) >= self.snapshot_interval);
        if added_nodes.is_empty()
            && updated_nodes.is_empty()
            && removed_pubkeys.is_empty()
            && !emit_snapshot
        {
            return None;
        }

        if emit_snapshot {
            self.last_snapshot_at = Some(now);
        }
        let snapshot_nodes = if emit_snapshot {
            let mut nodes: Vec<ClusterNodeInfo> = current_nodes.values().cloned().collect();
            sort_cluster_nodes(&mut nodes);
            nodes
        } else {
            Vec::new()
        };

        self.last_nodes = current_nodes;
        Some(ClusterTopologyEvent {
            source: ControlPlaneSource::GossipBootstrap,
            slot: latest_slot,
            epoch: None,
            active_entrypoint,
            total_nodes: self.last_nodes.len(),
            added_nodes,
            removed_pubkeys,
            updated_nodes,
            snapshot_nodes,
        })
    }

    fn should_poll(&self, now: Instant) -> bool {
        self.last_polled_at
            .is_none_or(|last| now.saturating_duration_since(last) >= self.poll_interval)
    }
}

#[cfg(feature = "gossip-bootstrap")]
pub(super) fn emit_leader_schedule_diff_event(
    plugin_host: &PluginHost,
    verifier: &mut ShredVerifier,
    latest_slot: Option<u64>,
    emitted_slot_leaders: &mut HashMap<u64, [u8; 32]>,
) {
    let diff = verifier.take_slot_leader_diff();
    if diff.added.is_empty() && diff.updated.is_empty() && diff.removed_slots.is_empty() {
        return;
    }

    let mut added_leaders: Vec<LeaderScheduleEntry> = diff
        .added
        .into_iter()
        .map(|(slot, leader)| LeaderScheduleEntry {
            slot,
            leader: Pubkey::new_from_array(leader),
        })
        .collect();
    let mut updated_leaders: Vec<LeaderScheduleEntry> = diff
        .updated
        .into_iter()
        .map(|(slot, leader)| LeaderScheduleEntry {
            slot,
            leader: Pubkey::new_from_array(leader),
        })
        .collect();
    let mut removed_slots = diff.removed_slots;

    sort_leader_entries(&mut added_leaders);
    sort_leader_entries(&mut updated_leaders);
    removed_slots.sort_unstable();
    for entry in &added_leaders {
        let _ = emitted_slot_leaders.insert(entry.slot, entry.leader.to_bytes());
    }
    for entry in &updated_leaders {
        let _ = emitted_slot_leaders.insert(entry.slot, entry.leader.to_bytes());
    }
    for slot in &removed_slots {
        let _ = emitted_slot_leaders.remove(slot);
    }

    let event_slot = added_leaders
        .last()
        .map(|entry| entry.slot)
        .or_else(|| updated_leaders.last().map(|entry| entry.slot))
        .or_else(|| removed_slots.last().copied())
        .or(latest_slot);

    plugin_host.on_leader_schedule(LeaderScheduleEvent {
        source: ControlPlaneSource::GossipBootstrap,
        slot: event_slot,
        epoch: None,
        added_leaders,
        removed_slots,
        updated_leaders,
        snapshot_leaders: Vec::new(),
    });
}

#[cfg(feature = "gossip-bootstrap")]
pub(super) fn emit_observed_slot_leader_event(
    plugin_host: &PluginHost,
    verifier: &ShredVerifier,
    observed_slot: u64,
    emitted_slot_leaders: &mut HashMap<u64, [u8; 32]>,
    slot_leader_window: u64,
) {
    let Some(leader_bytes) = verifier.slot_leader_for_slot(observed_slot) else {
        return;
    };
    let previous = emitted_slot_leaders.insert(observed_slot, leader_bytes);
    let leader = Pubkey::new_from_array(leader_bytes);
    let (added_leaders, updated_leaders) = match previous {
        None => (
            vec![LeaderScheduleEntry {
                slot: observed_slot,
                leader,
            }],
            Vec::new(),
        ),
        Some(previous_leader) if previous_leader != leader_bytes => (
            Vec::new(),
            vec![LeaderScheduleEntry {
                slot: observed_slot,
                leader,
            }],
        ),
        Some(_) => return,
    };

    let floor = observed_slot.saturating_sub(slot_leader_window);
    emitted_slot_leaders.retain(|slot, _| *slot >= floor);

    plugin_host.on_leader_schedule(LeaderScheduleEvent {
        source: ControlPlaneSource::GossipBootstrap,
        slot: Some(observed_slot),
        epoch: None,
        added_leaders,
        removed_slots: Vec::new(),
        updated_leaders,
        snapshot_leaders: Vec::new(),
    });
}

#[cfg(feature = "gossip-bootstrap")]
fn cluster_node_info_from_contact(contact_info: &ContactInfo) -> ClusterNodeInfo {
    ClusterNodeInfo {
        pubkey: *contact_info.pubkey(),
        wallclock: contact_info.wallclock(),
        shred_version: contact_info.shred_version(),
        gossip: contact_info.gossip(),
        tpu: contact_info.tpu(solana_gossip::contact_info::Protocol::UDP),
        tvu: contact_info.tvu(solana_gossip::contact_info::Protocol::UDP),
        rpc: contact_info.rpc(),
    }
}

#[cfg(feature = "gossip-bootstrap")]
fn sort_cluster_nodes(nodes: &mut [ClusterNodeInfo]) {
    nodes.sort_unstable_by_key(|node| node.pubkey.to_bytes());
}

#[cfg(feature = "gossip-bootstrap")]
fn sort_leader_entries(entries: &mut [LeaderScheduleEntry]) {
    entries.sort_unstable_by_key(|entry| entry.slot);
}
