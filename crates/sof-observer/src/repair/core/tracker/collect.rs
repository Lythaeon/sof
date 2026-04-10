use std::cmp::Ordering;

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SlotCollectPriority {
    probe_ready: bool,
    has_received: bool,
    received_upper: u32,
    probe_only: bool,
    has_last: bool,
    gap: u32,
    observed_span: u32,
}

impl SlotCollectPriority {
    fn from_state(slot_state: &SlotMissingState) -> Self {
        let probe_ready = slot_state.is_highest_probe_ready();
        let received_upper = slot_state.received_upper_bound();
        let has_received = received_upper.is_some();
        let probe_only = probe_ready && !has_received;
        let has_last = slot_state.last_index_seen.is_some();
        let received_upper_value = received_upper.unwrap_or(0);
        let gap = received_upper
            .map(|upper| upper.saturating_sub(slot_state.contiguous_data_prefix))
            .unwrap_or(u32::MAX);
        let observed_span = received_upper
            .map(|upper| {
                upper.saturating_sub(
                    slot_state
                        .min_data_index_seen
                        .unwrap_or(slot_state.contiguous_data_prefix),
                )
            })
            .unwrap_or(u32::MAX);

        Self {
            probe_ready,
            has_received,
            received_upper: received_upper_value,
            probe_only,
            has_last,
            gap,
            observed_span,
        }
    }

    fn cmp_with_slot(self, slot: u64, other: Self, other_slot: u64) -> Ordering {
        other
            .probe_ready
            .cmp(&self.probe_ready)
            .then_with(|| other.has_received.cmp(&self.has_received))
            .then_with(|| other.received_upper.cmp(&self.received_upper))
            .then_with(|| {
                if self.probe_only && other.probe_only {
                    slot.cmp(&other_slot)
                } else {
                    other_slot.cmp(&slot)
                }
            })
            .then_with(|| other.has_last.cmp(&self.has_last))
            .then_with(|| self.gap.cmp(&other.gap))
            .then_with(|| self.observed_span.cmp(&other.observed_span))
    }
}

impl MissingShredTracker {
    pub fn collect_requests(
        &mut self,
        now: Instant,
        max_requests: usize,
        max_highest_window_requests: usize,
        max_forward_probe_requests: usize,
    ) -> Vec<MissingShredRequest> {
        let tick_ms = self.tick_ms(now);
        let settle_ms = duration_to_ms_u64(self.settle_delay);
        let cooldown_ms = duration_to_ms_u64(self.request_cooldown);
        self.seed_forward_highest_probes(tick_ms);

        let mut requests = Vec::with_capacity(max_requests);
        let highest_request_budget = max_requests.min(max_highest_window_requests);
        let mut forward_probe_requests_sent = 0_usize;
        let mut slot_request_counts: HashMap<u64, usize> = HashMap::with_capacity(self.slots.len());
        let slot_keys = self.sorted_slot_keys();
        if highest_request_budget > 0 {
            for slot in &slot_keys {
                if requests.len() >= max_requests || requests.len() >= highest_request_budget {
                    break;
                }
                let slot = *slot;
                let remaining_global = max_requests.saturating_sub(requests.len());
                let used_budget = slot_request_counts.get(&slot).copied().unwrap_or(0);
                if used_budget >= self.per_slot_request_cap || remaining_global == 0 {
                    continue;
                }
                let slot_budget =
                    remaining_global.min(self.per_slot_request_cap.saturating_sub(used_budget));
                let Some(slot_state) = self.slots.get_mut(&slot) else {
                    continue;
                };
                if let Some(frontier) = slot_state
                    .last_index_seen
                    .or(slot_state.max_data_index_seen)
                {
                    Self::seed_prefix_sets_to_frontier(
                        slot_state,
                        frontier,
                        tick_ms,
                        self.auto_backfill_sets,
                    );
                }
                if slot_budget == 0 {
                    continue;
                }

                let highest_index = match slot_state.received_upper_bound() {
                    Some(received_upper) if slot_state.contiguous_data_prefix >= received_upper => {
                        Some(received_upper)
                    }
                    None if slot_state.seed_highest_probe => Some(0),
                    _ => None,
                };
                let Some(highest_index) = highest_index else {
                    continue;
                };
                if !slot_state.should_request_highest(tick_ms, settle_ms, cooldown_ms) {
                    continue;
                }
                let is_probe_only_highest =
                    slot_state.received_upper_bound().is_none() && slot_state.seed_highest_probe;
                if is_probe_only_highest
                    && forward_probe_requests_sent >= max_forward_probe_requests
                {
                    continue;
                }
                slot_state.mark_highest_requested(tick_ms);
                requests.push(MissingShredRequest {
                    slot,
                    index: highest_index,
                    kind: MissingShredRequestKind::HighestWindowIndex,
                });
                if is_probe_only_highest {
                    forward_probe_requests_sent = forward_probe_requests_sent.saturating_add(1);
                }
                *slot_request_counts.entry(slot).or_default() = used_budget.saturating_add(1);
            }
        }

        for slot in slot_keys {
            if requests.len() >= max_requests {
                break;
            }
            let remaining_global = max_requests.saturating_sub(requests.len());
            let used_budget = slot_request_counts.get(&slot).copied().unwrap_or(0);
            if used_budget >= self.per_slot_request_cap || remaining_global == 0 {
                continue;
            }
            let slot_budget =
                remaining_global.min(self.per_slot_request_cap.saturating_sub(used_budget));
            let allow_window_requests =
                !self.has_latest_slot || self.latest_slot.saturating_sub(slot) >= self.min_slot_lag;
            let Some(slot_state) = self.slots.get_mut(&slot) else {
                continue;
            };
            if let Some(frontier) = slot_state
                .last_index_seen
                .or(slot_state.max_data_index_seen)
            {
                Self::seed_prefix_sets_to_frontier(
                    slot_state,
                    frontier,
                    tick_ms,
                    self.auto_backfill_sets,
                );
            }
            let Some(received_upper) = slot_state.received_upper_bound() else {
                continue;
            };
            if slot_state.contiguous_data_prefix >= received_upper || !allow_window_requests {
                continue;
            }

            let mut slot_requests = 0_usize;
            let remaining = max_requests
                .saturating_sub(requests.len())
                .min(slot_budget.saturating_sub(slot_requests));
            if remaining == 0 {
                continue;
            }
            let observed_start = slot_state
                .min_data_index_seen
                .unwrap_or(slot_state.contiguous_data_prefix);
            let prioritized_start = slot_state.contiguous_data_prefix.max(observed_start);
            let prioritized_missing = slot_state.missing_indexes_ready(
                prioritized_start,
                received_upper,
                tick_ms,
                settle_ms,
                remaining,
            );
            for index in prioritized_missing {
                if slot_state.request_window_index_if_needed(index, tick_ms, settle_ms, cooldown_ms)
                {
                    requests.push(MissingShredRequest {
                        slot,
                        index,
                        kind: MissingShredRequestKind::WindowIndex,
                    });
                    slot_requests = slot_requests.saturating_add(1);
                    if requests.len() >= max_requests || slot_requests >= slot_budget {
                        break;
                    }
                }
            }

            if requests.len() >= max_requests || slot_requests >= slot_budget {
                if slot_requests > 0 {
                    *slot_request_counts.entry(slot).or_default() =
                        used_budget.saturating_add(slot_requests);
                }
                continue;
            }
            if slot_state.contiguous_data_prefix < observed_start {
                let prefix_backfill_cap = slot_budget
                    .saturating_sub(slot_requests)
                    .min(4)
                    .min(max_requests.saturating_sub(requests.len()));
                if prefix_backfill_cap > 0 {
                    let prefix_missing = slot_state.missing_indexes_ready(
                        slot_state.contiguous_data_prefix,
                        observed_start,
                        tick_ms,
                        settle_ms,
                        prefix_backfill_cap,
                    );
                    for index in prefix_missing {
                        if slot_state.request_window_index_if_needed(
                            index,
                            tick_ms,
                            settle_ms,
                            cooldown_ms,
                        ) {
                            requests.push(MissingShredRequest {
                                slot,
                                index,
                                kind: MissingShredRequestKind::WindowIndex,
                            });
                            slot_requests = slot_requests.saturating_add(1);
                            if requests.len() >= max_requests || slot_requests >= slot_budget {
                                break;
                            }
                        }
                    }
                }
            }
            if slot_requests > 0 {
                *slot_request_counts.entry(slot).or_default() =
                    used_budget.saturating_add(slot_requests);
            }
        }
        self.cleanup_complete_sets();
        requests
    }

