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
    leader_signature: [u8; SIZE_OF_SIGNATURE],
    data_shreds: HashMap<u32, Vec<u8>>,
    coding_shreds: HashMap<u16, Vec<u8>>,
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
            leader_signature,
            data_shreds: HashMap::new(),
            coding_shreds: HashMap::new(),
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
            set.ingest_packet(&parsed, packet.to_vec());
            recovered = recover_missing_data(set, fec_set_index, &mut self.reed_solomon_cache)
                .unwrap_or_default();
            should_remove = set.is_data_complete_for_config(fec_set_index);
        } else {
            let mut set = ErasureSet::new(signature);
            set.ingest_packet(&parsed, packet.to_vec());
            let _ = self.sets.insert(set_id, set);
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

    fn ingest_packet(&mut self, parsed: &ParsedShred, packet: Vec<u8>) {
        let common_variant = match parsed {
            ParsedShred::Data(data) => data.common.shred_variant,
            ParsedShred::Code(code) => code.common.shred_variant,
        };
        if self.variant.is_none() {
            self.variant = Some(SetVariant::from(common_variant));
        }

        match parsed {
            ParsedShred::Data(data) => {
                let _ = self.data_shreds.entry(data.common.index).or_insert(packet);
            }
            ParsedShred::Code(code) => {
                let incoming_config = ErasureConfig {
                    num_data: usize::from(code.coding_header.num_data_shreds),
                    num_coding: usize::from(code.coding_header.num_coding_shreds),
                };
                if let Some(config) = self.config
                    && config != incoming_config
                {
                    return;
                }
                let _ = self
                    .coding_shreds
                    .entry(code.coding_header.position)
                    .or_insert(packet);
                self.config = Some(incoming_config);
            }
        }
    }

    fn is_data_complete_for_config(&self, fec_set_index: u32) -> bool {
        let Some(config) = self.config else {
            return false;
        };
        let mut count = 0_usize;
        for position in 0..config.num_data {
            let Ok(position_u32) = u32::try_from(position) else {
                return false;
            };
            let Some(index) = fec_set_index.checked_add(position_u32) else {
                return false;
            };
            if self.data_shreds.contains_key(&index) {
                count = count.saturating_add(1);
            }
        }
        count >= config.num_data
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
}
