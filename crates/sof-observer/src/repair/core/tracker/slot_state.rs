use super::*;

const MILLIS_PER_SECOND: u64 = 1_000;
const NEVER_REQUESTED_TICK_MS: u64 = u64::MAX;
const MIN_EXPECTED_DATA_SHREDS_PER_SET: u16 = 1;

impl SlotMissingState {
    pub(super) fn is_highest_probe_ready(&self) -> bool {
        self.received_upper_bound()
            .map_or(self.seed_highest_probe, |received_upper| {
                self.contiguous_data_prefix >= received_upper
            })
    }

    pub(super) fn observe_data_index(&mut self, index: u32, reference_tick: u8, tick_ms: u64) {
        self.seed_highest_probe = false;
        self.min_data_index_seen = Some(
            self.min_data_index_seen
                .map_or(index, |prev| prev.min(index)),
        );
        self.max_data_index_seen = Some(
            self.max_data_index_seen
                .map_or(index, |prev| prev.max(index)),
        );
        let _ = self
            .received_reference_ticks
            .entry(index)
            .or_insert(reference_tick);
        if self.first_data_tick_ms.is_none() {
            let slot_elapsed_ms = u64::from(reference_tick).saturating_mul(MILLIS_PER_SECOND)
                / DEFAULT_TICKS_PER_SECOND;
            self.first_data_tick_ms = Some(tick_ms.saturating_sub(slot_elapsed_ms));
        }
    }

    pub(super) fn observe_last_in_slot(&mut self, index: u32) {
        self.max_data_index_seen = Some(
            self.max_data_index_seen
                .map_or(index, |prev| prev.max(index)),
        );
        self.last_index_seen = Some(self.last_index_seen.map_or(index, |prev| prev.max(index)));
    }

    pub(super) const fn should_request_highest(
        &self,
        now_tick_ms: u64,
        settle_ms: u64,
        cooldown_ms: u64,
    ) -> bool {
        if self.last_index_seen.is_some() {
            return false;
        }
        let Some(first_seen_tick_ms) = self.first_seen_tick_ms else {
            return false;
        };
        if now_tick_ms < first_seen_tick_ms.saturating_add(settle_ms) {
            return false;
        }
        if let Some(last_request_tick_ms) = self.last_highest_request_tick_ms
            && now_tick_ms < last_request_tick_ms.saturating_add(cooldown_ms)
        {
            return false;
        }
        true
    }

    pub(super) const fn mark_highest_requested(&mut self, tick_ms: u64) {
        self.last_highest_request_tick_ms = Some(tick_ms);
    }

    pub(super) fn advance_contiguous_prefix(&mut self) {
        loop {
            let index = self.contiguous_data_prefix;
            if !self.has_index(index) {
                break;
            }
            let Some(next) = index.checked_add(1) else {
                break;
            };
            self.contiguous_data_prefix = next;
        }
    }

    pub(super) fn has_index(&self, index: u32) -> bool {
        let set_width = DATA_SHREDS_PER_FEC_SET as u32;
        let fec_set_index = index
            .checked_div(set_width)
            .and_then(|value| value.checked_mul(set_width))
            .unwrap_or_default();
        let Some(set) = self.sets.get(&fec_set_index) else {
            return false;
        };
        let position = index.saturating_sub(fec_set_index);
        let Ok(position) = usize::try_from(position) else {
            return false;
        };
        set.is_received(position)
    }

    pub(super) fn received_upper_bound(&self) -> Option<u32> {
        if let Some(last_index) = self.last_index_seen {
            return last_index.checked_add(1);
        }
        self.max_data_index_seen
            .and_then(|index| index.checked_add(1))
    }

    pub(super) fn missing_indexes_ready(
        &self,
        start_index: u32,
        end_index: u32,
        now_tick_ms: u64,
        settle_ms: u64,
        max_missing: usize,
    ) -> Vec<u32> {
        if start_index >= end_index || max_missing == 0 {
            return Vec::new();
        }

        let Some(first_data_tick_ms) = self.first_data_tick_ms else {
            return Vec::new();
        };

        let ticks_since_first_insert = now_tick_ms
            .saturating_sub(first_data_tick_ms)
            .saturating_mul(DEFAULT_TICKS_PER_SECOND)
            / MILLIS_PER_SECOND;
        let defer_threshold_ticks =
            settle_ms.saturating_mul(DEFAULT_TICKS_PER_SECOND) / MILLIS_PER_SECOND;
        let mut missing_indexes = Vec::with_capacity(max_missing);
        let mut prev_index = start_index;
        let mut timed_out_all = true;

        for (&current_index, &reference_tick) in
            self.received_reference_ticks.range(start_index..end_index)
        {
            let threshold = u64::from(reference_tick).saturating_add(defer_threshold_ticks);
            if ticks_since_first_insert < threshold {
                timed_out_all = false;
                break;
            }

            let mut index = prev_index;
            while index < current_index {
                missing_indexes.push(index);
                if missing_indexes.len() >= max_missing {
                    return missing_indexes;
                }
                let Some(next) = index.checked_add(1) else {
                    return missing_indexes;
                };
                index = next;
            }
            prev_index = current_index.saturating_add(1);
        }

        if timed_out_all {
            let mut index = prev_index;
            while index < end_index {
                missing_indexes.push(index);
                if missing_indexes.len() >= max_missing {
                    break;
                }
                let Some(next) = index.checked_add(1) else {
                    break;
                };
                index = next;
            }
        }

        missing_indexes
    }

