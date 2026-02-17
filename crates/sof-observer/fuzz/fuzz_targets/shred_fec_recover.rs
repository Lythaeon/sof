#![no_main]

use libfuzzer_sys::fuzz_target;
use sof::{
    protocol::shred_wire::{
        DATA_COMPLETE_SHRED_MASK, LAST_SHRED_IN_SLOT_MASK, OFFSET_CODE_NUM_CODING,
        OFFSET_CODE_NUM_DATA, OFFSET_CODE_POSITION, OFFSET_DATA_SIZE, OFFSET_FEC_SET_INDEX,
        OFFSET_FLAGS, OFFSET_INDEX, OFFSET_PARENT_OFFSET, OFFSET_SHRED_VARIANT, OFFSET_SLOT,
        OFFSET_VERSION, SHRED_PROOF_SIZE_MASK, SIZE_OF_CODING_SHRED_PAYLOAD,
        SIZE_OF_DATA_SHRED_HEADERS, SIZE_OF_DATA_SHRED_PAYLOAD, SIZE_OF_MERKLE_PROOF_ENTRY,
        SIZE_OF_MERKLE_ROOT, SIZE_OF_SIGNATURE, VARIANT_MERKLE_CODE, VARIANT_MERKLE_CODE_RESIGNED,
        VARIANT_MERKLE_DATA, VARIANT_MERKLE_DATA_RESIGNED,
    },
    shred::{
        fec::FecRecoverer,
        wire::{ParsedShred, parse_shred},
    },
};

fn take_bytes<'a>(input: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    if input.len() < len {
        return None;
    }
    let (head, tail) = input.split_at(len);
    *input = tail;
    Some(head)
}

fn take_u8(input: &mut &[u8]) -> Option<u8> {
    take_bytes(input, 1).map(|bytes| bytes[0])
}

fn take_u16(input: &mut &[u8]) -> Option<u16> {
    let bytes = take_bytes(input, 2)?;
    let bytes: [u8; 2] = bytes.try_into().ok()?;
    Some(u16::from_le_bytes(bytes))
}

fn take_signature(input: &mut &[u8]) -> [u8; SIZE_OF_SIGNATURE] {
    let mut signature = [0_u8; SIZE_OF_SIGNATURE];
    let available = input.len().min(SIZE_OF_SIGNATURE);
    if available > 0 {
        if let Some(bytes) = take_bytes(input, available) {
            signature[..available].copy_from_slice(bytes);
        }
    }
    signature
}

fn max_data_payload_len(proof_size: u8, resigned: bool) -> usize {
    let proof = usize::from(proof_size);
    let trailer = SIZE_OF_MERKLE_ROOT
        .saturating_add(proof.saturating_mul(SIZE_OF_MERKLE_PROOF_ENTRY))
        .saturating_add(if resigned { SIZE_OF_SIGNATURE } else { 0 });
    SIZE_OF_DATA_SHRED_PAYLOAD
        .saturating_sub(SIZE_OF_DATA_SHRED_HEADERS)
        .saturating_sub(trailer)
}

fn build_data_packet(
    signature: [u8; SIZE_OF_SIGNATURE],
    slot: u64,
    index: u32,
    fec_set_index: u32,
    proof_size: u8,
    resigned: bool,
    parent_offset: u16,
    data_complete: bool,
    last_in_slot: bool,
    payload: &[u8],
) -> Vec<u8> {
    let mut packet = vec![0_u8; SIZE_OF_DATA_SHRED_PAYLOAD];
    packet[..SIZE_OF_SIGNATURE].copy_from_slice(&signature);

    let variant_base = if resigned {
        VARIANT_MERKLE_DATA_RESIGNED
    } else {
        VARIANT_MERKLE_DATA
    };
    packet[OFFSET_SHRED_VARIANT] = variant_base | (proof_size & SHRED_PROOF_SIZE_MASK);
    packet[OFFSET_SLOT..OFFSET_SLOT + 8].copy_from_slice(&slot.to_le_bytes());
    packet[OFFSET_INDEX..OFFSET_INDEX + 4].copy_from_slice(&index.to_le_bytes());
    packet[OFFSET_VERSION..OFFSET_VERSION + 2].copy_from_slice(&1_u16.to_le_bytes());
    packet[OFFSET_FEC_SET_INDEX..OFFSET_FEC_SET_INDEX + 4]
        .copy_from_slice(&fec_set_index.to_le_bytes());
    packet[OFFSET_PARENT_OFFSET..OFFSET_PARENT_OFFSET + 2]
        .copy_from_slice(&parent_offset.to_le_bytes());

    let mut flags = 0_u8;
    if data_complete {
        flags |= DATA_COMPLETE_SHRED_MASK;
    }
    if last_in_slot {
        flags |= LAST_SHRED_IN_SLOT_MASK;
    }
    packet[OFFSET_FLAGS] = flags;

    let max_payload_len = max_data_payload_len(proof_size, resigned);
    let payload_len = payload.len().min(max_payload_len);
    let declared_size = SIZE_OF_DATA_SHRED_HEADERS.saturating_add(payload_len);
    let declared_size_u16 = u16::try_from(declared_size).unwrap_or(u16::MAX);
    packet[OFFSET_DATA_SIZE..OFFSET_DATA_SIZE + 2]
        .copy_from_slice(&declared_size_u16.to_le_bytes());
    packet[SIZE_OF_DATA_SHRED_HEADERS..SIZE_OF_DATA_SHRED_HEADERS + payload_len]
        .copy_from_slice(&payload[..payload_len]);

    packet
}

