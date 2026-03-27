use std::collections::{HashMap, hash_map::Entry};

use reed_solomon_erasure::galois_8::ReedSolomon;

use crate::shred::wire::{ParsedShred, ShredVariant, parse_shred};

#[path = "recover.rs"]
mod recover;

use recover::{parse_packet_signature, recover_missing_data};

const SIZE_OF_SIGNATURE: usize = 64;
const SIZE_OF_MERKLE_ROOT: usize = 32;
const SIZE_OF_MERKLE_PROOF_ENTRY: usize = 20;

pub struct FecRecoverer {
    sets: HashMap<(u64, u32), ErasureSet>,
    reed_solomon_cache: HashMap<(usize, usize), ReedSolomon>,
    max_tracked_sets: usize,
    retained_slot_lag: u64,
    last_pruned_floor: u64,
}

struct ErasureSet {
    variant: Option<SetVariant>,
    config: Option<ErasureConfig>,
    config_fec_set_index: Option<u32>,
    leader_signature: [u8; SIZE_OF_SIGNATURE],
    data_shards: HashMap<u32, Vec<u8>>,
    coding_shards: HashMap<u16, Vec<u8>>,
    present_data_shreds_in_config: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SetVariant {
    proof_size: u8,
    resigned: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ErasureConfig {
    num_data: usize,
    num_coding: usize,
}

impl ErasureSet {
    fn new(leader_signature: [u8; SIZE_OF_SIGNATURE]) -> Self {
        Self {
            variant: None,
            config: None,
            config_fec_set_index: None,
            leader_signature,
            data_shards: HashMap::new(),
            coding_shards: HashMap::new(),
            present_data_shreds_in_config: 0,
        }
    }
}

impl FecRecoverer {
    #[must_use]
    pub fn new(max_tracked_sets: usize, retained_slot_lag: u64) -> Self {
        Self {
            sets: HashMap::new(),
            reed_solomon_cache: HashMap::new(),
            max_tracked_sets,
            retained_slot_lag: retained_slot_lag.max(1),
            last_pruned_floor: 0,
        }
    }

    pub fn ingest_packet(&mut self, packet: &[u8]) -> Vec<Vec<u8>> {
        let parsed = match parse_shred(packet) {
            Ok(parsed) => parsed,
            Err(_) => return Vec::new(),
        };
        let signature = match parse_packet_signature(packet) {
            Some(signature) => signature,
            None => return Vec::new(),
        };

        let (slot, fec_set_index, variant) = match &parsed {
            ParsedShred::Data(data) => (
                data.common.slot,
                data.common.fec_set_index,
                SetVariant::from(data.common.shred_variant),
            ),
            ParsedShred::Code(code) => (
                code.common.slot,
                code.common.fec_set_index,
                SetVariant::from(code.common.shred_variant),
            ),
        };

        let set_id = (slot, fec_set_index);
        self.purge_older_than(slot.saturating_sub(self.retained_slot_lag));
        self.evict_if_needed(set_id);

        let mut recovered = Vec::new();
        let mut should_remove = false;
        if let Some(set) = self.sets.get_mut(&set_id) {
            if !set.accepts_variant(variant) {
                return Vec::new();
            }
            set.ingest_packet(&parsed, packet);
            recovered = recover_missing_data(set, fec_set_index, &mut self.reed_solomon_cache)
                .unwrap_or_default();
            should_remove = set.is_data_complete_for_config(fec_set_index);
        } else {
            let mut new_set = ErasureSet::new(signature);
            new_set.ingest_packet(&parsed, packet);
            let _ = self.sets.insert(set_id, new_set);
        }

        if should_remove {
            let _ = self.sets.remove(&set_id);
        }

        recovered
    }

    fn evict_if_needed(&mut self, incoming_set: (u64, u32)) {
        if self.sets.len() < self.max_tracked_sets || self.sets.contains_key(&incoming_set) {
            return;
        }
        if let Some(oldest_key) = self
            .sets
            .keys()
            .min_by_key(|(slot, fec_set_index)| (*slot, *fec_set_index))
            .copied()
        {
            let _ = self.sets.remove(&oldest_key);
        }
    }

    pub fn purge_older_than(&mut self, slot_floor: u64) -> usize {
        if slot_floor <= self.last_pruned_floor {
            return 0;
        }
        let before = self.sets.len();
        self.sets.retain(|(slot, _), _| *slot >= slot_floor);
        self.last_pruned_floor = slot_floor;
        before.saturating_sub(self.sets.len())
    }

    #[must_use]
    pub fn tracked_sets(&self) -> usize {
        self.sets.len()
    }
}

impl ErasureSet {
    fn accepts_variant(&self, incoming: SetVariant) -> bool {
        self.variant.is_none_or(|existing| existing == incoming)
    }

    fn ingest_packet(&mut self, parsed: &ParsedShred, packet: &[u8]) {
        let common_variant = match parsed {
            ParsedShred::Data(data) => data.common.shred_variant,
            ParsedShred::Code(code) => code.common.shred_variant,
        };
        if self.variant.is_none() {
            self.variant = Some(SetVariant::from(common_variant));
        }
        let Some(shard_len) = coding_erasure_shard_len(SetVariant::from(common_variant)) else {
            return;
        };

        match parsed {
            ParsedShred::Data(data) => {
                let Some(shard) = extract_data_erasure_shard(packet, shard_len) else {
                    return;
                };
                if let Entry::Vacant(vacant) = self.data_shards.entry(data.common.index) {
                    let _ = vacant.insert(shard);
                    if self.index_within_config(data.common.index, self.config_fec_set_index) {
                        self.present_data_shreds_in_config =
                            self.present_data_shreds_in_config.saturating_add(1);
                    }
                }
            }
            ParsedShred::Code(code) => {
                let Some(shard) = extract_coding_erasure_shard(packet, shard_len) else {
                    return;
                };
                let incoming_config = ErasureConfig {
                    num_data: usize::from(code.coding_header.num_data_shreds),
                    num_coding: usize::from(code.coding_header.num_coding_shreds),
                };
                if let Some(config) = self.config
                    && config != incoming_config
                {
                    return;
                }
                if let Entry::Vacant(vacant) = self.coding_shards.entry(code.coding_header.position)
                {
                    let _ = vacant.insert(shard);
                }
                if self.config != Some(incoming_config)
                    || self.config_fec_set_index != Some(code.common.fec_set_index)
                {
                    self.config = Some(incoming_config);
                    self.config_fec_set_index = Some(code.common.fec_set_index);
                    self.present_data_shreds_in_config =
                        self.count_present_data_shreds_in_config(code.common.fec_set_index);
                }
            }
        }
    }

    fn is_data_complete_for_config(&self, fec_set_index: u32) -> bool {
        let Some(config) = self.config else {
            return false;
        };
        self.present_data_shreds_in_config >= config.num_data
            && self.config_fec_set_index == Some(fec_set_index)
    }

    fn index_within_config(&self, index: u32, fec_set_index: Option<u32>) -> bool {
        let (Some(config), Some(fec_set_index)) = (self.config, fec_set_index) else {
            return false;
        };
        let Some(delta) = index.checked_sub(fec_set_index) else {
            return false;
        };
        let Ok(offset) = usize::try_from(delta) else {
            return false;
        };
        offset < config.num_data
    }

    fn count_present_data_shreds_in_config(&self, fec_set_index: u32) -> usize {
        let Some(config) = self.config else {
            return 0;
        };
        self.data_shards
            .keys()
            .filter(|&&index| {
                usize::try_from(index.saturating_sub(fec_set_index))
                    .ok()
                    .is_some_and(|offset| offset < config.num_data)
            })
            .count()
    }

    fn insert_recovered_data_shred(
        &mut self,
        fec_set_index: u32,
        index: u32,
        recovered_shard: Vec<u8>,
    ) -> bool {
        if let Entry::Vacant(vacant) = self.data_shards.entry(index) {
            let _ = vacant.insert(recovered_shard);
            if self.index_within_config(index, Some(fec_set_index)) {
                self.present_data_shreds_in_config =
                    self.present_data_shreds_in_config.saturating_add(1);
            }
            return true;
        }
        false
    }
}

impl From<ShredVariant> for SetVariant {
    fn from(value: ShredVariant) -> Self {
        Self {
            proof_size: value.proof_size,
            resigned: value.resigned,
        }
    }
}

fn extract_data_erasure_shard(packet: &[u8], shard_len: usize) -> Option<Vec<u8>> {
    let start = SIZE_OF_SIGNATURE;
    let end = start.checked_add(shard_len)?;
    packet.get(start..end).map(ToOwned::to_owned)
}

fn extract_coding_erasure_shard(packet: &[u8], shard_len: usize) -> Option<Vec<u8>> {
    let start = crate::shred::wire::SIZE_OF_CODING_SHRED_HEADERS;
    let end = start.checked_add(shard_len)?;
    packet.get(start..end).map(ToOwned::to_owned)
}

fn coding_erasure_shard_len(variant: SetVariant) -> Option<usize> {
    let proof = usize::from(variant.proof_size);
    let proof_bytes = proof.checked_mul(SIZE_OF_MERKLE_PROOF_ENTRY)?;
    let trailer =
        SIZE_OF_MERKLE_ROOT
            .checked_add(proof_bytes)?
            .checked_add(if variant.resigned {
                SIZE_OF_SIGNATURE
            } else {
                0
            })?;
    crate::shred::wire::SIZE_OF_CODING_SHRED_PAYLOAD
        .checked_sub(crate::shred::wire::SIZE_OF_CODING_SHRED_HEADERS.checked_add(trailer)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purge_older_than_removes_old_sets() {
        let mut recoverer = FecRecoverer::new(16, 64);
        let _ = recoverer
            .sets
            .insert((100, 0), ErasureSet::new([0; SIZE_OF_SIGNATURE]));
        let _ = recoverer
            .sets
            .insert((120, 0), ErasureSet::new([0; SIZE_OF_SIGNATURE]));
        let _ = recoverer
            .sets
            .insert((140, 0), ErasureSet::new([0; SIZE_OF_SIGNATURE]));

        let purged = recoverer.purge_older_than(121);

        assert_eq!(purged, 2);
        assert_eq!(recoverer.tracked_sets(), 1);
        assert!(recoverer.sets.contains_key(&(140, 0)));
    }

    #[test]
    fn data_completeness_tracks_in_range_count_once_config_is_known() {
        let mut set = ErasureSet::new([0; SIZE_OF_SIGNATURE]);
        let _ = set.data_shards.insert(10, vec![1]);
        let _ = set.data_shards.insert(11, vec![2]);

        set.config = Some(ErasureConfig {
            num_data: 2,
            num_coding: 1,
        });
        set.config_fec_set_index = Some(10);
        set.present_data_shreds_in_config = set.count_present_data_shreds_in_config(10);

        assert!(set.is_data_complete_for_config(10));

        let mut incomplete_set = ErasureSet::new([0; SIZE_OF_SIGNATURE]);
        let _ = incomplete_set.data_shards.insert(10, vec![1]);
        let _ = incomplete_set.data_shards.insert(12, vec![2]);
        incomplete_set.config = Some(ErasureConfig {
            num_data: 2,
            num_coding: 1,
        });
        incomplete_set.config_fec_set_index = Some(10);
        incomplete_set.present_data_shreds_in_config =
            incomplete_set.count_present_data_shreds_in_config(10);

        assert!(!incomplete_set.is_data_complete_for_config(10));
    }
}
