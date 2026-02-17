use super::*;
use crate::shred::wire::{
    ParsedShred, SIZE_OF_CODING_SHRED_HEADERS, SIZE_OF_CODING_SHRED_PAYLOAD,
    SIZE_OF_DATA_SHRED_PAYLOAD, parse_shred,
};

pub(super) fn recover_missing_data(
    set: &mut ErasureSet,
    fec_set_index: u32,
    reed_solomon_cache: &mut HashMap<(usize, usize), ReedSolomon>,
) -> Option<Vec<Vec<u8>>> {
    let config = set.config?;
    let variant = set.variant?;
    if config.num_data == 0 || config.num_coding == 0 {
        return None;
    }

    let shard_len = coding_erasure_shard_len(variant)?;
    let total = config.num_data.checked_add(config.num_coding)?;
    let mut shards: Vec<Option<Vec<u8>>> = vec![None; total];
    let mut present = 0_usize;
    let mut data_present = vec![false; config.num_data];

    for (&index, packet) in &set.data_shreds {
        let Some(position) = index.checked_sub(fec_set_index) else {
            continue;
        };
        let Ok(position) = usize::try_from(position) else {
            continue;
        };
        if position >= config.num_data {
            continue;
        }
        let Some(shard) = extract_data_erasure_shard(packet, shard_len) else {
            continue;
        };
        if shards.get(position).is_some_and(Option::is_none) {
            present = present.saturating_add(1);
        }
        if let Some(data_present_slot) = data_present.get_mut(position) {
            *data_present_slot = true;
        } else {
            continue;
        }
        if let Some(shard_slot) = shards.get_mut(position) {
            *shard_slot = Some(shard);
        } else {
            continue;
        }
    }

    for (&position, packet) in &set.coding_shreds {
        let position = usize::from(position);
        if position >= config.num_coding {
            continue;
        }
        let Some(shard) = extract_coding_erasure_shard(packet, shard_len) else {
            continue;
        };
        let Some(slot) = config.num_data.checked_add(position) else {
            continue;
        };
        if shards.get(slot).is_some_and(Option::is_none) {
            present = present.saturating_add(1);
        }
        if let Some(shard_slot) = shards.get_mut(slot) {
            *shard_slot = Some(shard);
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
    if reed_solomon.reconstruct(&mut shards).is_err() {
        return None;
    }

    let mut recovered_payloads = Vec::new();
    for (position, was_present) in data_present.into_iter().enumerate() {
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
        if let Entry::Vacant(vacant) = set.data_shreds.entry(index) {
            let _ = vacant.insert(recovered.clone());
            recovered_payloads.push(recovered);
        }
    }

    Some(recovered_payloads)
}

fn extract_data_erasure_shard(packet: &[u8], shard_len: usize) -> Option<Vec<u8>> {
    let start = SIZE_OF_SIGNATURE;
    let end = start.checked_add(shard_len)?;
    packet.get(start..end).map(ToOwned::to_owned)
}

fn extract_coding_erasure_shard(packet: &[u8], shard_len: usize) -> Option<Vec<u8>> {
    let start = SIZE_OF_CODING_SHRED_HEADERS;
    let end = start.checked_add(shard_len)?;
    packet.get(start..end).map(ToOwned::to_owned)
}

fn build_recovered_data_shred(
    erasure_shard: &[u8],
    leader_signature: &[u8; SIZE_OF_SIGNATURE],
    payload_len: usize,
) -> Option<Vec<u8>> {
    let mut recovered = vec![0_u8; payload_len];
    recovered
        .get_mut(..SIZE_OF_SIGNATURE)?
        .copy_from_slice(leader_signature);
    let start = SIZE_OF_SIGNATURE;
    let end = start.checked_add(erasure_shard.len())?;
    recovered
        .get_mut(start..end)?
        .copy_from_slice(erasure_shard);
    match parse_shred(&recovered) {
        Ok(ParsedShred::Data(_)) => Some(recovered),
        Ok(ParsedShred::Code(_)) | Err(_) => None,
    }
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
    SIZE_OF_CODING_SHRED_PAYLOAD.checked_sub(SIZE_OF_CODING_SHRED_HEADERS.checked_add(trailer)?)
}

pub(super) fn parse_packet_signature(packet: &[u8]) -> Option<[u8; SIZE_OF_SIGNATURE]> {
    let bytes = packet.get(..SIZE_OF_SIGNATURE)?;
    <[u8; SIZE_OF_SIGNATURE]>::try_from(bytes).ok()
}
