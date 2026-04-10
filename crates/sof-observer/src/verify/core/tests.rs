use std::{
    env,
    hint::black_box,
    time::{Duration, Instant},
};

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
        verifier.verify_packet(&packet, std::time::Instant::now(), false),
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
        verifier.verify_packet(&packet, std::time::Instant::now(), false),
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
        verifier.verify_packet(&packet, std::time::Instant::now(), false),
        VerifyStatus::Verified
    );

    verifier.set_known_pubkeys(Vec::new());
    verifier.set_slot_leaders([(43, [7_u8; 32])]);
    assert!(verifier.slot_leader_for_slot(42).is_none());

    assert_eq!(
        verifier.verify_packet(&packet, std::time::Instant::now(), false),
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
        verifier.verify_packet(&packet, now, false),
        VerifyStatus::UnknownLeader
    );

    packet[OFFSET_INDEX..OFFSET_INDEX + 4].copy_from_slice(&0_u32.to_le_bytes());
    packet[OFFSET_FEC_SET_INDEX..OFFSET_FEC_SET_INDEX + 4].copy_from_slice(&5_u32.to_le_bytes());
    assert_eq!(
        verifier.verify_packet(&packet, now, false),
        VerifyStatus::UnknownLeader
    );
}

#[test]
fn unknown_slot_retry_short_circuits_distinct_signatures_in_same_slot() {
    let mut first = build_data_packet(9, 1, 1);
    first[..SIZE_OF_SIGNATURE].copy_from_slice(&[1_u8; SIZE_OF_SIGNATURE]);
    let mut second = build_data_packet(9, 7, 7);
    second[..SIZE_OF_SIGNATURE].copy_from_slice(&[2_u8; SIZE_OF_SIGNATURE]);
    let mut verifier = ShredVerifier::new(1024, 256, Duration::from_secs(5));
    let now = std::time::Instant::now();

    assert_eq!(
        verifier.verify_packet(&first, now, false),
        VerifyStatus::UnknownLeader
    );

    second[OFFSET_INDEX..OFFSET_INDEX + 4].copy_from_slice(&0_u32.to_le_bytes());
    second[OFFSET_FEC_SET_INDEX..OFFSET_FEC_SET_INDEX + 4].copy_from_slice(&3_u32.to_le_bytes());
    assert_eq!(
        verifier.verify_packet(&second, now, false),
        VerifyStatus::UnknownLeader
    );
}

#[test]
#[ignore = "profiling fixture for unknown-slot verifier backoff"]
fn unknown_slot_retry_profile_fixture() {
    let iterations = env::var("SOF_VERIFY_UNKNOWN_SLOT_PROFILE_ITERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(200_000);
    let strict_unknown = env::var("SOF_VERIFY_STRICT_UNKNOWN_PROFILE")
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false);
    let now = Instant::now();
    let mut verifier = ShredVerifier::new(1024, 256, Duration::from_secs(5));
    let started_at = Instant::now();

    for i in 0..iterations {
        let mut packet = build_data_packet(11, u32::try_from(i & 0xffff).unwrap_or(u32::MAX), 11);
        packet[..SIZE_OF_SIGNATURE].fill((i & 0xff) as u8);
        let _ = verifier.verify_packet(&packet, now, strict_unknown);
    }

    println!(
        "iterations={} strict_unknown={} elapsed_us={}",
        iterations,
        strict_unknown,
        started_at.elapsed().as_micros()
    );
}

#[test]
#[ignore = "profiling fixture for strict-unknown verifier short-circuit"]
fn strict_unknown_known_pubkey_profile_fixture() {
    let iterations = env::var("SOF_VERIFY_STRICT_KNOWN_PUBKEY_PROFILE_ITERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(50_000);
    let strict_unknown = env::var("SOF_VERIFY_STRICT_UNKNOWN_PROFILE")
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false);
    let keypair = Keypair::new();
    let mut verifier = ShredVerifier::new(1024, 256, Duration::from_secs(5));
    verifier.set_known_pubkeys(vec![keypair.pubkey().to_bytes()]);
    let now = Instant::now();
    let started_at = Instant::now();

    for i in 0..iterations {
        let slot = 100_000_u64.saturating_add(u64::try_from(i).unwrap_or(u64::MAX));
        let mut packet = build_data_packet(slot, 1, 1);
        sign_data_packet(&mut packet, &keypair);
        let _ = verifier.verify_packet(&packet, now, strict_unknown);
    }

    println!(
        "iterations={} strict_unknown={} elapsed_us={}",
        iterations,
        strict_unknown,
        started_at.elapsed().as_micros()
    );
}

#[test]
#[ignore = "profiling fixture for verifier slot-state allocation churn"]
fn verifier_slot_state_allocation_profile_fixture() {
    let iterations = env::var("SOF_VERIFY_UNKNOWN_SLOT_PROFILE_ITERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(50_000);
    let now = Instant::now();
    let mut baseline = ShredVerifier::new_baseline(1024, 256, Duration::from_secs(5));
    let mut optimized = ShredVerifier::new(1024, 256, Duration::from_secs(5));

    let baseline_started = Instant::now();
    for i in 0..iterations {
        let slot = 500_000_u64.saturating_add(u64::try_from(i).unwrap_or(u64::MAX));
        let mut packet = build_data_packet(slot, 1, 1);
        packet[..SIZE_OF_SIGNATURE].fill((i & 0xff) as u8);
        black_box(baseline.verify_packet(&packet, now, true));
    }
    let baseline_elapsed = baseline_started.elapsed();

    let optimized_started = Instant::now();
    for i in 0..iterations {
        let slot = 500_000_u64.saturating_add(u64::try_from(i).unwrap_or(u64::MAX));
        let mut packet = build_data_packet(slot, 1, 1);
        packet[..SIZE_OF_SIGNATURE].fill((i & 0xff) as u8);
        black_box(optimized.verify_packet(&packet, now, true));
    }
    let optimized_elapsed = optimized_started.elapsed();

    println!(
        "verifier_slot_state_allocation_profile_fixture iterations={} baseline_us={} optimized_us={}",
        iterations,
        baseline_elapsed.as_micros(),
        optimized_elapsed.as_micros()
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

#[test]
fn strict_unknown_short_circuits_before_pubkey_probe() {
    let keypair = Keypair::new();
    let mut packet = build_data_packet(77, 1, 1);
    sign_data_packet(&mut packet, &keypair);

    let mut verifier = ShredVerifier::new(1024, 256, Duration::from_secs(2));
    verifier.set_known_pubkeys(vec![keypair.pubkey().to_bytes()]);

    assert_eq!(
        verifier.verify_packet(&packet, Instant::now(), true),
        VerifyStatus::UnknownLeader
    );
}