fn build_code_packet(
    signature: [u8; SIZE_OF_SIGNATURE],
    slot: u64,
    index: u32,
    fec_set_index: u32,
    proof_size: u8,
    resigned: bool,
    num_data_shreds: u16,
    num_coding_shreds: u16,
    position: u16,
) -> Vec<u8> {
    let mut packet = vec![0_u8; SIZE_OF_CODING_SHRED_PAYLOAD];
    packet[..SIZE_OF_SIGNATURE].copy_from_slice(&signature);

    let variant_base = if resigned {
        VARIANT_MERKLE_CODE_RESIGNED
    } else {
        VARIANT_MERKLE_CODE
    };
    packet[OFFSET_SHRED_VARIANT] = variant_base | (proof_size & SHRED_PROOF_SIZE_MASK);
    packet[OFFSET_SLOT..OFFSET_SLOT + 8].copy_from_slice(&slot.to_le_bytes());
    packet[OFFSET_INDEX..OFFSET_INDEX + 4].copy_from_slice(&index.to_le_bytes());
    packet[OFFSET_VERSION..OFFSET_VERSION + 2].copy_from_slice(&1_u16.to_le_bytes());
    packet[OFFSET_FEC_SET_INDEX..OFFSET_FEC_SET_INDEX + 4]
        .copy_from_slice(&fec_set_index.to_le_bytes());
    packet[OFFSET_CODE_NUM_DATA..OFFSET_CODE_NUM_DATA + 2]
        .copy_from_slice(&num_data_shreds.to_le_bytes());
    packet[OFFSET_CODE_NUM_CODING..OFFSET_CODE_NUM_CODING + 2]
        .copy_from_slice(&num_coding_shreds.to_le_bytes());
    packet[OFFSET_CODE_POSITION..OFFSET_CODE_POSITION + 2].copy_from_slice(&position.to_le_bytes());

    packet
}

fuzz_target!(|bytes: &[u8]| {
    let mut input = bytes;

    let max_tracked_sets = usize::from(take_u8(&mut input).unwrap_or(16) % 64).max(1);
    let op_count = usize::from(take_u8(&mut input).unwrap_or(0));
    let mut recoverer = FecRecoverer::new(max_tracked_sets);

    for _ in 0..op_count {
        let Some(mode) = take_u8(&mut input) else {
            break;
        };

        let packet = if mode & 1 == 0 {
            let Some(raw_len) = take_u8(&mut input).map(usize::from) else {
                break;
            };
            let Some(raw) = take_bytes(&mut input, raw_len) else {
                break;
            };
            raw.to_vec()
        } else {
            let signature = take_signature(&mut input);
            let slot = u64::from(take_u16(&mut input).unwrap_or(0));
            let index = u32::from(take_u16(&mut input).unwrap_or(0));
            let fec_set_index = u32::from(take_u16(&mut input).unwrap_or(0));
            let proof_size = take_u8(&mut input).unwrap_or(0) & SHRED_PROOF_SIZE_MASK;
            let resigned = mode & 4 == 4;

            if mode & 2 == 0 {
                let parent_offset = take_u16(&mut input).unwrap_or(0);
                let flags = take_u8(&mut input).unwrap_or(0);
                let data_complete = flags & 1 == 1;
                let last_in_slot = flags & 2 == 2;
                let Some(payload_len) = take_u8(&mut input).map(usize::from) else {
                    break;
                };
                let Some(payload) = take_bytes(&mut input, payload_len) else {
                    break;
                };
                build_data_packet(
                    signature,
                    slot,
                    index,
                    fec_set_index,
                    proof_size,
                    resigned,
                    parent_offset,
                    data_complete,
                    last_in_slot,
                    payload,
                )
            } else {
                let num_data_shreds = u16::from(take_u8(&mut input).unwrap_or(0)).saturating_add(1);
                let num_coding_shreds =
                    u16::from(take_u8(&mut input).unwrap_or(0)).saturating_add(1);
                let raw_position = u16::from(take_u8(&mut input).unwrap_or(0));
                let position = if num_coding_shreds == 0 {
                    0
                } else {
                    raw_position % num_coding_shreds
                };
                build_code_packet(
                    signature,
                    slot,
                    index,
                    fec_set_index,
                    proof_size,
                    resigned,
                    num_data_shreds,
                    num_coding_shreds,
                    position,
                )
            }
        };

        let recovered = recoverer.ingest_packet(&packet);
        assert!(recoverer.tracked_sets() <= max_tracked_sets);

        for recovered_packet in recovered {
            assert_eq!(recovered_packet.len(), SIZE_OF_DATA_SHRED_PAYLOAD);
            let parsed = parse_shred(&recovered_packet);
            assert!(matches!(parsed, Ok(ParsedShred::Data(_))));
        }
    }
});
