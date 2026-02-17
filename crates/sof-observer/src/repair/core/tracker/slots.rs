use super::*;

impl MissingShredTracker {
    pub(super) fn tick_ms(&self, now: Instant) -> u64 {
        let elapsed = now.saturating_duration_since(self.epoch);
        duration_to_ms_u64(elapsed)
    }

    pub(super) fn touch_slot(&mut self, slot: u64, tick_ms: u64) {
        if !self.has_latest_slot || slot > self.latest_slot {
            self.latest_slot = slot;
            self.has_latest_slot = true;
            self.evict_old_slots();
        }
        self.ensure_slot_state(slot, tick_ms);
    }

    pub(super) fn touch_slot_seed(&mut self, slot: u64, tick_ms: u64) {
        self.ensure_slot_state(slot, tick_ms);
    }

    pub(super) fn ensure_slot_state(&mut self, slot: u64, tick_ms: u64) {
        let slot_state = self.slots.entry(slot).or_default();
        if slot_state.first_seen_tick_ms.is_none() {
            slot_state.first_seen_tick_ms = Some(tick_ms);
        }
    }

    pub(super) fn evict_old_slots(&mut self) {
        if !self.has_latest_slot {
            return;
        }
        let floor = self.latest_slot.saturating_sub(self.slot_window);
        self.slots.retain(|slot, _| *slot >= floor);
    }

    pub(super) fn cleanup_complete_sets(&mut self) {
        for slot_state in self.slots.values_mut() {
            let last_index_seen = slot_state.last_index_seen;
            slot_state.sets.retain(|fec_set_index, set| {
                if let Some(last_index) = last_index_seen
                    && *fec_set_index > last_index
                {
                    return false;
                }
                !set.is_complete()
            });
        }
    }

    pub(super) fn seed_backfill_sets(
        &mut self,
        slot: u64,
        anchor_fec_set_index: u32,
        tick_ms: u64,
    ) {
        let Some(slot_state) = self.slots.get_mut(&slot) else {
            return;
        };
        if self.auto_backfill_sets == 0 {
            return;
        }
        if !slot_state.backfill_seeded {
            for set in 0..self.auto_backfill_sets {
                let fec_set_index = u32::try_from(set)
                    .ok()
                    .and_then(|set_u32| set_u32.checked_mul(DATA_SHREDS_PER_FEC_SET as u32));
                let Some(fec_set_index) = fec_set_index else {
                    break;
                };
                let _ = slot_state
                    .sets
                    .entry(fec_set_index)
                    .or_insert_with(|| FecSetMissingState::new(tick_ms));
            }
            slot_state.backfill_seeded = true;
        }

        let set_width = DATA_SHREDS_PER_FEC_SET as u32;
        let trailing_sets = u32::try_from(self.auto_backfill_sets.saturating_sub(1)).unwrap_or(0);
        let trailing_span = trailing_sets.saturating_mul(set_width);
        let mut fec_set_index = anchor_fec_set_index.saturating_sub(trailing_span);
        let aligned_remainder = fec_set_index.checked_rem(set_width).unwrap_or(0);
        fec_set_index = fec_set_index.saturating_sub(aligned_remainder);
        while fec_set_index <= anchor_fec_set_index {
            let _ = slot_state
                .sets
                .entry(fec_set_index)
                .or_insert_with(|| FecSetMissingState::new(tick_ms));
            let Some(next) = fec_set_index.checked_add(set_width) else {
                break;
            };
            fec_set_index = next;
        }
    }

    pub(super) fn seed_next_slot_probe(&mut self, slot: u64, tick_ms: u64) {
        let Some(next_slot) = slot.checked_add(1) else {
            return;
        };
        self.touch_slot_seed(next_slot, tick_ms);
        if let Some(state) = self.slots.get_mut(&next_slot)
            && state.received_upper_bound().is_none()
        {
            state.seed_highest_probe = true;
        }
    }

    pub(super) fn seed_forward_highest_probes(&mut self, tick_ms: u64) {
        if !self.has_latest_slot || self.tip_probe_ahead_slots == 0 {
            return;
        }
        let mut probe_slot = self.latest_slot;
        for _ in 0..self.tip_probe_ahead_slots {
            let Some(next_slot) = probe_slot.checked_add(1) else {
                break;
            };
            probe_slot = next_slot;
            self.touch_slot_seed(probe_slot, tick_ms);
            if let Some(state) = self.slots.get_mut(&probe_slot)
                && state.received_upper_bound().is_none()
            {
                state.seed_highest_probe = true;
            }
        }
    }
}
