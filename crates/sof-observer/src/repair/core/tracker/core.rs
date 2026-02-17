use std::{
    collections::{BTreeMap, HashMap},
    time::{Duration, Instant},
};

const DATA_SHREDS_PER_FEC_SET: usize = 32;
const DEFAULT_TICKS_PER_SECOND: u64 = 160;
const MIN_PER_SLOT_REQUEST_CAP: usize = 1;
const MAX_TIP_PROBE_AHEAD_SLOTS: usize = 256;

#[path = "collect.rs"]
mod collect;
#[path = "slot_state.rs"]
mod slot_state;
#[path = "slots.rs"]
mod slots;
use slot_state::duration_to_ms_u64;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct MissingShredRequest {
    pub slot: u64,
    pub index: u32,
    pub kind: MissingShredRequestKind,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum MissingShredRequestKind {
    WindowIndex,
    HighestWindowIndex,
}

#[derive(Debug)]
pub struct MissingShredTracker {
    slots: HashMap<u64, SlotMissingState>,
    latest_slot: u64,
    has_latest_slot: bool,
    slot_window: u64,
    min_slot_lag: u64,
    settle_delay: Duration,
    request_cooldown: Duration,
    auto_backfill_sets: usize,
    per_slot_request_cap: usize,
    tip_probe_ahead_slots: usize,
    epoch: Instant,
}

#[derive(Debug, Default)]
struct SlotMissingState {
    sets: HashMap<u32, FecSetMissingState>,
    backfill_seeded: bool,
    seed_highest_probe: bool,
    first_seen_tick_ms: Option<u64>,
    first_data_tick_ms: Option<u64>,
    min_data_index_seen: Option<u32>,
    last_index_seen: Option<u32>,
    max_data_index_seen: Option<u32>,
    received_reference_ticks: BTreeMap<u32, u8>,
    last_highest_request_tick_ms: Option<u64>,
    contiguous_data_prefix: u32,
}

#[derive(Debug)]
struct FecSetMissingState {
    first_seen_tick_ms: u64,
    expected_data_shreds: u16,
    received_mask: u32,
    last_request_tick_ms: [u64; DATA_SHREDS_PER_FEC_SET],
}
impl MissingShredTracker {
    #[must_use]
    pub fn new(
        slot_window: u64,
        min_slot_lag: u64,
        settle_delay: Duration,
        request_cooldown: Duration,
        auto_backfill_sets: usize,
        per_slot_request_cap: usize,
        tip_probe_ahead_slots: usize,
    ) -> Self {
        Self {
            slots: HashMap::new(),
            latest_slot: 0,
            has_latest_slot: false,
            slot_window,
            min_slot_lag,
            settle_delay,
            request_cooldown,
            auto_backfill_sets,
            per_slot_request_cap: per_slot_request_cap.max(MIN_PER_SLOT_REQUEST_CAP),
            tip_probe_ahead_slots: tip_probe_ahead_slots.min(MAX_TIP_PROBE_AHEAD_SLOTS),
            epoch: Instant::now(),
        }
    }

    pub fn on_data_shred(
        &mut self,
        slot: u64,
        index: u32,
        fec_set_index: u32,
        last_in_slot: bool,
        reference_tick: u8,
        now: Instant,
    ) {
        let tick_ms = self.tick_ms(now);
        self.touch_slot(slot, tick_ms);
        let Some(position) = index.checked_sub(fec_set_index) else {
            return;
        };
        let Ok(position_usize) = usize::try_from(position) else {
            return;
        };
        if position_usize >= DATA_SHREDS_PER_FEC_SET {
            return;
        }
        let mut seed_next_slot_probe = false;
        {
            let slot_state = self.slots.entry(slot).or_default();
            slot_state.observe_data_index(index, reference_tick, tick_ms);
            if last_in_slot {
                slot_state.observe_last_in_slot(index);
                seed_next_slot_probe = true;
            }
            let set = slot_state
                .sets
                .entry(fec_set_index)
                .or_insert_with(|| FecSetMissingState::new(tick_ms));
            set.mark_received(position);
            slot_state.advance_contiguous_prefix();
        }
        self.seed_backfill_sets(slot, fec_set_index, tick_ms);
        if seed_next_slot_probe {
            self.seed_next_slot_probe(slot, tick_ms);
        }
    }

    pub fn on_recovered_data_shred(
        &mut self,
        slot: u64,
        index: u32,
        fec_set_index: u32,
        last_in_slot: bool,
        reference_tick: u8,
        now: Instant,
    ) {
        self.on_data_shred(
            slot,
            index,
            fec_set_index,
            last_in_slot,
            reference_tick,
            now,
        );
    }

    pub fn on_code_shred(
        &mut self,
        slot: u64,
        fec_set_index: u32,
        num_data_shreds: u16,
        now: Instant,
    ) {
        let tick_ms = self.tick_ms(now);
        self.touch_slot(slot, tick_ms);
        let slot_state = self.slots.entry(slot).or_default();
        let set = slot_state
            .sets
            .entry(fec_set_index)
            .or_insert_with(|| FecSetMissingState::new(tick_ms));
        set.set_expected_data_shreds(num_data_shreds);
        self.seed_backfill_sets(slot, fec_set_index, tick_ms);
    }

    pub fn seed_highest_probe_slot(&mut self, slot: u64, now: Instant) {
        let tick_ms = self.tick_ms(now);
        self.touch_slot(slot, tick_ms);
        if let Some(state) = self.slots.get_mut(&slot)
            && state.received_upper_bound().is_none()
        {
            state.seed_highest_probe = true;
        }
    }

    pub const fn set_min_slot_lag(&mut self, min_slot_lag: u64) {
        self.min_slot_lag = min_slot_lag;
    }

    pub fn set_per_slot_request_cap(&mut self, per_slot_request_cap: usize) {
        self.per_slot_request_cap = per_slot_request_cap.max(MIN_PER_SLOT_REQUEST_CAP);
    }
}
