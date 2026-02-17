/// Serialized Ed25519 signature length in Solana shred packets.
pub const SIZE_OF_SIGNATURE: usize = 64;
/// Merkle root length used in shred trailer.
pub const SIZE_OF_MERKLE_ROOT: usize = 32;
/// Truncated proof node size used by Solana shred merkle proofs.
pub const SIZE_OF_MERKLE_PROOF_ENTRY: usize = 20;

/// Common shred header byte length.
pub const SIZE_OF_COMMON_SHRED_HEADER: usize = 83;
/// Data shred headers byte length.
pub const SIZE_OF_DATA_SHRED_HEADERS: usize = 88;
/// Coding shred headers byte length.
pub const SIZE_OF_CODING_SHRED_HEADERS: usize = 89;
/// Fixed serialized data shred packet length.
pub const SIZE_OF_DATA_SHRED_PAYLOAD: usize = 1203;
/// Fixed serialized coding shred packet length.
pub const SIZE_OF_CODING_SHRED_PAYLOAD: usize = 1228;

/// Shred variant field byte offset.
pub const OFFSET_SHRED_VARIANT: usize = 64;
/// Slot field byte offset.
pub const OFFSET_SLOT: usize = 65;
/// Shred index field byte offset.
pub const OFFSET_INDEX: usize = 73;
/// Shred version field byte offset.
pub const OFFSET_VERSION: usize = 77;
/// FEC set index field byte offset.
pub const OFFSET_FEC_SET_INDEX: usize = 79;
/// Data shred parent offset field byte offset.
pub const OFFSET_PARENT_OFFSET: usize = 83;
/// Data shred flags byte offset.
pub const OFFSET_FLAGS: usize = 85;
/// Data shred size field byte offset.
pub const OFFSET_DATA_SIZE: usize = 86;
/// Coding shred num-data field byte offset.
pub const OFFSET_CODE_NUM_DATA: usize = 83;
/// Coding shred num-coding field byte offset.
pub const OFFSET_CODE_NUM_CODING: usize = 85;
/// Coding shred position field byte offset.
pub const OFFSET_CODE_POSITION: usize = 87;

/// Shred-variant low nibble mask for merkle proof length.
pub const SHRED_PROOF_SIZE_MASK: u8 = 0x0F;
/// Shred-variant high nibble mask for data/code/resigned class.
pub const SHRED_KIND_MASK: u8 = 0xF0;
/// Variant nibble for merkle code shred.
pub const VARIANT_MERKLE_CODE: u8 = 0x60;
/// Variant nibble for merkle code shred with resigned root.
pub const VARIANT_MERKLE_CODE_RESIGNED: u8 = 0x70;
/// Variant nibble for merkle data shred.
pub const VARIANT_MERKLE_DATA: u8 = 0x90;
/// Variant nibble for merkle data shred with resigned root.
pub const VARIANT_MERKLE_DATA_RESIGNED: u8 = 0xB0;

/// Bit mask for data shred reference tick in flags.
pub const SHRED_TICK_REFERENCE_MASK: u8 = 0b0011_1111;
/// Bit mask signaling `DATA_COMPLETE_SHRED`.
pub const DATA_COMPLETE_SHRED_MASK: u8 = 0b0100_0000;
/// Bit mask signaling `LAST_SHRED_IN_SLOT`.
pub const LAST_SHRED_IN_SLOT_MASK: u8 = 0b1100_0000;
