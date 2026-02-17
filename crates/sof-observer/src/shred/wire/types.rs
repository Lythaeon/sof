use std::fmt;

use thiserror::Error;

use crate::protocol::shred_wire::{
    DATA_COMPLETE_SHRED_MASK, LAST_SHRED_IN_SLOT_MASK, SHRED_KIND_MASK, SHRED_PROOF_SIZE_MASK,
    SHRED_TICK_REFERENCE_MASK, VARIANT_MERKLE_CODE, VARIANT_MERKLE_CODE_RESIGNED,
    VARIANT_MERKLE_DATA, VARIANT_MERKLE_DATA_RESIGNED,
};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ShredType {
    Data,
    Code,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ShredVariant {
    pub shred_type: ShredType,
    pub proof_size: u8,
    pub resigned: bool,
}

impl ShredVariant {
    pub(super) const fn parse(byte: u8) -> Result<Self, ParseError> {
        let proof_size = byte & SHRED_PROOF_SIZE_MASK;
        let kind = byte & SHRED_KIND_MASK;
        match kind {
            VARIANT_MERKLE_CODE => Ok(Self {
                shred_type: ShredType::Code,
                proof_size,
                resigned: false,
            }),
            VARIANT_MERKLE_CODE_RESIGNED => Ok(Self {
                shred_type: ShredType::Code,
                proof_size,
                resigned: true,
            }),
            VARIANT_MERKLE_DATA => Ok(Self {
                shred_type: ShredType::Data,
                proof_size,
                resigned: false,
            }),
            VARIANT_MERKLE_DATA_RESIGNED => Ok(Self {
                shred_type: ShredType::Data,
                proof_size,
                resigned: true,
            }),
            _ => Err(ParseError::InvalidShredVariant(byte)),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct CommonHeader {
    pub shred_variant: ShredVariant,
    pub slot: u64,
    pub index: u32,
    pub version: u16,
    pub fec_set_index: u32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DataHeader {
    pub parent_offset: u16,
    pub flags: u8,
    pub size: u16,
}

impl DataHeader {
    #[must_use]
    pub const fn reference_tick(self) -> u8 {
        self.flags & SHRED_TICK_REFERENCE_MASK
    }

    #[must_use]
    pub const fn data_complete(self) -> bool {
        self.flags & DATA_COMPLETE_SHRED_MASK == DATA_COMPLETE_SHRED_MASK
    }

    #[must_use]
    pub const fn last_in_slot(self) -> bool {
        self.flags & LAST_SHRED_IN_SLOT_MASK == LAST_SHRED_IN_SLOT_MASK
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct CodingHeader {
    pub num_data_shreds: u16,
    pub num_coding_shreds: u16,
    pub position: u16,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ParsedShred {
    Data(ParsedDataShred),
    Code(ParsedCodeShred),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ParsedShredHeader {
    Data(ParsedDataShredHeader),
    Code(ParsedCodeShred),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ParsedDataShred {
    pub common: CommonHeader,
    pub data_header: DataHeader,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ParsedDataShredHeader {
    pub common: CommonHeader,
    pub data_header: DataHeader,
    pub payload_offset: usize,
    pub payload_len: usize,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ParsedCodeShred {
    pub common: CommonHeader,
    pub coding_header: CodingHeader,
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum ParseError {
    #[error("packet too short: {actual} bytes (expected at least {minimum})")]
    PacketTooShort { actual: usize, minimum: usize },
    #[error("invalid shred variant: 0x{0:02x}")]
    InvalidShredVariant(u8),
    #[error("invalid data shred size: {0}")]
    InvalidDataSize(u16),
    #[error(
        "invalid coding header: num_data={num_data_shreds} num_coding={num_coding_shreds} position={position}"
    )]
    InvalidCodingHeader {
        num_data_shreds: u16,
        num_coding_shreds: u16,
        position: u16,
    },
}

impl fmt::Display for ParsedShred {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Data(data) => write!(
                f,
                "data(slot={}, index={}, fec_set={}, payload_len={}, flags=0x{:02x})",
                data.common.slot,
                data.common.index,
                data.common.fec_set_index,
                data.payload.len(),
                data.data_header.flags
            ),
            Self::Code(code) => write!(
                f,
                "code(slot={}, index={}, fec_set={}, num_data={}, num_code={}, pos={})",
                code.common.slot,
                code.common.index,
                code.common.fec_set_index,
                code.coding_header.num_data_shreds,
                code.coding_header.num_coding_shreds,
                code.coding_header.position
            ),
        }
    }
}
