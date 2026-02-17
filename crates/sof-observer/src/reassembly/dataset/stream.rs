use std::collections::{BTreeMap, BTreeSet};

use super::CompletedDataSet;

#[derive(Debug)]
pub(super) struct SlotStream {
    slot: u64,
    tail_min_shreds_without_anchor: usize,
    fragments: BTreeMap<u32, DataFragment>,
    dataset_boundaries: BTreeSet<u32>,
    emitted_boundaries: BTreeSet<u32>,
    last_index: Option<u32>,
    pub(super) finished: bool,
}

impl SlotStream {
    pub(super) const fn new(slot: u64, tail_min_shreds_without_anchor: usize) -> Self {
        Self {
            slot,
            tail_min_shreds_without_anchor,
            fragments: BTreeMap::new(),
            dataset_boundaries: BTreeSet::new(),
            emitted_boundaries: BTreeSet::new(),
            last_index: None,
            finished: false,
        }
    }

    pub(super) fn insert(
        &mut self,
        index: u32,
        data_complete: bool,
        last_in_slot: bool,
        serialized_shred: Vec<u8>,
    ) {
        let data_complete = data_complete || last_in_slot;

        if data_complete {
            let _ = self.dataset_boundaries.insert(index);
        }
        if last_in_slot {
            self.last_index = Some(self.last_index.map_or(index, |prev| prev.max(index)));
        }

        if let std::collections::btree_map::Entry::Vacant(vacant) = self.fragments.entry(index) {
            let _ = vacant.insert(DataFragment {
                serialized_shred,
                last_in_slot,
            });
        } else if last_in_slot && let Some(fragment) = self.fragments.get_mut(&index) {
            fragment.last_in_slot = true;
        }
    }

    pub(super) fn drain_contiguous_datasets(&mut self) -> Vec<CompletedDataSet> {
        let mut completed = Vec::new();
        let boundaries: Vec<u32> = self.dataset_boundaries.iter().copied().collect();
        for complete_index in boundaries {
            if self.emitted_boundaries.contains(&complete_index) {
                continue;
            }
            let Some(start_index) = self.start_index_for_boundary(complete_index) else {
                continue;
            };
            if !self.has_full_range(start_index, complete_index) {
                continue;
            }
            let mut last_in_slot = false;
            let range_len = complete_index.saturating_sub(start_index).saturating_add(1);
            let mut serialized_shreds =
                Vec::with_capacity(usize::try_from(range_len).unwrap_or(usize::MAX));
            for index in start_index..=complete_index {
                if let Some(fragment) = self.fragments.remove(&index) {
                    last_in_slot |= fragment.last_in_slot;
                    serialized_shreds.push(fragment.serialized_shred);
                }
            }
            let _ = self.emitted_boundaries.insert(complete_index);
            completed.push(CompletedDataSet {
                slot: self.slot,
                start_index,
                end_index: complete_index,
                serialized_shreds,
                last_in_slot,
            });
        }

        if let Some(last_index) = self.last_index
            && self.emitted_boundaries.contains(&last_index)
            && self.fragments.is_empty()
        {
            self.finished = true;
        }
        completed
    }

    fn start_index_for_boundary(&self, complete_index: u32) -> Option<u32> {
        if let Some(previous_boundary) = self.dataset_boundaries.range(..complete_index).next_back()
        {
            return Some(previous_boundary.saturating_add(1));
        }

        // If we have no earlier known boundary for the slot, emit the longest contiguous tail that
        // ends at `complete_index` instead of forcing a slot-0 anchor.
        let mut start = complete_index;
        while start > 0 {
            let prev = start.saturating_sub(1);
            if !self.fragments.contains_key(&prev) {
                break;
            }
            start = prev;
        }
        if start > 0 {
            let contiguous = complete_index.saturating_sub(start).saturating_add(1);
            if contiguous < self.tail_min_shreds_without_anchor as u32 {
                return None;
            }
        }
        Some(start)
    }

    fn has_full_range(&self, start_index: u32, end_index: u32) -> bool {
        (start_index..=end_index).all(|index| self.fragments.contains_key(&index))
    }
}

#[derive(Debug)]
struct DataFragment {
    serialized_shred: Vec<u8>,
    last_in_slot: bool,
}
