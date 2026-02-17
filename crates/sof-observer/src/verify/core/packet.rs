use crate::protocol::shred_wire::{
    OFFSET_SHRED_VARIANT, SHRED_KIND_MASK, SHRED_PROOF_SIZE_MASK, SIZE_OF_SIGNATURE,
    VARIANT_MERKLE_CODE, VARIANT_MERKLE_CODE_RESIGNED, VARIANT_MERKLE_DATA,
    VARIANT_MERKLE_DATA_RESIGNED,
};

use super::types::{ShredKind, Variant};

pub(super) fn parse_variant(packet: &[u8]) -> Option<Variant> {
    let byte = *packet.get(OFFSET_SHRED_VARIANT)?;
    let proof_size = byte & SHRED_PROOF_SIZE_MASK;
    let kind = match byte & SHRED_KIND_MASK {
        VARIANT_MERKLE_CODE => (ShredKind::Code, false),
        VARIANT_MERKLE_CODE_RESIGNED => (ShredKind::Code, true),
        VARIANT_MERKLE_DATA => (ShredKind::Data, false),
        VARIANT_MERKLE_DATA_RESIGNED => (ShredKind::Data, true),
        _ => return None,
    };
    Some(Variant {
        kind: kind.0,
        proof_size,
        resigned: kind.1,
    })
}

pub(super) fn parse_signature(shred: &[u8]) -> Option<[u8; SIZE_OF_SIGNATURE]> {
    let bytes = shred.get(..SIZE_OF_SIGNATURE)?;
    <[u8; SIZE_OF_SIGNATURE]>::try_from(bytes).ok()
}

pub(super) fn read_u16_le(packet: &[u8], offset: usize) -> Option<u16> {
    let bytes = packet.get(offset..offset.checked_add(2)?)?;
    let bytes: [u8; 2] = bytes.try_into().ok()?;
    Some(u16::from_le_bytes(bytes))
}

pub(super) fn read_u32_le(packet: &[u8], offset: usize) -> Option<u32> {
    let bytes = packet.get(offset..offset.checked_add(4)?)?;
    let bytes: [u8; 4] = bytes.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

pub(super) fn read_u64_le(packet: &[u8], offset: usize) -> Option<u64> {
    let bytes = packet.get(offset..offset.checked_add(8)?)?;
    let bytes: [u8; 8] = bytes.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}
