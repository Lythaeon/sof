mod parser;
mod types;
mod util;

#[cfg(test)]
mod tests;

pub use parser::{parse_shred, parse_shred_header};
pub use types::{
    CodingHeader, CommonHeader, DataHeader, ParseError, ParsedCodeShred, ParsedDataShred,
    ParsedDataShredHeader, ParsedShred, ParsedShredHeader, ShredType, ShredVariant,
};

pub use crate::protocol::shred_wire::{
    SIZE_OF_CODING_SHRED_HEADERS, SIZE_OF_CODING_SHRED_PAYLOAD, SIZE_OF_COMMON_SHRED_HEADER,
    SIZE_OF_DATA_SHRED_HEADERS, SIZE_OF_DATA_SHRED_PAYLOAD,
};
