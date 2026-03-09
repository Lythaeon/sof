use std::collections::{BTreeSet, HashMap};

use super::{CompletedDataSet, PayloadFragmentBatch, SharedPayloadFragment};

#[derive(Debug)]
pub(super) struct SlotStream {
    slot: u64,
    tail_min_shreds_without_anchor: usize,
    fragments: HashMap<u32, DataFragment>,
    dataset_boundaries: BTreeSet<u32>,
    last_index: Option<u32>,
    pub(super) finished: bool,
}

impl SlotStream {
    pub(super) fn new(slot: u64, tail_min_shreds_without_anchor: usize) -> Self {
        Self {
            slot,
            tail_min_shreds_without_anchor,
            fragments: HashMap::new(),
            dataset_boundaries: BTreeSet::new(),
            last_index: None,
            finished: false,
        }
    }

    pub(super) fn insert(
        &mut self,
        index: u32,
        data_complete: bool,
        last_in_slot: bool,
        payload_fragment: SharedPayloadFragment,
    ) {
        let data_complete = data_complete || last_in_slot;

        if data_complete {
            let _ = self.dataset_boundaries.insert(index);
        }
        if last_in_slot {
            self.last_index = Some(self.last_index.map_or(index, |prev| prev.max(index)));
        }

        if let std::collections::hash_map::Entry::Vacant(vacant) = self.fragments.entry(index) {
            let _ = vacant.insert(DataFragment {
                payload_fragment,
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
            let Some(start_index) = self.start_index_for_boundary(complete_index) else {
                continue;
            };
            if !self.has_full_range(start_index, complete_index) {
                continue;
            }
            let mut last_in_slot = false;
            let range_len = complete_index.saturating_sub(start_index).saturating_add(1);
            let mut fragments =
                Vec::with_capacity(usize::try_from(range_len).unwrap_or(usize::MAX));
            for index in start_index..=complete_index {
                if let Some(fragment) = self.fragments.remove(&index) {
                    last_in_slot |= fragment.last_in_slot;
                    fragments.push(fragment.payload_fragment);
                }
            }
            let _ = self.dataset_boundaries.remove(&complete_index);
            completed.push(CompletedDataSet {
                slot: self.slot,
                start_index,
                end_index: complete_index,
                payload_fragments: PayloadFragmentBatch::new(fragments),
                last_in_slot,
            });
        }

        if self
            .last_index
            .is_some_and(|last_index| !self.dataset_boundaries.contains(&last_index))
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
    payload_fragment: SharedPayloadFragment,
    last_in_slot: bool,
}
