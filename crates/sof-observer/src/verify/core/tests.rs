use std::time::Duration;

use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::protocol::shred_wire::OFFSET_DATA_SIZE;

use super::{
    ShredVerifier, VerifyStatus,
    merkle::compute_merkle_root,
    packet::{parse_signature, parse_variant},
};
use crate::protocol::shred_wire::{
    OFFSET_FEC_SET_INDEX, OFFSET_INDEX, OFFSET_SHRED_VARIANT, OFFSET_SLOT,
    SIZE_OF_DATA_SHRED_HEADERS, SIZE_OF_DATA_SHRED_PAYLOAD, SIZE_OF_SIGNATURE, VARIANT_MERKLE_DATA,
};

#[test]
fn verifies_signature_for_known_pubkey() {
    let mut packet = vec![0_u8; SIZE_OF_DATA_SHRED_PAYLOAD];
    packet[OFFSET_SHRED_VARIANT] = VARIANT_MERKLE_DATA; // data, proof_size=0
    packet[OFFSET_SLOT..OFFSET_SLOT + 8].copy_from_slice(&42_u64.to_le_bytes());
    packet[OFFSET_INDEX..OFFSET_INDEX + 4].copy_from_slice(&0_u32.to_le_bytes());
    packet[OFFSET_FEC_SET_INDEX..OFFSET_FEC_SET_INDEX + 4].copy_from_slice(&0_u32.to_le_bytes());
    packet[OFFSET_DATA_SIZE..OFFSET_DATA_SIZE + 2]
        .copy_from_slice(&(SIZE_OF_DATA_SHRED_HEADERS as u16).to_le_bytes());

    let variant = parse_variant(&packet).expect("variant should parse");
    let root = compute_merkle_root(&packet, variant, 0, 0).expect("root should compute");
    let keypair = Keypair::new();
    let signature = keypair.sign_message(&root);
    packet[..SIZE_OF_SIGNATURE].copy_from_slice(signature.as_ref());

    let mut verifier = ShredVerifier::new(1024, 256, Duration::from_secs(2));
    verifier.set_known_pubkeys(vec![keypair.pubkey().to_bytes()]);

    assert_eq!(
        verifier.verify_packet(&packet, std::time::Instant::now()),
        VerifyStatus::Verified
    );
}

#[test]
fn marks_unknown_without_matching_pubkey() {
    let mut packet = vec![0_u8; SIZE_OF_DATA_SHRED_PAYLOAD];
    packet[OFFSET_SHRED_VARIANT] = VARIANT_MERKLE_DATA; // data, proof_size=0
    packet[OFFSET_SLOT..OFFSET_SLOT + 8].copy_from_slice(&7_u64.to_le_bytes());
    packet[OFFSET_INDEX..OFFSET_INDEX + 4].copy_from_slice(&0_u32.to_le_bytes());
    packet[OFFSET_FEC_SET_INDEX..OFFSET_FEC_SET_INDEX + 4].copy_from_slice(&0_u32.to_le_bytes());
    packet[OFFSET_DATA_SIZE..OFFSET_DATA_SIZE + 2]
        .copy_from_slice(&(SIZE_OF_DATA_SHRED_HEADERS as u16).to_le_bytes());
    packet[..SIZE_OF_SIGNATURE].copy_from_slice(&[1_u8; SIZE_OF_SIGNATURE]);

    assert!(parse_signature(&packet).is_some());
    let mut verifier = ShredVerifier::new(1024, 256, Duration::from_secs(2));
    assert_eq!(
        verifier.verify_packet(&packet, std::time::Instant::now()),
        VerifyStatus::UnknownLeader
    );
}
