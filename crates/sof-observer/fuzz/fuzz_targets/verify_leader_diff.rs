#![no_main]

use std::time::{Duration, Instant};

use libfuzzer_sys::fuzz_target;
use sof::verify::{ShredVerifier, SlotLeaderDiff};

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

fn assert_sorted_unique_slots(slots: &[u64]) {
    for window in slots.windows(2) {
        assert!(window[0] < window[1]);
    }
}

fn assert_sorted_unique_entries(entries: &[(u64, [u8; 32])]) {
    for window in entries.windows(2) {
        assert!(window[0].0 < window[1].0);
    }
}

fn assert_valid_diff(diff: &SlotLeaderDiff) {
    assert_sorted_unique_entries(&diff.added);
    assert_sorted_unique_entries(&diff.updated);
    assert_sorted_unique_slots(&diff.removed_slots);
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

    let op_count = usize::from(take_u8(&mut input).unwrap_or(0));
    let now = Instant::now();

    for op_index in 0..op_count {
        let Some(op) = take_u8(&mut input) else {
            break;
        };
        match op % 5 {
            0 => {
                let count = usize::from(take_u8(&mut input).unwrap_or(0) % 32);
                let mut known_pubkeys = Vec::with_capacity(count);
                for _ in 0..count {
                    let Some(pubkey) = take_pubkey(&mut input) else {
                        break;
                    };
                    known_pubkeys.push(pubkey);
                }
                verifier.set_known_pubkeys(known_pubkeys);
            }
            1 => {
                let count = usize::from(take_u8(&mut input).unwrap_or(0) % 32);
                let mut leaders = Vec::with_capacity(count);
                for _ in 0..count {
                    let Some(slot) = take_u64(&mut input) else {
                        break;
                    };
                    let Some(pubkey) = take_pubkey(&mut input) else {
                        break;
                    };
                    leaders.push((slot, pubkey));
                }
                verifier.set_slot_leaders(leaders);
            }
            2 => {
                let Some(packet_len) = take_u8(&mut input).map(usize::from) else {
                    break;
                };
                let Some(packet) = take_bytes(&mut input, packet_len) else {
                    break;
                };
                let _ = verifier.verify_packet(
                    packet,
                    now + Duration::from_millis(u64::try_from(op_index).unwrap_or(u64::MAX)),
                );
            }
            3 => {
                let diff = verifier.take_slot_leader_diff();
                assert_valid_diff(&diff);
            }
            _ => {
                let snapshot = verifier.slot_leaders_snapshot();
                assert_sorted_unique_entries(&snapshot);
                if let Some((slot, leader)) = snapshot.last().copied() {
                    assert_eq!(verifier.slot_leader_for_slot(slot), Some(leader));
                } else {
                    let random_slot = take_u64(&mut input).unwrap_or(0);
                    let _ = verifier.slot_leader_for_slot(random_slot);
                }
            }
        }
    }

    let final_diff = verifier.take_slot_leader_diff();
    assert_valid_diff(&final_diff);
});
