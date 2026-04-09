use super::*;
use crate::shred::wire::{
    ParsedDataShredHeader, ParsedShredHeader, SIZE_OF_DATA_SHRED_PAYLOAD, parse_shred_header,
};

#[derive(Debug)]
pub struct RecoveredDataPacket {
    pub bytes: Vec<u8>,
    pub parsed: ParsedDataShredHeader,
}

pub(super) fn recover_missing_data(
    set: &mut ErasureSet,
    fec_set_index: u32,
    reed_solomon_cache: &mut HashMap<(usize, usize), ReedSolomon>,
    recovery_scratch: &mut RecoveryScratch,
) -> Option<Vec<RecoveredDataPacket>> {
    let config = set.config?;
    let variant = set.variant?;
    if config.num_data == 0 || config.num_coding == 0 {
        return None;
    }

    let _ = coding_erasure_shard_len(variant)?;
    let total = config.num_data.checked_add(config.num_coding)?;
    recovery_scratch.prepare(total, config.num_data);
    let shards = &mut recovery_scratch.shards;
    let mut present = 0_usize;
    let data_present = &mut recovery_scratch.data_present;

    for (&index, shard) in &set.data_shards {
        let Some(position) = index.checked_sub(fec_set_index) else {
            continue;
        };
        let Ok(position) = usize::try_from(position) else {
            continue;
        };
        if position >= config.num_data {
            continue;
        }
        if shards.get(position).is_some_and(Option::is_none) {
            present = present.saturating_add(1);
        }
        if let Some(data_present_slot) = data_present.get_mut(position) {
            *data_present_slot = true;
        } else {
            continue;
        }
        if let Some(shard_slot) = shards.get_mut(position) {
            let Some(bytes) = shard.to_owned_vec() else {
                continue;
            };
            *shard_slot = Some(bytes);
        } else {
            continue;
        }
    }

    for (&position, shard) in &set.coding_shards {
        let position = usize::from(position);
        if position >= config.num_coding {
            continue;
        }
        let Some(slot) = config.num_data.checked_add(position) else {
            continue;
        };
        if shards.get(slot).is_some_and(Option::is_none) {
            present = present.saturating_add(1);
        }
        if let Some(shard_slot) = shards.get_mut(slot) {
            *shard_slot = Some(shard.clone());
        } else {
            continue;
        }
    }

    if present < config.num_data {
        return None;
    }
    if data_present.iter().all(|has_data| *has_data) {
        return Some(Vec::new());
    }

    let key = (config.num_data, config.num_coding);
    if let Entry::Vacant(vacant) = reed_solomon_cache.entry(key) {
        let reed_solomon = ReedSolomon::new(config.num_data, config.num_coding).ok()?;
        let _ = vacant.insert(reed_solomon);
    }
    let reed_solomon = reed_solomon_cache.get(&key)?;
    if reed_solomon.reconstruct(shards).is_err() {
        return None;
    }

    let mut recovered_payloads = Vec::new();
    for (position, was_present) in data_present.iter().copied().enumerate() {
        if was_present {
            continue;
        }
        let Some(shard) = shards.get_mut(position).and_then(Option::take) else {
            continue;
        };
        let Ok(position_u32) = u32::try_from(position) else {
            continue;
        };
        let Some(index) = fec_set_index.checked_add(position_u32) else {
            continue;
        };
        let recovered =
            build_recovered_data_shred(&shard, &set.leader_signature, SIZE_OF_DATA_SHRED_PAYLOAD)?;
        if set.insert_recovered_data_shred(fec_set_index, index, shard) {
            recovered_payloads.push(recovered);
        }
    }

    Some(recovered_payloads)
}

fn build_recovered_data_shred(
    erasure_shard: &[u8],
    leader_signature: &[u8; SIZE_OF_SIGNATURE],
    payload_len: usize,
) -> Option<RecoveredDataPacket> {
    let mut recovered = vec![0_u8; payload_len];
    recovered
        .get_mut(..SIZE_OF_SIGNATURE)?
        .copy_from_slice(leader_signature);
    let start = SIZE_OF_SIGNATURE;
    let end = start.checked_add(erasure_shard.len())?;
    recovered
        .get_mut(start..end)?
        .copy_from_slice(erasure_shard);
    match parse_shred_header(&recovered) {
        Ok(ParsedShredHeader::Data(parsed)) => Some(RecoveredDataPacket {
            bytes: recovered,
            parsed,
        }),
        Ok(ParsedShredHeader::Code(_)) | Err(_) => None,
    }
}

pub(super) fn parse_packet_signature(packet: &[u8]) -> Option<[u8; SIZE_OF_SIGNATURE]> {
    let bytes = packet.get(..SIZE_OF_SIGNATURE)?;
    <[u8; SIZE_OF_SIGNATURE]>::try_from(bytes).ok()
}
