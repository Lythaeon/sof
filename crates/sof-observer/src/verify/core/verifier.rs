use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use ed25519_dalek::{Signature as DalekSignature, VerifyingKey};

use crate::protocol::shred_wire::{
    OFFSET_FEC_SET_INDEX, OFFSET_INDEX, OFFSET_SLOT, SIZE_OF_CODING_SHRED_PAYLOAD,
    SIZE_OF_DATA_SHRED_PAYLOAD, SIZE_OF_SIGNATURE,
};

use super::{
    cache::{SignatureCache, SignatureCacheEntry},
    merkle::compute_merkle_root,
    packet::{parse_signature, parse_variant, read_u32_le, read_u64_le},
    types::{ShredKind, VerifyStatus},
};

#[derive(Debug, Clone)]
struct PreparedPubkeyVerifier {
    pubkey: [u8; 32],
    verifying_key: VerifyingKey,
}

#[derive(Debug, Default)]
pub struct SlotLeaderDiff {
    pub added: Vec<(u64, [u8; 32])>,
    pub updated: Vec<(u64, [u8; 32])>,
    pub removed_slots: Vec<u64>,
}

#[derive(Debug)]
pub struct ShredVerifier {
    known_pubkey_verifiers: Vec<PreparedPubkeyVerifier>,
    slot_leaders: HashMap<u64, [u8; 32]>,
    pending_added_slot_leaders: HashMap<u64, [u8; 32]>,
    pending_updated_slot_leaders: HashMap<u64, [u8; 32]>,
    pending_removed_slots: HashSet<u64>,
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
            known_pubkey_verifiers: Vec::new(),
            slot_leaders: HashMap::new(),
            pending_added_slot_leaders: HashMap::new(),
            pending_updated_slot_leaders: HashMap::new(),
            pending_removed_slots: HashSet::new(),
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
        let mut known_pubkey_verifiers = Vec::with_capacity(pubkeys.len());
        for pubkey in pubkeys {
            let Ok(verifying_key) = VerifyingKey::from_bytes(&pubkey) else {
                continue;
            };
            known_pubkey_verifiers.push(PreparedPubkeyVerifier {
                pubkey,
                verifying_key,
            });
        }
        self.known_pubkey_verifiers = known_pubkey_verifiers;
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
            let previous = self.slot_leaders.insert(slot, pubkey);
            self.record_slot_leader_change(slot, pubkey, previous);
            if !has_latest || slot > latest {
                latest = slot;
                has_latest = true;
            }
        }
        self.latest_slot = latest;
        self.has_latest_slot = has_latest;
        self.evict_old_slot_leaders();
    }

    #[must_use]
    pub fn slot_leaders_snapshot(&self) -> Vec<(u64, [u8; 32])> {
        let mut leaders: Vec<(u64, [u8; 32])> = self
            .slot_leaders
            .iter()
            .map(|(slot, pubkey)| (*slot, *pubkey))
            .collect();
        leaders.sort_unstable_by_key(|(slot, _)| *slot);
        leaders
    }

    #[must_use]
    pub fn slot_leader_for_slot(&self, slot: u64) -> Option<[u8; 32]> {
        self.slot_leaders.get(&slot).copied()
    }

    #[must_use]
    pub fn take_slot_leader_diff(&mut self) -> SlotLeaderDiff {
        let mut added: Vec<(u64, [u8; 32])> = self.pending_added_slot_leaders.drain().collect();
        let mut updated: Vec<(u64, [u8; 32])> = self.pending_updated_slot_leaders.drain().collect();
        let mut removed_slots: Vec<u64> = self.pending_removed_slots.drain().collect();
        added.sort_unstable_by_key(|(slot, _)| *slot);
        updated.sort_unstable_by_key(|(slot, _)| *slot);
        removed_slots.sort_unstable();
        SlotLeaderDiff {
            added,
            updated,
            removed_slots,
        }
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
        let slot_leader = self.slot_leaders.get(&slot).copied();
        let had_slot_leader = slot_leader.is_some();

        // Unknown-leader retries can be dropped before Merkle work when a slot
        // leader is still unavailable.
        if !had_slot_leader
            && let Some(SignatureCacheEntry::Unknown(last_checked)) =
                self.signature_cache.get(&signature_bytes)
            && now.saturating_duration_since(last_checked) < self.unknown_retry
        {
            return VerifyStatus::UnknownLeader;
        }

        let Some(merkle_root) = compute_merkle_root(shred, variant, index, fec_set_index) else {
            return VerifyStatus::InvalidMerkle;
        };
        let signature = DalekSignature::from_bytes(&signature_bytes);

        if let Some(entry) = self.signature_cache.get(&signature_bytes) {
            match entry {
                SignatureCacheEntry::Known {
                    pubkey,
                    merkle_root: cached_merkle_root,
                } => {
                    if cached_merkle_root == merkle_root {
                        self.remember_slot_leader(slot, pubkey);
                        return VerifyStatus::Verified;
                    }
                    if verify_signature_for_pubkey(&signature, pubkey, &merkle_root) {
                        self.remember_slot_leader(slot, pubkey);
                        self.cache_known_signature(signature_bytes, pubkey, merkle_root);
                        return VerifyStatus::Verified;
                    }
                    self.signature_cache.remove(&signature_bytes);
                }
                SignatureCacheEntry::Unknown(last_checked) => {
                    if !had_slot_leader
                        && now.saturating_duration_since(last_checked) < self.unknown_retry
                    {
                        return VerifyStatus::UnknownLeader;
                    }
                }
            }
        }

        if let Some(leader) = slot_leader
            && verify_signature_for_pubkey(&signature, leader, &merkle_root)
        {
            self.cache_known_signature(signature_bytes, leader, merkle_root);
            return VerifyStatus::Verified;
        }

        let matched_pubkey = self.verify_known_pubkeys_ranked(&signature, &merkle_root);
        if let Some(pubkey) = matched_pubkey {
            self.remember_slot_leader(slot, pubkey);
            self.cache_known_signature(signature_bytes, pubkey, merkle_root);
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

    fn verify_known_pubkeys_ranked(
        &mut self,
        signature: &DalekSignature,
        merkle_root: &[u8; 32],
    ) -> Option<[u8; 32]> {
        let match_index = self.known_pubkey_verifiers.iter().position(|entry| {
            verify_signature_with_key(signature, &entry.verifying_key, merkle_root)
        });
        let index = match_index?;
        let pubkey = self
            .known_pubkey_verifiers
            .get(index)
            .map(|entry| entry.pubkey)?;
        if index > 0
            && let Some(prefix) = self.known_pubkey_verifiers.get_mut(..=index)
        {
            // Keep recently matched keys at the front to reduce average
            // candidate scans for active leaders.
            prefix.rotate_right(1);
        }
        Some(pubkey)
    }

    fn cache_known_signature(
        &mut self,
        signature_bytes: [u8; SIZE_OF_SIGNATURE],
        pubkey: [u8; 32],
        merkle_root: [u8; 32],
    ) {
        self.signature_cache.insert(
            signature_bytes,
            SignatureCacheEntry::Known {
                pubkey,
                merkle_root,
            },
        );
    }

    fn remember_slot_leader(&mut self, slot: u64, pubkey: [u8; 32]) {
        let previous = self.slot_leaders.insert(slot, pubkey);
        self.record_slot_leader_change(slot, pubkey, previous);
        if !self.has_latest_slot || slot > self.latest_slot {
            self.latest_slot = slot;
            self.has_latest_slot = true;
        }
        self.evict_old_slot_leaders();
    }

    fn record_slot_leader_change(
        &mut self,
        slot: u64,
        leader: [u8; 32],
        previous: Option<[u8; 32]>,
    ) {
        match previous {
            None => {
                let _ = self.pending_added_slot_leaders.insert(slot, leader);
                let _ = self.pending_updated_slot_leaders.remove(&slot);
            }
            Some(previous_leader) if previous_leader != leader => {
                if let Some(pending_added) = self.pending_added_slot_leaders.get_mut(&slot) {
                    *pending_added = leader;
                } else {
                    let _ = self.pending_updated_slot_leaders.insert(slot, leader);
                }
            }
            Some(_) => {}
        }
        let _ = self.pending_removed_slots.remove(&slot);
    }

    fn evict_old_slot_leaders(&mut self) {
        if !self.has_latest_slot {
            return;
        }
        let floor = self.latest_slot.saturating_sub(self.slot_leader_window);
        let mut removed_slots = Vec::new();
        self.slot_leaders.retain(|slot, _| {
            let keep = *slot >= floor;
            if !keep {
                removed_slots.push(*slot);
            }
            keep
        });
        for slot in removed_slots {
            let _ = self.pending_added_slot_leaders.remove(&slot);
            let _ = self.pending_updated_slot_leaders.remove(&slot);
            let _ = self.pending_removed_slots.insert(slot);
        }
    }
}

fn verify_signature_for_pubkey(
    signature: &DalekSignature,
    pubkey: [u8; 32],
    merkle_root: &[u8; 32],
) -> bool {
    let Ok(verifying_key) = VerifyingKey::from_bytes(&pubkey) else {
        return false;
    };
    verify_signature_with_key(signature, &verifying_key, merkle_root)
}

fn verify_signature_with_key(
    signature: &DalekSignature,
    verifying_key: &VerifyingKey,
    merkle_root: &[u8; 32],
) -> bool {
    verifying_key.verify_strict(merkle_root, signature).is_ok()
}
