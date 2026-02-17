#![no_main]

use std::time::{Duration, Instant};

use libfuzzer_sys::fuzz_target;
use sof::verify::ShredVerifier;

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

fn take_u64(input: &mut &[u8]) -> Option<u64> {
    let bytes = take_bytes(input, 8)?;
    let bytes: [u8; 8] = bytes.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

fn take_pubkey(input: &mut &[u8]) -> Option<[u8; 32]> {
    let bytes = take_bytes(input, 32)?;
    bytes.try_into().ok()
}

fuzz_target!(|bytes: &[u8]| {
    let mut input = bytes;

    let signature_cache_capacity = usize::from(take_u16(&mut input).unwrap_or(64)).max(1);
    let slot_leader_window = take_u64(&mut input).unwrap_or(512).max(1);
    let unknown_retry_ms = u64::from(take_u16(&mut input).unwrap_or(1)).max(1);

    let mut verifier = ShredVerifier::new(
        signature_cache_capacity,
        slot_leader_window,
        Duration::from_millis(unknown_retry_ms),
    );

    let known_count = usize::from(take_u8(&mut input).unwrap_or(0) % 16);
    let mut known_pubkeys = Vec::with_capacity(known_count);
    for _ in 0..known_count {
        let Some(pubkey) = take_pubkey(&mut input) else {
            break;
        };
        known_pubkeys.push(pubkey);
    }
    verifier.set_known_pubkeys(known_pubkeys);

    let slot_leader_count = usize::from(take_u8(&mut input).unwrap_or(0) % 16);
    let mut slot_leaders = Vec::with_capacity(slot_leader_count);
    for _ in 0..slot_leader_count {
        let Some(slot) = take_u64(&mut input) else {
            break;
        };
        let Some(pubkey) = take_pubkey(&mut input) else {
            break;
        };
        slot_leaders.push((slot, pubkey));
    }
    verifier.set_slot_leaders(slot_leaders);

    let now = Instant::now();
    let packet_count = usize::from(take_u8(&mut input).unwrap_or(0) % 32);
    for index in 0..packet_count {
        let Some(packet_len) = take_u8(&mut input).map(usize::from) else {
            break;
        };
        let Some(packet) = take_bytes(&mut input, packet_len) else {
            break;
        };
        let _ = verifier.verify_packet(packet, now + Duration::from_millis(index as u64));
        let _ = verifier.slot_leaders_snapshot();
        let _ = verifier.take_slot_leader_diff();
    }
});
