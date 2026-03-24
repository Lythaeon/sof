use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use super::dataset::{InlineContiguousDataSet, PayloadFragmentBatch, SharedPayloadFragment};

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct InlineDataObservation {
    pub fec_set_became_ready: bool,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct InlineCodeObservation {
    pub fec_set_became_ready: bool,
}

#[derive(Debug, Default)]
pub struct InlineContiguousDataSetScratch {
    pub slot: u64,
    pub start_index: u32,
    pub end_index: u32,
    pub payload_fragments: Vec<SharedPayloadFragment>,
    pub fragment_observed_ats: Vec<Instant>,
    pub total_payload_len: usize,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct InlineContiguousDataSetMeta {
    pub slot: u64,
    pub start_index: u32,
    pub end_index: u32,
    pub fragment_count: usize,
    pub total_payload_len: usize,
}

pub trait InlineContiguousDataSetSink {
    fn existing_fragments(&self) -> usize;
    fn payload_len(&self) -> usize;
    fn reset_inline_contiguous_dataset(&mut self);
    fn reserve_inline_contiguous_dataset(
        &mut self,
        additional_fragments: usize,
        additional_bytes: usize,
    );
    fn append_inline_contiguous_fragment(
        &mut self,
        fragment: &SharedPayloadFragment,
        observed_at: Instant,
    );
}

#[derive(Debug, Default)]
pub struct InlineDataReassembler {
    slots: HashMap<u64, InlineSlotStream>,
    max_tracked_slots: usize,
    retained_slot_lag: u64,
    last_pruned_floor: u64,
}

impl InlineDataReassembler {
    #[must_use]
    pub fn new(max_tracked_slots: usize) -> Self {
        Self {
            slots: HashMap::new(),
            max_tracked_slots,
            retained_slot_lag: u64::try_from(max_tracked_slots / 4)
                .unwrap_or(u64::MAX)
                .clamp(64, 256),
            last_pruned_floor: 0,
        }
    }

    #[must_use]
    pub fn with_retained_slot_lag(mut self, retained_slot_lag: u64) -> Self {
        self.retained_slot_lag = retained_slot_lag.max(1);
        self
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "inline shred observations need explicit shred metadata on the hot path"
    )]
    pub fn observe_data_shred_meta_at(
        &mut self,
        slot: u64,
        index: u32,
        fec_set_index: u32,
        data_complete: bool,
        last_in_slot: bool,
        payload_fragment: SharedPayloadFragment,
        observed_at: Instant,
    ) -> InlineDataObservation {
        self.purge_older_than(slot.saturating_sub(self.retained_slot_lag));
        self.evict_if_needed(slot);
        let stream = self
            .slots
            .entry(slot)
            .or_insert_with(|| InlineSlotStream::new(slot));
        stream.observe_data_shred(
            index,
            fec_set_index,
            data_complete,
            last_in_slot,
            payload_fragment,
            observed_at,
        )
    }

    pub fn observe_code_shred(
        &mut self,
        slot: u64,
        fec_set_index: u32,
        num_data_shreds: u16,
    ) -> InlineCodeObservation {
        self.purge_older_than(slot.saturating_sub(self.retained_slot_lag));
        self.evict_if_needed(slot);
        let stream = self
            .slots
            .entry(slot)
            .or_insert_with(|| InlineSlotStream::new(slot));
        stream.observe_code_shred(fec_set_index, num_data_shreds)
    }

    #[must_use]
    pub fn inline_contiguous_datasets(&self, slot: u64) -> Vec<InlineContiguousDataSet> {
        self.slots
            .get(&slot)
            .map_or_else(Vec::new, InlineSlotStream::inline_contiguous_datasets)
    }

    pub fn fill_inline_contiguous_datasets(
        &self,
        slot: u64,
        out: &mut Vec<InlineContiguousDataSetScratch>,
    ) {
        if let Some(stream) = self.slots.get(&slot) {
            stream.fill_inline_contiguous_datasets(out);
        } else {
            out.clear();
        }
    }

    pub fn fill_inline_contiguous_dataset_starts(&self, slot: u64, out: &mut Vec<u32>) {
        if let Some(stream) = self.slots.get(&slot) {
            stream.fill_inline_contiguous_dataset_starts(out);
        } else {
            out.clear();
        }
    }

    pub fn sync_inline_contiguous_dataset<S: InlineContiguousDataSetSink>(
        &self,
        slot: u64,
        start_index: u32,
        sink: &mut S,
    ) -> Option<InlineContiguousDataSetMeta> {
        self.slots
            .get(&slot)?
            .sync_inline_contiguous_dataset(start_index, sink)
    }

    pub fn retire_range(&mut self, slot: u64, start_index: u32, end_index: u32) {
        let mut finished = false;
        if let Some(stream) = self.slots.get_mut(&slot) {
            stream.retire_range(start_index, end_index);
            finished = stream.finished;
        }
        if finished {
            let _ = self.slots.remove(&slot);
        }
    }

    pub fn purge_older_than(&mut self, slot_floor: u64) {
        if slot_floor <= self.last_pruned_floor {
            return;
        }
        self.slots.retain(|slot, _| *slot >= slot_floor);
        self.last_pruned_floor = slot_floor;
    }

    fn evict_if_needed(&mut self, incoming_slot: u64) {
        if self.slots.len() < self.max_tracked_slots || self.slots.contains_key(&incoming_slot) {
            return;
        }
        if let Some(oldest_slot) = self.slots.keys().min().copied() {
            let _ = self.slots.remove(&oldest_slot);
        }
    }
}

#[derive(Debug)]
struct InlineSlotStream {
    slot: u64,
    fragments: HashMap<u32, InlineDataFragment>,
    completed_boundaries: BTreeSet<u32>,
    fec_sets: HashMap<u32, InlineFecSetState>,
    last_index: Option<u32>,
    finished: bool,
}

impl InlineSlotStream {
    fn new(slot: u64) -> Self {
        Self {
            slot,
            fragments: HashMap::new(),
            completed_boundaries: BTreeSet::new(),
            fec_sets: HashMap::new(),
            last_index: None,
            finished: false,
        }
    }

    fn observe_data_shred(
        &mut self,
        index: u32,
        fec_set_index: u32,
        data_complete: bool,
        last_in_slot: bool,
        payload_fragment: SharedPayloadFragment,
        observed_at: Instant,
    ) -> InlineDataObservation {
        let boundary = data_complete || last_in_slot;
        if boundary {
            let _ = self.completed_boundaries.insert(index);
        }
        if last_in_slot {
            self.last_index = Some(self.last_index.map_or(index, |prev| prev.max(index)));
        }
        if let std::collections::hash_map::Entry::Vacant(vacant) = self.fragments.entry(index) {
            let _ = vacant.insert(InlineDataFragment {
                payload_fragment,
                observed_at,
            });
        }
        let fec_state = self
            .fec_sets
            .entry(fec_set_index)
            .or_insert_with(|| InlineFecSetState::new(fec_set_index));
        let fec_set_became_ready = fec_state.observe_data_shred(index, boundary);
        InlineDataObservation {
            fec_set_became_ready,
        }
    }

    fn observe_code_shred(
        &mut self,
        fec_set_index: u32,
        num_data_shreds: u16,
    ) -> InlineCodeObservation {
        let fec_state = self
            .fec_sets
            .entry(fec_set_index)
            .or_insert_with(|| InlineFecSetState::new(fec_set_index));
        InlineCodeObservation {
            fec_set_became_ready: fec_state.observe_code_shred(num_data_shreds),
        }
    }

    fn inline_contiguous_datasets(&self) -> Vec<InlineContiguousDataSet> {
        let mut start_indices =
            Vec::with_capacity(self.completed_boundaries.len().saturating_add(1));
        if self.fragments.contains_key(&0) {
            start_indices.push(0);
        }
        for boundary in self.completed_boundaries.iter().copied() {
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

    fn fill_inline_contiguous_dataset_starts(&self, out: &mut Vec<u32>) {
        out.clear();
        if self.fragments.contains_key(&0) {
            out.push(0);
        }
        for boundary in self.completed_boundaries.iter().copied() {
            let Some(start_index) = boundary.checked_add(1) else {
                continue;
            };
            if self.fragments.contains_key(&start_index) {
                out.push(start_index);
            }
        }
    }

    fn fill_inline_contiguous_datasets(&self, out: &mut Vec<InlineContiguousDataSetScratch>) {
        let mut candidate_count = 0_usize;
        if self.fragments.contains_key(&0) {
            self.fill_inline_contiguous_dataset_from(0, out, candidate_count);
            candidate_count = candidate_count.saturating_add(1);
        }
        for boundary in self.completed_boundaries.iter().copied() {
            let Some(start_index) = boundary.checked_add(1) else {
                continue;
            };
            if !self.fragments.contains_key(&start_index) {
                continue;
            }
            self.fill_inline_contiguous_dataset_from(start_index, out, candidate_count);
            candidate_count = candidate_count.saturating_add(1);
        }
        out.truncate(candidate_count);
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

    fn fill_inline_contiguous_dataset_from(
        &self,
        start_index: u32,
        out: &mut Vec<InlineContiguousDataSetScratch>,
        out_index: usize,
    ) {
        if out.len() <= out_index {
            out.push(InlineContiguousDataSetScratch::default());
        }
        let Some(candidate) = out.get_mut(out_index) else {
            return;
        };
        candidate.slot = self.slot;
        candidate.start_index = start_index;
        candidate.end_index = start_index;
        candidate.total_payload_len = 0;
        candidate.payload_fragments.clear();
        candidate.fragment_observed_ats.clear();
        let mut cursor = start_index;
        while let Some(fragment) = self.fragments.get(&cursor) {
            candidate
                .payload_fragments
                .push(fragment.payload_fragment.clone());
            candidate.fragment_observed_ats.push(fragment.observed_at);
            candidate.total_payload_len = candidate
                .total_payload_len
                .saturating_add(fragment.payload_fragment.len());
            candidate.end_index = cursor;
            cursor = cursor.saturating_add(1);
        }
    }

    fn sync_inline_contiguous_dataset<S: InlineContiguousDataSetSink>(
        &self,
        start_index: u32,
        sink: &mut S,
    ) -> Option<InlineContiguousDataSetMeta> {
        let mut existing_fragments = sink.existing_fragments();
        let mut total_payload_len = sink.payload_len();
        if existing_fragments > 0 {
            let existing_end_index = u32::try_from(existing_fragments.saturating_sub(1))
                .ok()
                .and_then(|width| start_index.checked_add(width));
            if existing_end_index.is_none_or(|end_index| !self.fragments.contains_key(&end_index)) {
                sink.reset_inline_contiguous_dataset();
                existing_fragments = 0;
                total_payload_len = 0;
            }
        }

        let cursor_start = u32::try_from(existing_fragments)
            .ok()
            .and_then(|offset| start_index.checked_add(offset))
            .unwrap_or(start_index);
        let mut additional_fragments = 0_usize;
        let mut additional_bytes = 0_usize;
        let mut end_index = if existing_fragments == 0 {
            start_index
        } else {
            start_index
                .saturating_add(u32::try_from(existing_fragments.saturating_sub(1)).unwrap_or(0))
        };
        let mut cursor = cursor_start;
        while let Some(fragment) = self.fragments.get(&cursor) {
            additional_fragments = additional_fragments.saturating_add(1);
            additional_bytes = additional_bytes.saturating_add(fragment.payload_fragment.len());
            total_payload_len = total_payload_len.saturating_add(fragment.payload_fragment.len());
            end_index = cursor;
            cursor = cursor.saturating_add(1);
        }
        let fragment_count = existing_fragments.saturating_add(additional_fragments);
        if fragment_count == 0 {
            return None;
        }

        sink.reserve_inline_contiguous_dataset(additional_fragments, additional_bytes);
        let mut appended_cursor = cursor_start;
        while let Some(fragment) = self.fragments.get(&appended_cursor) {
            sink.append_inline_contiguous_fragment(
                &fragment.payload_fragment,
                fragment.observed_at,
            );
            appended_cursor = appended_cursor.saturating_add(1);
        }

        Some(InlineContiguousDataSetMeta {
            slot: self.slot,
            start_index,
            end_index,
            fragment_count,
            total_payload_len,
        })
    }

    fn retire_range(&mut self, start_index: u32, end_index: u32) {
        for index in start_index..=end_index {
            let _ = self.fragments.remove(&index);
        }
        self.fec_sets.retain(|_, state| {
            state.retain_outside_range(start_index, end_index);
            !state.observed_indices.is_empty()
        });
        if self.last_index.is_some() && self.fragments.is_empty() {
            self.finished = true;
        }
    }
}

#[derive(Debug)]
struct InlineDataFragment {
    payload_fragment: SharedPayloadFragment,
    observed_at: Instant,
}

#[derive(Debug)]
struct InlineFecSetState {
    fec_set_index: u32,
    observed_indices: BTreeSet<u32>,
    expected_num_data_shreds: Option<u16>,
    terminal_data_index: Option<u32>,
    ready: bool,
}

impl InlineFecSetState {
    #[expect(
        clippy::missing_const_for_fn,
        reason = "BTreeSet construction keeps this initializer non-const on our MSRV"
    )]
    fn new(fec_set_index: u32) -> Self {
        Self {
            fec_set_index,
            observed_indices: BTreeSet::new(),
            expected_num_data_shreds: None,
            terminal_data_index: None,
            ready: false,
        }
    }

    fn observe_data_shred(&mut self, index: u32, boundary: bool) -> bool {
        let _ = self.observed_indices.insert(index);
        if boundary {
            self.terminal_data_index = Some(
                self.terminal_data_index
                    .map_or(index, |prev| prev.min(index)),
            );
        }
        self.try_mark_ready()
    }

    fn observe_code_shred(&mut self, num_data_shreds: u16) -> bool {
        self.expected_num_data_shreds = Some(num_data_shreds);
        self.try_mark_ready()
    }

    fn retain_outside_range(&mut self, start_index: u32, end_index: u32) {
        self.observed_indices
            .retain(|index| *index < start_index || *index > end_index);
        if self
            .terminal_data_index
            .is_some_and(|terminal| terminal >= start_index && terminal <= end_index)
        {
            self.terminal_data_index = None;
        }
        if self.observed_indices.is_empty() {
            self.ready = false;
        }
    }

    fn try_mark_ready(&mut self) -> bool {
        if self.ready {
            return false;
        }
        let Some(end_index) = self.end_index() else {
            return false;
        };
        if (self.fec_set_index..=end_index).all(|index| self.observed_indices.contains(&index)) {
            self.ready = true;
            return true;
        }
        false
    }

    fn end_index(&self) -> Option<u32> {
        if let Some(terminal) = self.terminal_data_index {
            return Some(terminal);
        }
        let num_data = self.expected_num_data_shreds?;
        let width = u32::from(num_data).checked_sub(1)?;
        self.fec_set_index.checked_add(width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingSink {
        payload: Vec<u8>,
        fragment_count: usize,
        append_calls: usize,
        reset_calls: usize,
    }

    impl InlineContiguousDataSetSink for RecordingSink {
        fn existing_fragments(&self) -> usize {
            self.fragment_count
        }

        fn payload_len(&self) -> usize {
            self.payload.len()
        }

        fn reset_inline_contiguous_dataset(&mut self) {
            self.payload.clear();
            self.fragment_count = 0;
            self.reset_calls = self.reset_calls.saturating_add(1);
        }

        fn reserve_inline_contiguous_dataset(
            &mut self,
            additional_fragments: usize,
            additional_bytes: usize,
        ) {
            self.payload.reserve(additional_bytes);
            let _ = additional_fragments;
        }

        fn append_inline_contiguous_fragment(
            &mut self,
            fragment: &SharedPayloadFragment,
            _observed_at: Instant,
        ) {
            self.payload.extend_from_slice(fragment.as_slice());
            self.fragment_count = self.fragment_count.saturating_add(1);
            self.append_calls = self.append_calls.saturating_add(1);
        }
    }

    #[test]
    fn exposes_late_prefix_and_boundary_anchored_tail() {
        let mut reassembler = InlineDataReassembler::new(16);
        let first = Instant::now();
        let Some(second) = first.checked_add(std::time::Duration::from_millis(1)) else {
            panic!("expected later instant");
        };
        let Some(third) = second.checked_add(std::time::Duration::from_millis(1)) else {
            panic!("expected later instant");
        };
        let Some(fourth) = third.checked_add(std::time::Duration::from_millis(1)) else {
            panic!("expected later instant");
        };

        let _ = reassembler.observe_data_shred_meta_at(
            9,
            2,
            0,
            false,
            false,
            SharedPayloadFragment::owned(vec![2]),
            first,
        );
        let _ = reassembler.observe_data_shred_meta_at(
            9,
            3,
            0,
            true,
            false,
            SharedPayloadFragment::owned(vec![3]),
            second,
        );
        let _ = reassembler.observe_data_shred_meta_at(
            9,
            4,
            4,
            false,
            false,
            SharedPayloadFragment::owned(vec![4]),
            third,
        );
        let _ = reassembler.observe_data_shred_meta_at(
            9,
            0,
            0,
            false,
            false,
            SharedPayloadFragment::owned(vec![0]),
            fourth,
        );

        let inline = reassembler.inline_contiguous_datasets(9);
        assert_eq!(inline.len(), 2);
        assert_eq!(inline[0].start_index, 0);
        assert_eq!(inline[0].end_index, 0);
        assert_eq!(
            inline[0].payload_fragments,
            PayloadFragmentBatch::from_owned_fragments(vec![vec![0]])
        );
        assert_eq!(inline[1].start_index, 4);
        assert_eq!(inline[1].end_index, 4);
        assert_eq!(
            inline[1].payload_fragments,
            PayloadFragmentBatch::from_owned_fragments(vec![vec![4]])
        );
    }

    #[test]
    fn retire_range_preserves_following_boundary_anchor() {
        let mut reassembler = InlineDataReassembler::new(16);
        let now = Instant::now();
        let _ = reassembler.observe_data_shred_meta_at(
            11,
            0,
            0,
            false,
            false,
            SharedPayloadFragment::owned(vec![0]),
            now,
        );
        let _ = reassembler.observe_data_shred_meta_at(
            11,
            1,
            0,
            true,
            false,
            SharedPayloadFragment::owned(vec![1]),
            now,
        );
        let _ = reassembler.observe_data_shred_meta_at(
            11,
            2,
            0,
            false,
            false,
            SharedPayloadFragment::owned(vec![2]),
            now,
        );

        reassembler.retire_range(11, 0, 1);

        let inline = reassembler.inline_contiguous_datasets(11);
        assert_eq!(inline.len(), 1);
        assert_eq!(inline[0].start_index, 2);
        assert_eq!(inline[0].end_index, 2);
    }

    #[test]
    fn fec_set_becomes_ready_after_code_and_all_data_arrive() {
        let mut reassembler = InlineDataReassembler::new(16);
        let now = Instant::now();
        assert!(
            !reassembler
                .observe_code_shred(13, 8, 3)
                .fec_set_became_ready
        );
        assert!(
            !reassembler
                .observe_data_shred_meta_at(
                    13,
                    8,
                    8,
                    false,
                    false,
                    SharedPayloadFragment::owned(vec![8]),
                    now,
                )
                .fec_set_became_ready
        );
        assert!(
            !reassembler
                .observe_data_shred_meta_at(
                    13,
                    10,
                    8,
                    false,
                    false,
                    SharedPayloadFragment::owned(vec![10]),
                    now,
                )
                .fec_set_became_ready
        );
        assert!(
            reassembler
                .observe_data_shred_meta_at(
                    13,
                    9,
                    8,
                    false,
                    false,
                    SharedPayloadFragment::owned(vec![9]),
                    now,
                )
                .fec_set_became_ready
        );
    }

    #[test]
    fn boundary_marks_fec_ready_without_code_metadata() {
        let mut reassembler = InlineDataReassembler::new(16);
        let now = Instant::now();
        assert!(
            !reassembler
                .observe_data_shred_meta_at(
                    15,
                    4,
                    4,
                    false,
                    false,
                    SharedPayloadFragment::owned(vec![4]),
                    now,
                )
                .fec_set_became_ready
        );
        assert!(
            reassembler
                .observe_data_shred_meta_at(
                    15,
                    5,
                    4,
                    true,
                    false,
                    SharedPayloadFragment::owned(vec![5]),
                    now,
                )
                .fec_set_became_ready
        );
    }

    #[test]
    fn sync_inline_contiguous_dataset_appends_only_new_suffix() {
        let mut reassembler = InlineDataReassembler::new(16);
        let now = Instant::now();
        let _ = reassembler.observe_data_shred_meta_at(
            17,
            0,
            0,
            false,
            false,
            SharedPayloadFragment::owned(vec![0, 1]),
            now,
        );
        let _ = reassembler.observe_data_shred_meta_at(
            17,
            1,
            0,
            false,
            false,
            SharedPayloadFragment::owned(vec![2, 3]),
            now,
        );

        let Some(stream) = reassembler.slots.get(&17) else {
            panic!("slot stream present");
        };
        let mut sink = RecordingSink::default();
        let Some(first) = stream.sync_inline_contiguous_dataset(0, &mut sink) else {
            panic!("first sync should exist");
        };
        assert_eq!(first.fragment_count, 2);
        assert_eq!(sink.payload, vec![0, 1, 2, 3]);
        assert_eq!(sink.append_calls, 2);
        assert_eq!(sink.reset_calls, 0);

        let _ = reassembler.observe_data_shred_meta_at(
            17,
            2,
            0,
            true,
            false,
            SharedPayloadFragment::owned(vec![4, 5]),
            now,
        );
        let Some(updated_stream) = reassembler.slots.get(&17) else {
            panic!("slot stream present");
        };
        let Some(second) = updated_stream.sync_inline_contiguous_dataset(0, &mut sink) else {
            panic!("second sync should exist");
        };
        assert_eq!(second.fragment_count, 3);
        assert_eq!(sink.payload, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(sink.append_calls, 3);
        assert_eq!(sink.reset_calls, 0);
    }
}
