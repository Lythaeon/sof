use std::collections::HashMap;

/// Prunes one slot-keyed map down to the retained recent window once the threshold is crossed.
pub fn prune_recent_slots<T>(
    slot_states: &mut HashMap<u64, T>,
    slot: u64,
    retained_lag: u64,
    prune_threshold: usize,
) {
    if slot_states.len() <= prune_threshold {
        return;
    }
    let slot_floor = slot.saturating_sub(retained_lag);
    slot_states.retain(|tracked_slot, _| *tracked_slot >= slot_floor);
}

#[cfg(test)]
mod tests {
    use super::prune_recent_slots;

    #[test]
    fn prune_recent_slots_drops_old_entries_after_threshold() {
        let mut slot_states = (0_u64..10_u64).map(|slot| (slot, slot)).collect();

        prune_recent_slots(&mut slot_states, 9, 3, 4);

        assert_eq!(slot_states.len(), 4);
        assert!(!slot_states.contains_key(&5));
        assert!(slot_states.contains_key(&6));
        assert!(slot_states.contains_key(&9));
    }
}
