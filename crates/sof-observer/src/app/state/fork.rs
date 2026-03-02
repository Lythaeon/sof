use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::event::ForkSlotStatus;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ForkTrackerSnapshot {
    pub tracked_slots: usize,
    pub tip_slot: Option<u64>,
    pub confirmed_slot: Option<u64>,
    pub finalized_slot: Option<u64>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ForkStatusTransition {
    pub slot: u64,
    pub parent_slot: Option<u64>,
    pub previous_status: Option<ForkSlotStatus>,
    pub status: ForkSlotStatus,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ForkReorg {
    pub old_tip: u64,
    pub new_tip: u64,
    pub common_ancestor: Option<u64>,
    pub detached_slots: Vec<u64>,
    pub attached_slots: Vec<u64>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ForkTrackerUpdate {
    pub snapshot: ForkTrackerSnapshot,
    pub status_transitions: Vec<ForkStatusTransition>,
    pub reorg: Option<ForkReorg>,
}

impl ForkTrackerUpdate {
    const fn snapshot_only(snapshot: ForkTrackerSnapshot) -> Self {
        Self {
            snapshot,
            status_transitions: Vec::new(),
            reorg: None,
        }
    }
}

#[derive(Debug)]
struct ForkNode {
    parent_slot: Option<u64>,
    children: HashSet<u64>,
    depth: u32,
    was_canonical: bool,
    status: ForkSlotStatus,
}

#[derive(Debug)]
pub struct ForkTracker {
    nodes: HashMap<u64, ForkNode>,
    slots: BTreeSet<u64>,
    max_tracked_slots: u64,
    confirmed_depth_slots: u64,
    finalized_depth_slots: u64,
    max_observed_slot: Option<u64>,
    tip_slot: Option<u64>,
    confirmed_slot: Option<u64>,
    finalized_slot: Option<u64>,
}

impl ForkTracker {
    #[must_use]
    pub fn new(
        max_tracked_slots: u64,
        confirmed_depth_slots: u64,
        finalized_depth_slots: u64,
    ) -> Self {
        Self {
            nodes: HashMap::new(),
            slots: BTreeSet::new(),
            max_tracked_slots: max_tracked_slots.max(1),
            confirmed_depth_slots,
            finalized_depth_slots: finalized_depth_slots.max(confirmed_depth_slots),
            max_observed_slot: None,
            tip_slot: None,
            confirmed_slot: None,
            finalized_slot: None,
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> ForkTrackerSnapshot {
        ForkTrackerSnapshot {
            tracked_slots: self.nodes.len(),
            tip_slot: self.tip_slot,
            confirmed_slot: self.confirmed_slot,
            finalized_slot: self.finalized_slot,
        }
    }

    pub fn observe_data_shred(&mut self, slot: u64, parent_slot: Option<u64>) -> ForkTrackerUpdate {
        self.observe_slot(slot, parent_slot)
    }

    pub fn observe_recovered_data_shred(
        &mut self,
        slot: u64,
        parent_slot: Option<u64>,
    ) -> ForkTrackerUpdate {
        self.observe_slot(slot, parent_slot)
    }

    pub fn observe_code_shred(&mut self, slot: u64) -> ForkTrackerUpdate {
        self.observe_slot(slot, None)
    }

    fn observe_slot(&mut self, slot: u64, parent_slot: Option<u64>) -> ForkTrackerUpdate {
        let parent_slot = parent_slot.filter(|parent| *parent < slot);
        let mut structural_change = false;
        let mut newly_inserted_slot = None;
        let mut depth_recompute_roots = Vec::new();

        if self.nodes.contains_key(&slot) {
            let current_parent = self.nodes.get(&slot).and_then(|node| node.parent_slot);
            if current_parent.is_none() && parent_slot.is_some() {
                let Some(parent) = parent_slot else {
                    return ForkTrackerUpdate::snapshot_only(self.snapshot());
                };
                if let Some(node) = self.nodes.get_mut(&slot) {
                    node.parent_slot = Some(parent);
                }
                if let Some(parent_node) = self.nodes.get_mut(&parent) {
                    parent_node.children.insert(slot);
                }
                structural_change = true;
                depth_recompute_roots.push(slot);
            }
        } else {
            structural_change = true;
            newly_inserted_slot = Some(slot);
            self.max_observed_slot = Some(
                self.max_observed_slot
                    .map_or(slot, |current| current.max(slot)),
            );
            self.slots.insert(slot);
            self.nodes.insert(
                slot,
                ForkNode {
                    parent_slot,
                    children: HashSet::new(),
                    depth: 0,
                    was_canonical: false,
                    status: ForkSlotStatus::Processed,
                },
            );
            if let Some(parent) = parent_slot
                && let Some(parent_node) = self.nodes.get_mut(&parent)
            {
                parent_node.children.insert(slot);
            }
            depth_recompute_roots.push(slot);
            depth_recompute_roots.extend(self.attach_waiting_children(slot));
        }

        if structural_change {
            let detached_children = self.prune_old_slots();
            depth_recompute_roots.extend(detached_children);
            for root in depth_recompute_roots {
                self.recompute_depths_from(root);
            }
            return self.refresh_fork_state(newly_inserted_slot);
        }

        ForkTrackerUpdate::snapshot_only(self.snapshot())
    }

    fn prune_old_slots(&mut self) -> Vec<u64> {
        let Some(max_observed_slot) = self.max_observed_slot else {
            return Vec::new();
        };
        let min_slot = max_observed_slot.saturating_sub(self.max_tracked_slots);
        let to_remove: Vec<u64> = self.slots.range(..min_slot).copied().collect();
        if to_remove.is_empty() {
            return Vec::new();
        }
        let mut detached_children = Vec::new();
        for slot in to_remove {
            self.slots.remove(&slot);
            let Some(node) = self.nodes.remove(&slot) else {
                continue;
            };
            if let Some(parent) = node.parent_slot
                && let Some(parent_node) = self.nodes.get_mut(&parent)
            {
                parent_node.children.remove(&slot);
            }
            for child in node.children {
                if let Some(child_node) = self.nodes.get_mut(&child)
                    && child_node.parent_slot == Some(slot)
                {
                    child_node.parent_slot = None;
                    detached_children.push(child);
                }
            }
        }
        detached_children
    }

    fn recompute_depths_from(&mut self, root: u64) {
        if !self.nodes.contains_key(&root) {
            return;
        }
        let mut queue = VecDeque::new();
        queue.push_back(root);
        while let Some(slot) = queue.pop_front() {
            let (parent_slot, children) = match self.nodes.get(&slot) {
                Some(node) => (
                    node.parent_slot,
                    node.children.iter().copied().collect::<Vec<_>>(),
                ),
                None => continue,
            };
            let new_depth = parent_slot
                .and_then(|parent| {
                    self.nodes
                        .get(&parent)
                        .map(|node| node.depth.saturating_add(1))
                })
                .unwrap_or(0);
            let mut changed = false;
            if let Some(node) = self.nodes.get_mut(&slot)
                && node.depth != new_depth
            {
                node.depth = new_depth;
                changed = true;
            }
            if changed || slot == root {
                queue.extend(children);
            }
        }
    }

    fn attach_waiting_children(&mut self, parent_slot: u64) -> Vec<u64> {
        let waiting_children = self
            .nodes
            .iter()
            .filter_map(|(&slot, node)| (node.parent_slot == Some(parent_slot)).then_some(slot))
            .collect::<Vec<_>>();
        if waiting_children.is_empty() {
            return waiting_children;
        }
        if let Some(parent_node) = self.nodes.get_mut(&parent_slot) {
            parent_node
                .children
                .extend(waiting_children.iter().copied());
        }
        waiting_children
    }

    fn refresh_fork_state(&mut self, newly_inserted_slot: Option<u64>) -> ForkTrackerUpdate {
        let previous_tip = self.tip_slot;
        let old_chain = previous_tip
            .map(|tip| self.ancestor_chain(tip))
            .unwrap_or_default();
        let old_chain_set: HashSet<u64> = old_chain.iter().copied().collect();

        self.tip_slot = self.select_tip_slot();
        self.confirmed_slot = self
            .tip_slot
            .and_then(|tip| self.ancestor_at_distance(tip, self.confirmed_depth_slots));
        self.finalized_slot = self
            .tip_slot
            .and_then(|tip| self.ancestor_at_distance(tip, self.finalized_depth_slots));

        let canonical_chain = self
            .tip_slot
            .map(|tip| self.ancestor_chain(tip))
            .unwrap_or_default();
        let canonical_set: HashSet<u64> = canonical_chain.iter().copied().collect();

        let mut status_transitions = Vec::new();
        for (&slot, node) in &mut self.nodes {
            let on_canonical = canonical_set.contains(&slot);
            let new_status = if on_canonical {
                if self
                    .finalized_slot
                    .is_some_and(|finalized_slot| slot <= finalized_slot)
                {
                    ForkSlotStatus::Finalized
                } else if self
                    .confirmed_slot
                    .is_some_and(|confirmed_slot| slot <= confirmed_slot)
                {
                    ForkSlotStatus::Confirmed
                } else {
                    ForkSlotStatus::Processed
                }
            } else if node.was_canonical {
                ForkSlotStatus::Orphaned
            } else {
                ForkSlotStatus::Processed
            };
            if node.status != new_status {
                status_transitions.push(ForkStatusTransition {
                    slot,
                    parent_slot: node.parent_slot,
                    previous_status: Some(node.status),
                    status: new_status,
                });
                node.status = new_status;
            }
            if on_canonical {
                node.was_canonical = true;
            }
        }
        if let Some(slot) = newly_inserted_slot
            && let Some(node) = self.nodes.get(&slot)
            && !status_transitions
                .iter()
                .any(|transition| transition.slot == slot)
        {
            status_transitions.push(ForkStatusTransition {
                slot,
                parent_slot: node.parent_slot,
                previous_status: None,
                status: node.status,
            });
        }

        let reorg = match (previous_tip, self.tip_slot) {
            (Some(old_tip), Some(new_tip)) if old_tip != new_tip => {
                let new_chain = self.ancestor_chain(new_tip);
                let common_ancestor = new_chain
                    .iter()
                    .copied()
                    .find(|slot| old_chain_set.contains(slot));
                if common_ancestor != Some(old_tip) {
                    let detached_slots = old_chain
                        .iter()
                        .copied()
                        .take_while(|slot| Some(*slot) != common_ancestor)
                        .collect::<Vec<_>>();
                    let mut attached_slots = new_chain
                        .iter()
                        .copied()
                        .take_while(|slot| Some(*slot) != common_ancestor)
                        .collect::<Vec<_>>();
                    attached_slots.reverse();
                    Some(ForkReorg {
                        old_tip,
                        new_tip,
                        common_ancestor,
                        detached_slots,
                        attached_slots,
                    })
                } else {
                    None
                }
            }
            _ => None,
        };

        ForkTrackerUpdate {
            snapshot: self.snapshot(),
            status_transitions,
            reorg,
        }
    }

    fn select_tip_slot(&self) -> Option<u64> {
        let best_metric = self
            .nodes
            .iter()
            .map(|(&slot, node)| (node.depth, slot))
            .max()?;
        if let Some(current_tip) = self.tip_slot
            && let Some(node) = self.nodes.get(&current_tip)
            && (node.depth, current_tip) == best_metric
        {
            return Some(current_tip);
        }
        self.nodes
            .iter()
            .filter_map(|(&slot, node)| ((node.depth, slot) == best_metric).then_some(slot))
            .max()
    }

    fn ancestor_chain(&self, tip_slot: u64) -> Vec<u64> {
        let mut chain = Vec::new();
        let mut cursor = Some(tip_slot);
        let mut guard = 0_usize;
        let guard_limit = self.nodes.len().saturating_add(1);
        while let Some(slot) = cursor {
            chain.push(slot);
            cursor = self
                .nodes
                .get(&slot)
                .and_then(|node| node.parent_slot)
                .filter(|parent| self.nodes.contains_key(parent));
            guard = guard.saturating_add(1);
            if guard > guard_limit {
                break;
            }
        }
        chain
    }

    fn ancestor_at_distance(&self, tip_slot: u64, distance: u64) -> Option<u64> {
        let mut cursor = Some(tip_slot);
        for _ in 0..distance {
            let slot = cursor?;
            cursor = self
                .nodes
                .get(&slot)
                .and_then(|node| node.parent_slot)
                .filter(|parent| self.nodes.contains_key(parent));
        }
        cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_updates_commitment_watermarks_from_ancestor_depth() {
        let mut tracker = ForkTracker::new(512, 1, 2);
        assert_eq!(tracker.snapshot().tip_slot, None);

        let first_update = tracker.observe_data_shred(10, Some(9));
        assert_eq!(first_update.snapshot.tip_slot, Some(10));
        assert_eq!(first_update.snapshot.confirmed_slot, None);
        assert_eq!(first_update.snapshot.finalized_slot, None);

        let second_update = tracker.observe_data_shred(9, Some(8));
        assert_eq!(second_update.snapshot.tip_slot, Some(10));
        assert_eq!(second_update.snapshot.confirmed_slot, Some(9));
        assert_eq!(second_update.snapshot.finalized_slot, None);

        let third_update = tracker.observe_data_shred(11, Some(10));
        assert_eq!(third_update.snapshot.tip_slot, Some(11));
        assert_eq!(third_update.snapshot.confirmed_slot, Some(10));
        assert_eq!(third_update.snapshot.finalized_slot, Some(9));
    }

    #[test]
    fn tracker_emits_reorg_and_orphan_transition() {
        let mut tracker = ForkTracker::new(512, 1, 2);
        let _ = tracker.observe_data_shred(8, None);
        let _ = tracker.observe_data_shred(9, Some(8));
        let _ = tracker.observe_data_shred(10, Some(9));
        let _ = tracker.observe_data_shred(11, Some(10));
        let _ = tracker.observe_data_shred(12, Some(11));

        let _ = tracker.observe_data_shred(13, Some(9));
        let _ = tracker.observe_data_shred(14, Some(13));
        let update = tracker.observe_data_shred(15, Some(14));

        let reorg = update.reorg.expect("reorg expected");
        assert_eq!(reorg.old_tip, 12);
        assert_eq!(reorg.new_tip, 15);
        assert_eq!(reorg.common_ancestor, Some(9));
        assert_eq!(reorg.detached_slots, vec![12, 11, 10]);
        assert_eq!(reorg.attached_slots, vec![13, 14, 15]);

        let orphaned = update
            .status_transitions
            .iter()
            .find(|transition| transition.slot == 12)
            .expect("slot 12 transition exists");
        assert_eq!(orphaned.status, ForkSlotStatus::Orphaned);
    }

    #[test]
    fn tracker_prunes_old_slots_with_window_bound() {
        let mut tracker = ForkTracker::new(4, 1, 2);
        for slot in 0..10_u64 {
            let parent = (slot > 0).then_some(slot.saturating_sub(1));
            let _ = tracker.observe_data_shred(slot, parent);
        }
        let snapshot = tracker.snapshot();
        assert!(snapshot.tracked_slots <= 5);
        assert_eq!(snapshot.tip_slot, Some(9));
    }
}
