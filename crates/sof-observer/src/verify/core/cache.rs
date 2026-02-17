use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use crate::protocol::shred_wire::SIZE_OF_SIGNATURE;

#[derive(Debug, Clone, Copy)]
pub(super) enum SignatureCacheEntry {
    Known([u8; 32]),
    Unknown(Instant),
}

#[derive(Debug)]
pub(super) struct SignatureCache {
    map: HashMap<[u8; SIZE_OF_SIGNATURE], SignatureCacheEntry>,
    order: VecDeque<[u8; SIZE_OF_SIGNATURE]>,
    capacity: usize,
}

impl SignatureCache {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            capacity,
        }
    }

    pub(super) fn get(&self, signature: &[u8; SIZE_OF_SIGNATURE]) -> Option<SignatureCacheEntry> {
        self.map.get(signature).copied()
    }

    pub(super) fn remove(&mut self, signature: &[u8; SIZE_OF_SIGNATURE]) {
        let _ = self.map.remove(signature);
    }

    pub(super) fn insert(
        &mut self,
        signature: [u8; SIZE_OF_SIGNATURE],
        value: SignatureCacheEntry,
    ) {
        if !self.map.contains_key(&signature) {
            self.order.push_back(signature);
        }
        let _ = self.map.insert(signature, value);
        while self.map.len() > self.capacity {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            let _ = self.map.remove(&oldest);
        }
    }
}
