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

#[test]
fn reuses_cached_verified_signature_without_known_pubkey_or_slot_leader() {
    let keypair = Keypair::new();
    let mut packet = build_data_packet(42, 0, 0);
    sign_data_packet(&mut packet, &keypair);

    let mut verifier = ShredVerifier::new(1024, 0, Duration::from_secs(2));
    verifier.set_known_pubkeys(vec![keypair.pubkey().to_bytes()]);
    assert_eq!(
        verifier.verify_packet(&packet, std::time::Instant::now()),
        VerifyStatus::Verified
    );

    verifier.set_known_pubkeys(Vec::new());
    verifier.set_slot_leaders([(43, [7_u8; 32])]);
    assert!(verifier.slot_leader_for_slot(42).is_none());

    assert_eq!(
        verifier.verify_packet(&packet, std::time::Instant::now()),
        VerifyStatus::Verified
    );
}

#[test]
fn unknown_retry_short_circuits_before_merkle_validation() {
    let mut packet = build_data_packet(7, 2, 2);
    packet[..SIZE_OF_SIGNATURE].copy_from_slice(&[1_u8; SIZE_OF_SIGNATURE]);
    let mut verifier = ShredVerifier::new(1024, 256, Duration::from_secs(5));
    let now = std::time::Instant::now();

    assert_eq!(
        verifier.verify_packet(&packet, now),
        VerifyStatus::UnknownLeader
    );

    packet[OFFSET_INDEX..OFFSET_INDEX + 4].copy_from_slice(&0_u32.to_le_bytes());
    packet[OFFSET_FEC_SET_INDEX..OFFSET_FEC_SET_INDEX + 4].copy_from_slice(&5_u32.to_le_bytes());
    assert_eq!(
        verifier.verify_packet(&packet, now),
        VerifyStatus::UnknownLeader
    );
}

fn build_data_packet(slot: u64, index: u32, fec_set_index: u32) -> Vec<u8> {
    let mut packet = vec![0_u8; SIZE_OF_DATA_SHRED_PAYLOAD];
    packet[OFFSET_SHRED_VARIANT] = VARIANT_MERKLE_DATA; // data, proof_size=0
    packet[OFFSET_SLOT..OFFSET_SLOT + 8].copy_from_slice(&slot.to_le_bytes());
    packet[OFFSET_INDEX..OFFSET_INDEX + 4].copy_from_slice(&index.to_le_bytes());
    packet[OFFSET_FEC_SET_INDEX..OFFSET_FEC_SET_INDEX + 4]
        .copy_from_slice(&fec_set_index.to_le_bytes());
    packet[OFFSET_DATA_SIZE..OFFSET_DATA_SIZE + 2]
        .copy_from_slice(&(SIZE_OF_DATA_SHRED_HEADERS as u16).to_le_bytes());
    packet
}

fn sign_data_packet(packet: &mut [u8], keypair: &Keypair) {
    let variant = parse_variant(packet).expect("variant should parse");
    let index = u32::from_le_bytes(
        packet[OFFSET_INDEX..OFFSET_INDEX + 4]
            .try_into()
            .expect("index bytes should exist"),
    );
    let fec_set_index = u32::from_le_bytes(
        packet[OFFSET_FEC_SET_INDEX..OFFSET_FEC_SET_INDEX + 4]
            .try_into()
            .expect("fec set bytes should exist"),
    );
    let root =
        compute_merkle_root(packet, variant, index, fec_set_index).expect("root should compute");
    let signature = keypair.sign_message(&root);
    packet[..SIZE_OF_SIGNATURE].copy_from_slice(signature.as_ref());
}
