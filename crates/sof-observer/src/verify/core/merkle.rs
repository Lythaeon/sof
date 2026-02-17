use sha2::{Digest, Sha256};
use solana_signature::Signature;

use crate::protocol::shred_wire::{
    OFFSET_CODE_NUM_DATA, OFFSET_CODE_POSITION, SIZE_OF_CODING_SHRED_HEADERS,
    SIZE_OF_CODING_SHRED_PAYLOAD, SIZE_OF_DATA_SHRED_HEADERS, SIZE_OF_DATA_SHRED_PAYLOAD,
    SIZE_OF_MERKLE_PROOF_ENTRY, SIZE_OF_MERKLE_ROOT, SIZE_OF_SIGNATURE,
};

use super::{
    packet::read_u16_le,
    types::{ShredKind, Variant},
};

const MERKLE_HASH_PREFIX_LEAF: &[u8] = b"\x00SOLANA_MERKLE_SHREDS_LEAF";
const MERKLE_HASH_PREFIX_NODE: &[u8] = b"\x01SOLANA_MERKLE_SHREDS_NODE";

pub(super) fn verify_signature(
    signature: [u8; SIZE_OF_SIGNATURE],
    pubkey: [u8; 32],
    merkle_root: [u8; SIZE_OF_MERKLE_ROOT],
) -> bool {
    let signature = Signature::from(signature);
    signature.verify(&pubkey, &merkle_root)
}

pub(super) fn compute_merkle_root(
    shred: &[u8],
    variant: Variant,
    index: u32,
    fec_set_index: u32,
) -> Option<[u8; SIZE_OF_MERKLE_ROOT]> {
    let proof_offset = proof_offset(variant)?;
    let proof_size = usize::from(variant.proof_size);
    let proof_bytes = shred.get(
        proof_offset
            ..proof_offset.checked_add(proof_size.checked_mul(SIZE_OF_MERKLE_PROOF_ENTRY)?)?,
    )?;
    let leaf_bytes = shred.get(SIZE_OF_SIGNATURE..proof_offset)?;
    let mut root = hashv(&[MERKLE_HASH_PREFIX_LEAF, leaf_bytes]);

    let mut shard_index = match variant.kind {
        ShredKind::Data => usize::try_from(index.checked_sub(fec_set_index)?).ok()?,
        ShredKind::Code => {
            let num_data = usize::from(read_u16_le(shred, OFFSET_CODE_NUM_DATA)?);
            let position = usize::from(read_u16_le(shred, OFFSET_CODE_POSITION)?);
            num_data.checked_add(position)?
        }
    };

    for entry_bytes in proof_bytes.chunks(SIZE_OF_MERKLE_PROOF_ENTRY) {
        let entry: [u8; SIZE_OF_MERKLE_PROOF_ENTRY] = entry_bytes.try_into().ok()?;
        root = if shard_index % 2 == 0 {
            hashv(&[
                MERKLE_HASH_PREFIX_NODE,
                &root[..SIZE_OF_MERKLE_PROOF_ENTRY],
                &entry,
            ])
        } else {
            hashv(&[
                MERKLE_HASH_PREFIX_NODE,
                &entry,
                &root[..SIZE_OF_MERKLE_PROOF_ENTRY],
            ])
        };
        shard_index >>= 1;
    }
    (shard_index == 0).then_some(root)
}

fn proof_offset(variant: Variant) -> Option<usize> {
    let proof = usize::from(variant.proof_size);
    let proof_bytes = proof.checked_mul(SIZE_OF_MERKLE_PROOF_ENTRY)?;
    let resigned = if variant.resigned {
        SIZE_OF_SIGNATURE
    } else {
        0
    };
    let headers = match variant.kind {
        ShredKind::Data => SIZE_OF_DATA_SHRED_HEADERS,
        ShredKind::Code => SIZE_OF_CODING_SHRED_HEADERS,
    };
    let payload_size = match variant.kind {
        ShredKind::Data => SIZE_OF_DATA_SHRED_PAYLOAD,
        ShredKind::Code => SIZE_OF_CODING_SHRED_PAYLOAD,
    };
    let capacity = payload_size.checked_sub(
        headers
            .checked_add(SIZE_OF_MERKLE_ROOT)?
            .checked_add(proof_bytes)?
            .checked_add(resigned)?,
    )?;
    headers
        .checked_add(capacity)?
        .checked_add(SIZE_OF_MERKLE_ROOT)
}

fn hashv(parts: &[&[u8]]) -> [u8; SIZE_OF_MERKLE_ROOT] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}
