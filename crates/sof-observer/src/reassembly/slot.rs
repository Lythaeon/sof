use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CompletedSlot {
    pub slot: u64,
    pub last_index: u32,
    pub payload: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct SlotReassembler {
    slots: HashMap<u64, SlotAccumulator>,
    max_tracked_slots: usize,
}

impl SlotReassembler {
    #[must_use]
    pub fn new(max_tracked_slots: usize) -> Self {
        Self {
            slots: HashMap::new(),
            max_tracked_slots,
        }
    }

    pub fn ingest_data_shred_meta(
        &mut self,
        slot: u64,
        index: u32,
        data_complete: bool,
        last_in_slot: bool,
        payload: Vec<u8>,
    ) -> Option<CompletedSlot> {
        self.evict_if_needed(slot);

        let entry = self
            .slots
            .entry(slot)
            .or_insert_with(|| SlotAccumulator::new(slot));
        entry.insert(index, data_complete, last_in_slot, payload);

        if let Some(completed) = entry.try_complete() {
            self.slots.remove(&slot);
            return Some(completed);
        }

        None
    }

    fn evict_if_needed(&mut self, incoming_slot: u64) {
        if self.slots.len() < self.max_tracked_slots {
            return;
        }
        if self.slots.contains_key(&incoming_slot) {
            return;
        }

        if let Some(oldest_slot) = self.slots.keys().min().copied() {
            let _ = self.slots.remove(&oldest_slot);
        }
    }
}

#[derive(Debug)]
struct SlotAccumulator {
    slot: u64,
    data_by_index: BTreeMap<u32, DataFragment>,
    last_index: Option<u32>,
}

impl SlotAccumulator {
    const fn new(slot: u64) -> Self {
        Self {
            slot,
            data_by_index: BTreeMap::new(),
            last_index: None,
        }
    }

    fn insert(&mut self, index: u32, data_complete: bool, last_in_slot: bool, payload: Vec<u8>) {
        if last_in_slot {
            self.last_index = Some(index);
        }

        self.data_by_index
            .entry(index)
            .or_insert_with(|| DataFragment {
                payload,
                data_complete,
            });
    }

    fn try_complete(&self) -> Option<CompletedSlot> {
        let last_index = self.last_index?;

        let mut payload = Vec::new();
        let mut index = 0_u32;
        while index <= last_index {
            let fragment = self.data_by_index.get(&index)?;
            payload.extend_from_slice(&fragment.payload);
            index = index.checked_add(1)?;
        }

        let last = self.data_by_index.get(&last_index)?;
        if !last.data_complete {
            return None;
        }

        Some(CompletedSlot {
            slot: self.slot,
            last_index,
            payload,
        })
    }
}

#[derive(Debug)]
struct DataFragment {
    payload: Vec<u8>,
    data_complete: bool,
}

#[cfg(test)]
mod tests {
    use super::SlotReassembler;

    #[test]
    fn completes_slot_when_contiguous_and_last_arrives() {
        let mut reassembler = SlotReassembler::new(128);

        let _ = reassembler.ingest_data_shred_meta(1, 0, false, false, b"aa".to_vec());
        let _ = reassembler.ingest_data_shred_meta(1, 1, false, false, b"bb".to_vec());
        let completed = reassembler.ingest_data_shred_meta(1, 2, true, true, b"cc".to_vec());

        let completed = completed.expect("slot should be complete");
        assert_eq!(completed.slot, 1);
        assert_eq!(completed.last_index, 2);
        assert_eq!(completed.payload, b"aabbcc");
    }

    #[test]
    fn waits_for_missing_middle_shred() {
        let mut reassembler = SlotReassembler::new(128);

        let _ = reassembler.ingest_data_shred_meta(9, 0, false, false, b"aa".to_vec());
        let pending = reassembler.ingest_data_shred_meta(9, 2, true, true, b"cc".to_vec());
        assert!(pending.is_none());

        let completed = reassembler.ingest_data_shred_meta(9, 1, false, false, b"bb".to_vec());
        let completed = completed.expect("slot should become complete once gap is filled");
        assert_eq!(completed.payload, b"aabbcc");
    }
}
