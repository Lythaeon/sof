use crate::protocol::shred_wire::{
    OFFSET_CODE_NUM_CODING, OFFSET_CODE_NUM_DATA, OFFSET_CODE_POSITION, OFFSET_DATA_SIZE,
    OFFSET_FEC_SET_INDEX, OFFSET_FLAGS, OFFSET_INDEX, OFFSET_PARENT_OFFSET, OFFSET_SHRED_VARIANT,
    OFFSET_SLOT, OFFSET_VERSION, SIZE_OF_MERKLE_PROOF_ENTRY, SIZE_OF_MERKLE_ROOT,
    SIZE_OF_SIGNATURE,
};

use super::{
    ParseError, ParsedCodeShred, ParsedDataShred, ParsedDataShredHeader, ParsedShred,
    ParsedShredHeader, ShredType,
    types::{CodingHeader, CommonHeader, DataHeader, ShredVariant},
    util::{ensure_len, read_u8, read_u16_le, read_u32_le, read_u64_le},
};

#[inline]
pub fn parse_shred(packet: &[u8]) -> Result<ParsedShred, ParseError> {
    match parse_shred_header(packet)? {
        ParsedShredHeader::Data(data_header) => {
            let payload_end = data_header
                .payload_offset
                .saturating_add(data_header.payload_len);
            let payload = packet
                .get(data_header.payload_offset..payload_end)
                .ok_or(ParseError::PacketTooShort {
                    actual: packet.len(),
                    minimum: payload_end,
                })?
                .to_vec();
            Ok(ParsedShred::Data(ParsedDataShred {
                common: data_header.common,
                data_header: data_header.data_header,
                payload,
            }))
        }
        ParsedShredHeader::Code(code) => Ok(ParsedShred::Code(code)),
    }
}

#[inline]
pub fn parse_shred_header(packet: &[u8]) -> Result<ParsedShredHeader, ParseError> {
    ensure_len(packet, super::SIZE_OF_COMMON_SHRED_HEADER)?;
    let variant = ShredVariant::parse(read_u8(packet, OFFSET_SHRED_VARIANT)?)?;
    let common = CommonHeader {
        shred_variant: variant,
        slot: read_u64_le(packet, OFFSET_SLOT)?,
        index: read_u32_le(packet, OFFSET_INDEX)?,
        version: read_u16_le(packet, OFFSET_VERSION)?,
        fec_set_index: read_u32_le(packet, OFFSET_FEC_SET_INDEX)?,
    };
    match variant.shred_type {
        ShredType::Data => parse_data_shred_header(packet, common).map(ParsedShredHeader::Data),
        ShredType::Code => parse_code_shred(packet, common).map(ParsedShredHeader::Code),
    }
}

#[inline]
fn parse_data_shred_header(
    packet: &[u8],
    common: CommonHeader,
) -> Result<ParsedDataShredHeader, ParseError> {
    ensure_len(packet, super::SIZE_OF_DATA_SHRED_PAYLOAD)?;

    let size = read_u16_le(packet, OFFSET_DATA_SIZE)?;
    let declared_size = usize::from(size);
    let max_size =
        max_data_shred_size(common.shred_variant).ok_or(ParseError::InvalidDataSize(size))?;
    if declared_size < super::SIZE_OF_DATA_SHRED_HEADERS || declared_size > max_size {
        return Err(ParseError::InvalidDataSize(size));
    }
    let flags = read_u8(packet, OFFSET_FLAGS)?;
    let payload_offset = super::SIZE_OF_DATA_SHRED_HEADERS;
    let payload_len = declared_size.saturating_sub(payload_offset);
    if packet.get(payload_offset..declared_size).is_none() {
        return Err(ParseError::PacketTooShort {
            actual: packet.len(),
            minimum: declared_size,
        });
    }

    Ok(ParsedDataShredHeader {
        common,
        data_header: DataHeader {
            parent_offset: read_u16_le(packet, OFFSET_PARENT_OFFSET)?,
            flags,
            size,
        },
        payload_offset,
        payload_len,
    })
}

#[inline]
fn parse_code_shred(packet: &[u8], common: CommonHeader) -> Result<ParsedCodeShred, ParseError> {
    ensure_len(packet, super::SIZE_OF_CODING_SHRED_PAYLOAD)?;
    let num_data_shreds = read_u16_le(packet, OFFSET_CODE_NUM_DATA)?;
    let num_coding_shreds = read_u16_le(packet, OFFSET_CODE_NUM_CODING)?;
    let position = read_u16_le(packet, OFFSET_CODE_POSITION)?;
    if num_data_shreds == 0 || num_coding_shreds == 0 || position >= num_coding_shreds {
        return Err(ParseError::InvalidCodingHeader {
            num_data_shreds,
            num_coding_shreds,
            position,
        });
    }
    Ok(ParsedCodeShred {
        common,
        coding_header: CodingHeader {
            num_data_shreds,
            num_coding_shreds,
            position,
        },
    })
}

fn max_data_shred_size(variant: ShredVariant) -> Option<usize> {
    if variant.shred_type != ShredType::Data {
        return None;
    }
    let proof = usize::from(variant.proof_size);
    let trailer = SIZE_OF_MERKLE_ROOT
        .checked_add(proof.checked_mul(SIZE_OF_MERKLE_PROOF_ENTRY)?)?
        .checked_add(if variant.resigned {
            SIZE_OF_SIGNATURE
        } else {
            0
        })?;
    let capacity = super::SIZE_OF_DATA_SHRED_PAYLOAD
        .checked_sub(super::SIZE_OF_DATA_SHRED_HEADERS.checked_add(trailer)?)?;
    super::SIZE_OF_DATA_SHRED_HEADERS.checked_add(capacity)
}