    pub(super) fn request_window_index_if_needed(
        &mut self,
        index: u32,
        now_tick_ms: u64,
        settle_ms: u64,
        cooldown_ms: u64,
    ) -> bool {
        let set_width = DATA_SHREDS_PER_FEC_SET as u32;
        let fec_set_index = index
            .checked_div(set_width)
            .and_then(|value| value.checked_mul(set_width))
            .unwrap_or_default();
        let position = index.saturating_sub(fec_set_index);
        let Ok(position) = usize::try_from(position) else {
            return false;
        };
        if position >= DATA_SHREDS_PER_FEC_SET {
            return false;
        }

        let first_seen = self.first_seen_tick_ms.unwrap_or(now_tick_ms);
        let set_state = self
            .sets
            .entry(fec_set_index)
            .or_insert_with(|| FecSetMissingState::new(first_seen));
        if position >= usize::from(set_state.expected_data_shreds) {
            return false;
        }
        if set_state.is_received(position) {
            return false;
        }
        if !set_state.can_request(position, now_tick_ms, settle_ms, cooldown_ms) {
            return false;
        }
        set_state.mark_requested(position, now_tick_ms);
        true
    }
}

impl FecSetMissingState {
    pub(super) const fn new(first_seen_tick_ms: u64) -> Self {
        Self {
            first_seen_tick_ms,
            expected_data_shreds: DATA_SHREDS_PER_FEC_SET as u16,
            received_mask: 0,
            last_request_tick_ms: [NEVER_REQUESTED_TICK_MS; DATA_SHREDS_PER_FEC_SET],
        }
    }

    pub(super) fn set_expected_data_shreds(&mut self, expected_data_shreds: u16) {
        self.expected_data_shreds = expected_data_shreds.clamp(
            MIN_EXPECTED_DATA_SHREDS_PER_SET,
            DATA_SHREDS_PER_FEC_SET as u16,
        );
    }

    pub(super) fn mark_received(&mut self, position: u32) {
        if let Ok(position) = usize::try_from(position)
            && position < DATA_SHREDS_PER_FEC_SET
        {
            self.received_mask |= 1_u32 << position;
        }
    }

    pub(super) const fn is_received(&self, position: usize) -> bool {
        if position >= DATA_SHREDS_PER_FEC_SET {
            return false;
        }
        let bit = 1_u32 << position;
        self.received_mask & bit == bit
    }

    pub(super) fn can_request(
        &self,
        position: usize,
        now_tick_ms: u64,
        settle_ms: u64,
        cooldown_ms: u64,
    ) -> bool {
        if now_tick_ms < self.first_seen_tick_ms.saturating_add(settle_ms) {
            return false;
        }
        let Some(last_request) = self.last_request_tick_ms.get(position).copied() else {
            return false;
        };
        if last_request == NEVER_REQUESTED_TICK_MS {
            return true;
        }
        now_tick_ms >= last_request.saturating_add(cooldown_ms)
    }

    pub(super) fn mark_requested(&mut self, position: usize, now_tick_ms: u64) {
        if let Some(last_request) = self.last_request_tick_ms.get_mut(position) {
            *last_request = now_tick_ms;
        }
    }

    pub(super) fn is_complete(&self) -> bool {
        let expected = usize::from(self.expected_data_shreds).clamp(
            usize::from(MIN_EXPECTED_DATA_SHREDS_PER_SET),
            DATA_SHREDS_PER_FEC_SET,
        );
        let needed_mask = if expected == DATA_SHREDS_PER_FEC_SET {
            u32::MAX
        } else {
            let shift = u32::try_from(expected).unwrap_or(u32::MAX);
            let shifted = 1_u32.checked_shl(shift).unwrap_or(0);
            shifted.saturating_sub(1)
        };
        self.received_mask & needed_mask == needed_mask
    }
}

pub(super) fn duration_to_ms_u64(duration: Duration) -> u64 {
    let millis = duration.as_millis();
    if millis > u128::from(u64::MAX) {
        u64::MAX
    } else {
        millis as u64
    }
}
