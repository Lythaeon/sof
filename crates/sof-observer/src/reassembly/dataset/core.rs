use std::collections::HashMap;

use super::stream::SlotStream;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CompletedDataSet {
    pub slot: u64,
    pub start_index: u32,
    pub end_index: u32,
    pub serialized_shreds: Vec<Vec<u8>>,
    pub last_in_slot: bool,
}

#[derive(Debug, Default)]
pub struct DataSetReassembler {
    slots: HashMap<u64, SlotStream>,
    max_tracked_slots: usize,
    tail_min_shreds_without_anchor: usize,
}

impl DataSetReassembler {
    #[must_use]
    pub fn new(max_tracked_slots: usize) -> Self {
        Self {
            slots: HashMap::new(),
            max_tracked_slots,
            tail_min_shreds_without_anchor: 2,
        }
    }

    #[must_use]
    pub fn with_tail_min_shreds_without_anchor(mut self, min_shreds: usize) -> Self {
        self.tail_min_shreds_without_anchor = min_shreds.max(1);
        self
    }

    pub fn ingest_data_shred_meta(
        &mut self,
        slot: u64,
        index: u32,
        data_complete: bool,
        last_in_slot: bool,
        serialized_shred: Vec<u8>,
    ) -> Vec<CompletedDataSet> {
        self.evict_if_needed(slot);

        let stream = self
            .slots
            .entry(slot)
            .or_insert_with(|| SlotStream::new(slot, self.tail_min_shreds_without_anchor));
        stream.insert(index, data_complete, last_in_slot, serialized_shred);

        let completed = stream.drain_contiguous_datasets();
        if stream.finished {
            let _ = self.slots.remove(&slot);
        }
        completed
    }

    fn evict_if_needed(&mut self, incoming_slot: u64) {
        if self.slots.len() < self.max_tracked_slots || self.slots.contains_key(&incoming_slot) {
            return;
        }
        if let Some(oldest_slot) = self.slots.keys().min().copied() {
            let _ = self.slots.remove(&oldest_slot);
        }
    }

    #[must_use]
    pub fn tracked_slots(&self) -> usize {
        self.slots.len()
    }
}
