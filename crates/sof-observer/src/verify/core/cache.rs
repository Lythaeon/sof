use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use crate::protocol::shred_wire::SIZE_OF_SIGNATURE;

#[derive(Debug, Clone, Copy)]
pub(super) enum SignatureCacheEntry {
    Known {
        pubkey: [u8; 32],
        merkle_root: [u8; 32],
    },
    Unknown(Instant),
}

#[derive(Debug)]
pub(super) struct SignatureCache {
    map: HashMap<[u8; SIZE_OF_SIGNATURE], SignatureCacheRecord>,
    order: VecDeque<([u8; SIZE_OF_SIGNATURE], u64)>,
    capacity: usize,
    next_generation: u64,
}

#[derive(Debug, Clone, Copy)]
struct SignatureCacheRecord {
    entry: SignatureCacheEntry,
    generation: u64,
}

impl SignatureCache {
    pub(super) fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            map: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
            next_generation: 0,
        }
    }

    pub(super) fn get(&self, signature: &[u8; SIZE_OF_SIGNATURE]) -> Option<SignatureCacheEntry> {
        self.map.get(signature).map(|record| record.entry)
    }

    pub(super) fn remove(&mut self, signature: &[u8; SIZE_OF_SIGNATURE]) {
        let _ = self.map.remove(signature);
    }

    pub(super) fn insert(
        &mut self,
        signature: [u8; SIZE_OF_SIGNATURE],
        value: SignatureCacheEntry,
    ) {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        self.order.push_back((signature, generation));
        let _ = self.map.insert(
            signature,
            SignatureCacheRecord {
                entry: value,
                generation,
            },
        );
        while self.map.len() > self.capacity {
            let Some((oldest, queued_generation)) = self.order.pop_front() else {
                break;
            };
            if matches!(
                self.map.get(&oldest),
                Some(record) if record.generation == queued_generation
            ) {
                let _ = self.map.remove(&oldest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, VecDeque},
        hint::black_box,
        time::Instant,
    };

    use sof_support::bench::profile_iterations;

    use super::{SIZE_OF_SIGNATURE, SignatureCache, SignatureCacheEntry};

    #[test]
    fn stale_queue_entries_do_not_evict_reinserted_signature() {
        let mut cache = SignatureCache::new(2);
        let first = [1_u8; 64];
        let second = [2_u8; 64];
        let third = [3_u8; 64];
        let old_at = Instant::now();
        let refreshed_at = Instant::now();

        cache.insert(first, SignatureCacheEntry::Unknown(old_at));
        cache.insert(second, SignatureCacheEntry::Unknown(Instant::now()));
        cache.remove(&first);
        cache.insert(first, SignatureCacheEntry::Unknown(refreshed_at));
        cache.insert(third, SignatureCacheEntry::Unknown(Instant::now()));

        assert!(matches!(
            cache.get(&first),
            Some(SignatureCacheEntry::Unknown(value)) if value == refreshed_at
        ));
        assert!(cache.get(&second).is_none());
        assert!(cache.get(&third).is_some());
    }

    #[test]
    #[ignore = "profiling fixture for signature cache insert churn"]
    fn signature_cache_insert_profile_fixture() {
        let iterations = profile_iterations(200_000);
        let mut baseline = signature_cache_baseline(4096);
        let mut optimized = SignatureCache::new(4096);
        let started_at = Instant::now();

        let baseline_started = Instant::now();
        for i in 0..iterations {
            let signature = make_signature(i);
            baseline.insert(signature, SignatureCacheEntry::Unknown(started_at));
            black_box(());
        }
        let baseline_elapsed = baseline_started.elapsed();

        let optimized_started = Instant::now();
        for i in 0..iterations {
            let signature = make_signature(i);
            optimized.insert(signature, SignatureCacheEntry::Unknown(started_at));
            black_box(());
        }
        let optimized_elapsed = optimized_started.elapsed();

        eprintln!(
            "signature_cache_insert_profile_fixture iterations={} baseline_us={} optimized_us={}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
        );
    }

    fn signature_cache_baseline(capacity: usize) -> SignatureCache {
        SignatureCache {
            map: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
            next_generation: 0,
        }
    }

    fn make_signature(iteration: usize) -> [u8; SIZE_OF_SIGNATURE] {
        let mut signature = [0_u8; SIZE_OF_SIGNATURE];
        signature[..8].copy_from_slice(&u64::try_from(iteration).unwrap_or(0).to_le_bytes());
        signature
    }
}
