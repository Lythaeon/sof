use std::collections::HashMap;
use std::sync::Arc;

use super::stream::SlotStream;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SharedPayloadFragment {
    bytes: Arc<[u8]>,
    offset: u32,
    len: u32,
}

impl SharedPayloadFragment {
    #[must_use]
    pub fn borrowed(bytes: Arc<[u8]>, offset: usize, len: usize) -> Option<Self> {
        let end = offset.checked_add(len)?;
        if end > bytes.len() {
            return None;
        }
        Some(Self {
            bytes,
            offset: u32::try_from(offset).ok()?,
            len: u32::try_from(len).ok()?,
        })
    }

    #[must_use]
    pub fn owned(bytes: Vec<u8>) -> Self {
        let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        Self {
            bytes: Arc::from(bytes),
            offset: 0,
            len,
        }
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        let start = usize::try_from(self.offset).unwrap_or(0);
        let len = usize::try_from(self.len).unwrap_or(0);
        let end = start.saturating_add(len);
        self.bytes.get(start..end).unwrap_or(&[])
    }

    #[must_use]
    pub fn len(&self) -> usize {
        usize::try_from(self.len).unwrap_or(usize::MAX)
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct PayloadFragmentBatch {
    fragments: Vec<SharedPayloadFragment>,
    total_len: usize,
    suffix_total_lens: Vec<usize>,
}

impl PayloadFragmentBatch {
    #[must_use]
    pub fn new(fragments: Vec<SharedPayloadFragment>) -> Self {
        let mut suffix_total_lens = Vec::with_capacity(fragments.len());
        let mut total_len = 0_usize;
        for fragment in fragments.iter().rev() {
            total_len = total_len.saturating_add(fragment.len());
            suffix_total_lens.push(total_len);
        }
        suffix_total_lens.reverse();
        Self {
            fragments,
            total_len,
            suffix_total_lens,
        }
    }

    #[must_use]
    pub fn from_owned_fragments(fragments: Vec<Vec<u8>>) -> Self {
        Self::new(
            fragments
                .into_iter()
                .map(SharedPayloadFragment::owned)
                .collect(),
        )
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.fragments.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }

    #[must_use]
    pub const fn total_len(&self) -> usize {
        self.total_len
    }

    #[must_use]
    pub fn total_len_from(&self, start_index: usize) -> Option<usize> {
        self.suffix_total_lens.get(start_index).copied()
    }

    #[must_use]
    pub fn slice_from(&self, start_index: usize) -> Option<&[SharedPayloadFragment]> {
        self.fragments.get(start_index..)
    }

    #[must_use]
    pub fn fragment_len(&self, index: usize) -> Option<usize> {
        self.fragments.get(index).map(SharedPayloadFragment::len)
    }

    #[must_use]
    pub fn single_fragment_from(&self, start_index: usize) -> Option<&SharedPayloadFragment> {
        let suffix = self.fragments.get(start_index..)?;
        match suffix {
            [fragment] => Some(fragment),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CompletedDataSet {
    pub slot: u64,
    pub start_index: u32,
    pub end_index: u32,
    pub payload_fragments: PayloadFragmentBatch,
    pub last_in_slot: bool,
}

#[derive(Debug, Default)]
pub struct DataSetReassembler {
    slots: HashMap<u64, SlotStream>,
    max_tracked_slots: usize,
    retained_slot_lag: u64,
    last_pruned_floor: u64,
    tail_min_shreds_without_anchor: usize,
}

impl DataSetReassembler {
    #[must_use]
    pub fn new(max_tracked_slots: usize) -> Self {
        Self {
            slots: HashMap::new(),
            max_tracked_slots,
            retained_slot_lag: u64::try_from(max_tracked_slots / 4)
                .unwrap_or(u64::MAX)
                .clamp(64, 256),
            last_pruned_floor: 0,
            tail_min_shreds_without_anchor: 2,
        }
    }

    #[must_use]
    pub fn with_retained_slot_lag(mut self, retained_slot_lag: u64) -> Self {
        self.retained_slot_lag = retained_slot_lag.max(1);
        self
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
        payload_fragment: SharedPayloadFragment,
    ) -> Vec<CompletedDataSet> {
        self.purge_older_than(slot.saturating_sub(self.retained_slot_lag));
        self.evict_if_needed(slot);

        let stream = self
            .slots
            .entry(slot)
            .or_insert_with(|| SlotStream::new(slot, self.tail_min_shreds_without_anchor));
        stream.insert(index, data_complete, last_in_slot, payload_fragment);

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

    pub fn purge_older_than(&mut self, slot_floor: u64) -> usize {
        if slot_floor <= self.last_pruned_floor {
            return 0;
        }
        let before = self.slots.len();
        self.slots.retain(|slot, _| *slot >= slot_floor);
        self.last_pruned_floor = slot_floor;
        before.saturating_sub(self.slots.len())
    }

    #[must_use]
    pub fn tracked_slots(&self) -> usize {
        self.slots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purge_older_than_removes_old_slot_streams() {
        let mut reassembler = DataSetReassembler::new(16).with_retained_slot_lag(64);
        let _ = reassembler.slots.insert(100, SlotStream::new(100, 2));
        let _ = reassembler.slots.insert(120, SlotStream::new(120, 2));
        let _ = reassembler.slots.insert(140, SlotStream::new(140, 2));

        let purged = reassembler.purge_older_than(121);

        assert_eq!(purged, 2);
        assert_eq!(reassembler.tracked_slots(), 1);
        assert!(reassembler.slots.contains_key(&140));
    }
}
