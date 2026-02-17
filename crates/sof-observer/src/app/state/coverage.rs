use super::*;

#[derive(Debug, Default, Clone, Copy)]
struct SlotCounters {
    data_shreds: u64,
    code_shreds: u64,
    recovered_data_shreds: u64,
    datasets_completed: u64,
    txs: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CoverageSnapshot {
    pub slots_tracked: u64,
    pub slots_with_tx: u64,
    pub tx_total: u64,
    pub dataset_total: u64,
    pub data_shreds: u64,
    pub code_shreds: u64,
    pub recovered_data_shreds: u64,
}

#[derive(Debug)]
pub struct SlotCoverageWindow {
    slots: HashMap<u64, SlotCounters>,
    latest_slot: u64,
    has_latest: bool,
    window_slots: u64,
}

impl SlotCoverageWindow {
    pub fn new(window_slots: u64) -> Self {
        Self {
            slots: HashMap::new(),
            latest_slot: 0,
            has_latest: false,
            window_slots,
        }
    }

    pub fn on_data_shred(&mut self, slot: u64) {
        let counters = self.touch(slot);
        counters.data_shreds = counters.data_shreds.saturating_add(1);
    }

    pub fn on_code_shred(&mut self, slot: u64) {
        let counters = self.touch(slot);
        counters.code_shreds = counters.code_shreds.saturating_add(1);
    }

    pub fn on_recovered_data_shred(&mut self, slot: u64) {
        let counters = self.touch(slot);
        counters.recovered_data_shreds = counters.recovered_data_shreds.saturating_add(1);
    }

    pub fn on_dataset_completed(&mut self, slot: u64) {
        let counters = self.touch(slot);
        counters.datasets_completed = counters.datasets_completed.saturating_add(1);
    }

    pub fn on_tx(&mut self, slot: u64) {
        let counters = self.touch(slot);
        counters.txs = counters.txs.saturating_add(1);
    }

    pub fn snapshot(&self) -> CoverageSnapshot {
        let mut snapshot = CoverageSnapshot {
            slots_tracked: self.slots.len() as u64,
            ..CoverageSnapshot::default()
        };
        for counters in self.slots.values() {
            snapshot.data_shreds = snapshot.data_shreds.saturating_add(counters.data_shreds);
            snapshot.code_shreds = snapshot.code_shreds.saturating_add(counters.code_shreds);
            snapshot.recovered_data_shreds = snapshot
                .recovered_data_shreds
                .saturating_add(counters.recovered_data_shreds);
            snapshot.dataset_total = snapshot
                .dataset_total
                .saturating_add(counters.datasets_completed);
            snapshot.tx_total = snapshot.tx_total.saturating_add(counters.txs);
            if counters.txs > 0 {
                snapshot.slots_with_tx = snapshot.slots_with_tx.saturating_add(1);
            }
        }
        snapshot
    }

    fn touch(&mut self, slot: u64) -> &mut SlotCounters {
        if !self.has_latest || slot > self.latest_slot {
            self.latest_slot = slot;
            self.has_latest = true;
            self.evict_old();
        }
        self.slots.entry(slot).or_default()
    }

    fn evict_old(&mut self) {
        if !self.has_latest {
            return;
        }
        let floor = self.latest_slot.saturating_sub(self.window_slots);
        self.slots.retain(|slot, _| *slot >= floor);
    }
}
