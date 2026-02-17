use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::protocol::shred_wire::{
    OFFSET_FEC_SET_INDEX, OFFSET_INDEX, OFFSET_SLOT, SIZE_OF_CODING_SHRED_PAYLOAD,
    SIZE_OF_DATA_SHRED_PAYLOAD,
};

use super::{
    cache::{SignatureCache, SignatureCacheEntry},
    merkle::{compute_merkle_root, verify_signature},
    packet::{parse_signature, parse_variant, read_u32_le, read_u64_le},
    types::{ShredKind, VerifyStatus},
};

#[derive(Debug)]
pub struct ShredVerifier {
    known_pubkeys: Vec<[u8; 32]>,
    slot_leaders: HashMap<u64, [u8; 32]>,
    latest_slot: u64,
    has_latest_slot: bool,
    slot_leader_window: u64,
    signature_cache: SignatureCache,
    unknown_retry: Duration,
}

impl ShredVerifier {
    #[must_use]
    pub fn new(
        signature_cache_capacity: usize,
        slot_leader_window: u64,
        unknown_retry: Duration,
    ) -> Self {
        Self {
            known_pubkeys: Vec::new(),
            slot_leaders: HashMap::new(),
            latest_slot: 0,
            has_latest_slot: false,
            slot_leader_window,
            signature_cache: SignatureCache::new(signature_cache_capacity),
            unknown_retry,
        }
    }

    pub fn set_known_pubkeys(&mut self, mut pubkeys: Vec<[u8; 32]>) {
        pubkeys.sort_unstable();
        pubkeys.dedup();
        self.known_pubkeys = pubkeys;
    }

    pub fn set_slot_leaders<I>(&mut self, leaders: I)
    where
        I: IntoIterator<Item = (u64, [u8; 32])>,
    {
        let mut latest = if self.has_latest_slot {
            self.latest_slot
        } else {
            0
        };
        let mut has_latest = self.has_latest_slot;
        for (slot, pubkey) in leaders {
            let _ = self.slot_leaders.insert(slot, pubkey);
            if !has_latest || slot > latest {
                latest = slot;
                has_latest = true;
            }
        }
        self.latest_slot = latest;
        self.has_latest_slot = has_latest;
        self.evict_old_slot_leaders();
    }

    pub fn verify_packet(&mut self, packet: &[u8], now: Instant) -> VerifyStatus {
        let Some(variant) = parse_variant(packet) else {
            return VerifyStatus::Malformed;
        };
        let shred_len = match variant.kind {
            ShredKind::Data => SIZE_OF_DATA_SHRED_PAYLOAD,
            ShredKind::Code => SIZE_OF_CODING_SHRED_PAYLOAD,
        };
        let Some(shred) = packet.get(..shred_len) else {
            return VerifyStatus::Malformed;
        };
        let Some(slot) = read_u64_le(shred, OFFSET_SLOT) else {
            return VerifyStatus::Malformed;
        };
        let Some(index) = read_u32_le(shred, OFFSET_INDEX) else {
            return VerifyStatus::Malformed;
        };
        let Some(fec_set_index) = read_u32_le(shred, OFFSET_FEC_SET_INDEX) else {
            return VerifyStatus::Malformed;
        };
        let Some(signature_bytes) = parse_signature(shred) else {
            return VerifyStatus::Malformed;
        };
        let Some(merkle_root) = compute_merkle_root(shred, variant, index, fec_set_index) else {
            return VerifyStatus::InvalidMerkle;
        };

        let mut had_slot_leader = false;
        if let Some(leader) = self.slot_leaders.get(&slot).copied() {
            had_slot_leader = true;
            if verify_signature(signature_bytes, leader, merkle_root) {
                return VerifyStatus::Verified;
            }
        }

        if let Some(entry) = self.signature_cache.get(&signature_bytes) {
            match entry {
                SignatureCacheEntry::Known(pubkey) => {
                    if verify_signature(signature_bytes, pubkey, merkle_root) {
                        self.remember_slot_leader(slot, pubkey);
                        return VerifyStatus::Verified;
                    }
                    self.signature_cache.remove(&signature_bytes);
                }
                SignatureCacheEntry::Unknown(last_checked) => {
                    if now.saturating_duration_since(last_checked) < self.unknown_retry {
                        return VerifyStatus::UnknownLeader;
                    }
                }
            }
        }

        let mut matched_pubkey: Option<[u8; 32]> = None;
        for pubkey in &self.known_pubkeys {
            if verify_signature(signature_bytes, *pubkey, merkle_root) {
                matched_pubkey = Some(*pubkey);
                break;
            }
        }
        if let Some(pubkey) = matched_pubkey {
            self.remember_slot_leader(slot, pubkey);
            self.signature_cache
                .insert(signature_bytes, SignatureCacheEntry::Known(pubkey));
            return VerifyStatus::Verified;
        }

        self.signature_cache
            .insert(signature_bytes, SignatureCacheEntry::Unknown(now));
        if had_slot_leader {
            VerifyStatus::InvalidSignature
        } else {
            VerifyStatus::UnknownLeader
        }
    }

    fn remember_slot_leader(&mut self, slot: u64, pubkey: [u8; 32]) {
        let _ = self.slot_leaders.insert(slot, pubkey);
        if !self.has_latest_slot || slot > self.latest_slot {
            self.latest_slot = slot;
            self.has_latest_slot = true;
        }
        self.evict_old_slot_leaders();
    }

    fn evict_old_slot_leaders(&mut self) {
        if !self.has_latest_slot {
            return;
        }
        let floor = self.latest_slot.saturating_sub(self.slot_leader_window);
        self.slot_leaders.retain(|slot, _| *slot >= floor);
    }
}