    fn seed_prefix_sets_to_frontier(
        slot_state: &mut SlotMissingState,
        frontier_index: u32,
        tick_ms: u64,
        max_sets_per_tick: usize,
    ) {
        if max_sets_per_tick == 0 {
            return;
        }
        let set_width = DATA_SHREDS_PER_FEC_SET as u32;
        let frontier_fec_set_index = frontier_index
            .checked_div(set_width)
            .and_then(|value| value.checked_mul(set_width))
            .unwrap_or(0);
        let mut inserted = 0_usize;
        let mut fec_set_index = 0_u32;
        while fec_set_index <= frontier_fec_set_index {
            if let std::collections::hash_map::Entry::Vacant(vacant) =
                slot_state.sets.entry(fec_set_index)
            {
                let _ = vacant.insert(FecSetMissingState::new(tick_ms));
                inserted = inserted.saturating_add(1);
                if inserted >= max_sets_per_tick {
                    break;
                }
            }
            let Some(next) = fec_set_index.checked_add(set_width) else {
                break;
            };
            fec_set_index = next;
        }
    }

    pub(crate) fn sorted_slot_keys(&self) -> Vec<u64> {
        let mut priorities: Vec<_> = self
            .slots
            .iter()
            .map(|(&slot, slot_state)| (slot, SlotCollectPriority::from_state(slot_state)))
            .collect();
        priorities.sort_unstable_by(|(slot, priority), (other_slot, other_priority)| {
            priority.cmp_with_slot(*slot, *other_priority, *other_slot)
        });
        priorities.into_iter().map(|(slot, _)| slot).collect()
    }

    #[cfg(test)]
    pub(crate) fn sorted_slot_keys_baseline(&self) -> Vec<u64> {
        let mut slot_keys: Vec<u64> = self.slots.keys().copied().collect();
        slot_keys.sort_unstable_by(|a, b| {
            let Some(a_state) = self.slots.get(a) else {
                return Ordering::Greater;
            };
            let Some(b_state) = self.slots.get(b) else {
                return Ordering::Less;
            };
            SlotCollectPriority::from_state(a_state).cmp_with_slot(
                *a,
                SlotCollectPriority::from_state(b_state),
                *b,
            )
        });
        slot_keys
    }
}
