use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use super::{
    CompletedDataSet, InlineContiguousDataSet, PayloadFragmentBatch, SharedPayloadFragment,
};

#[derive(Debug)]
pub(super) struct SlotStream {
    slot: u64,
    tail_min_shreds_without_anchor: usize,
    fragments: HashMap<u32, DataFragment>,
    dataset_boundaries: BTreeSet<u32>,
    next_dataset_start: u32,
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
            next_dataset_start: 0,
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
        observed_at: Instant,
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
                observed_at,
            });
        } else if last_in_slot && let Some(fragment) = self.fragments.get_mut(&index) {
            fragment.last_in_slot = true;
            fragment.observed_at = fragment.observed_at.max(observed_at);
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
            let mut first_shred_observed_at: Option<Instant> = None;
            let mut last_shred_observed_at: Option<Instant> = None;
            let range_len = complete_index.saturating_sub(start_index).saturating_add(1);
            let mut fragments =
                Vec::with_capacity(usize::try_from(range_len).unwrap_or(usize::MAX));
            for index in start_index..=complete_index {
                if let Some(fragment) = self.fragments.remove(&index) {
                    last_in_slot |= fragment.last_in_slot;
                    first_shred_observed_at = Some(
                        first_shred_observed_at.map_or(fragment.observed_at, |current| {
                            current.min(fragment.observed_at)
                        }),
                    );
                    last_shred_observed_at = Some(
                        last_shred_observed_at.map_or(fragment.observed_at, |current| {
                            current.max(fragment.observed_at)
                        }),
                    );
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
                first_shred_observed_at: first_shred_observed_at.unwrap_or_else(Instant::now),
                last_shred_observed_at: last_shred_observed_at.unwrap_or_else(Instant::now),
            });
            self.next_dataset_start = complete_index.saturating_add(1);
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

    pub(super) fn inline_contiguous_dataset(&self) -> Option<InlineContiguousDataSet> {
        self.inline_contiguous_datasets().into_iter().next()
    }

    pub(super) fn inline_contiguous_datasets(&self) -> Vec<InlineContiguousDataSet> {
        let mut start_indices = Vec::with_capacity(self.dataset_boundaries.len().saturating_add(2));
        if self.fragments.contains_key(&0) {
            start_indices.push(0);
        }
        if self.next_dataset_start != 0 && self.fragments.contains_key(&self.next_dataset_start) {
            start_indices.push(self.next_dataset_start);
        }
        for boundary in self.dataset_boundaries.iter().copied() {
            let Some(start_index) = boundary.checked_add(1) else {
                continue;
            };
            if self.fragments.contains_key(&start_index) {
                start_indices.push(start_index);
            }
        }
        start_indices.sort_unstable();
        start_indices.dedup();
        start_indices
            .into_iter()
            .filter_map(|start_index| self.inline_contiguous_dataset_from(start_index))
            .collect()
    }

    fn inline_contiguous_dataset_from(&self, start_index: u32) -> Option<InlineContiguousDataSet> {
        let mut payload_fragments = Vec::new();
        let mut fragment_observed_ats = Vec::new();
        let mut cursor = start_index;
        loop {
            let fragment = self.fragments.get(&cursor)?;
            payload_fragments.push(fragment.payload_fragment.clone());
            fragment_observed_ats.push(fragment.observed_at);
            cursor = cursor.saturating_add(1);
            if !self.fragments.contains_key(&cursor) {
                break;
            }
        }
        let end_index = start_index
            .saturating_add(u32::try_from(payload_fragments.len().saturating_sub(1)).unwrap_or(0));
        Some(InlineContiguousDataSet {
            slot: self.slot,
            start_index,
            end_index,
            payload_fragments: PayloadFragmentBatch::new(payload_fragments),
            fragment_observed_ats,
        })
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
    observed_at: Instant,
}
