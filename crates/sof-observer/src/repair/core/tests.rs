use std::time::Duration;

use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::repair::{MissingShredTracker, build_window_index_request};

use super::request::unix_timestamp_ms;

#[test]
fn missing_tracker_emits_gap_request_after_settle_delay() {
    let mut tracker = MissingShredTracker::new(
        128,
        0,
        Duration::from_millis(50),
        Duration::from_millis(100),
        0,
        16,
        0,
    );
    let base = std::time::Instant::now();
    tracker.on_code_shred(99, 0, 32, base);
    tracker.on_data_shred(99, 0, 0, false, 0, base);
    tracker.on_data_shred(99, 2, 0, false, 0, base);

    let early = tracker.collect_requests(base + Duration::from_millis(10), 16, 16, 16);
    assert!(early.is_empty());

    let late = tracker.collect_requests(base + Duration::from_millis(60), 16, 16, 16);
    assert!(late.iter().any(|request| {
        request.slot == 99
            && request.index == 1
            && request.kind == super::MissingShredRequestKind::WindowIndex
    }));
}

#[test]
fn missing_tracker_emits_highest_probe_when_tail_unknown() {
    let mut tracker = MissingShredTracker::new(
        128,
        0,
        Duration::from_millis(10),
        Duration::from_millis(100),
        0,
        16,
        0,
    );
    let base = std::time::Instant::now();
    tracker.on_code_shred(42, 0, 32, base);
    tracker.on_data_shred(42, 0, 0, false, 0, base);

    let early = tracker.collect_requests(base + Duration::from_millis(5), 16, 16, 16);
    assert!(early.is_empty());

    let late = tracker.collect_requests(base + Duration::from_millis(20), 16, 16, 16);
    assert!(late.iter().any(|request| {
        request.slot == 42
            && request.index == 1
            && request.kind == super::MissingShredRequestKind::HighestWindowIndex
    }));
}

#[test]
fn missing_tracker_respects_per_slot_budget() {
    let mut tracker = MissingShredTracker::new(
        128,
        0,
        Duration::from_millis(10),
        Duration::from_millis(100),
        0,
        1,
        0,
    );
    let base = std::time::Instant::now();
    tracker.on_data_shred(10, 0, 0, false, 0, base);
    tracker.on_data_shred(10, 2, 0, false, 0, base);
    tracker.on_data_shred(11, 0, 0, false, 0, base);
    tracker.on_data_shred(11, 2, 0, false, 0, base);

    let late = tracker.collect_requests(base + Duration::from_millis(20), 16, 16, 16);
    assert_eq!(late.len(), 2);
    assert!(
        late.iter()
            .any(|request| request.slot == 10 && request.index == 1)
    );
    assert!(
        late.iter()
            .any(|request| request.slot == 11 && request.index == 1)
    );
}

#[test]
fn missing_tracker_probes_next_slot_after_last_in_slot() {
    let mut tracker = MissingShredTracker::new(
        128,
        0,
        Duration::from_millis(10),
        Duration::from_millis(100),
        0,
        1,
        0,
    );
    let base = std::time::Instant::now();
    tracker.on_data_shred(21, 31, 0, true, 0, base);

    let early = tracker.collect_requests(base + Duration::from_millis(5), 16, 16, 16);
    assert!(early.is_empty());

    let late = tracker.collect_requests(base + Duration::from_millis(20), 16, 16, 16);
    assert!(late.iter().any(|request| {
        request.slot == 22
            && request.index == 0
            && request.kind == super::MissingShredRequestKind::HighestWindowIndex
    }));
}

#[test]
fn missing_tracker_seeded_slot_emits_highest_probe() {
    let mut tracker = MissingShredTracker::new(
        128,
        0,
        Duration::from_millis(10),
        Duration::from_millis(100),
        0,
        16,
        0,
    );
    let base = std::time::Instant::now();
    tracker.seed_highest_probe_slot(77, base);

    let early = tracker.collect_requests(base + Duration::from_millis(5), 16, 16, 16);
    assert!(early.is_empty());

    let late = tracker.collect_requests(base + Duration::from_millis(20), 16, 16, 16);
    assert!(late.iter().any(|request| {
        request.slot == 77
            && request.index == 0
            && request.kind == super::MissingShredRequestKind::HighestWindowIndex
    }));
}

#[test]
fn missing_tracker_requests_observed_gap_before_prefix_backfill() {
    let mut tracker = MissingShredTracker::new(
        128,
        0,
        Duration::from_millis(10),
        Duration::from_millis(100),
        0,
        16,
        0,
    );
    let base = std::time::Instant::now();
    tracker.on_data_shred(300, 100, 96, false, 0, base);
    tracker.on_data_shred(300, 102, 96, false, 0, base);

    let late = tracker.collect_requests(base + Duration::from_millis(20), 16, 16, 16);
    assert!(!late.is_empty());
    assert_eq!(late[0].slot, 300);
    assert_eq!(late[0].index, 101);
}

#[test]
fn missing_tracker_prioritizes_newer_slot_before_wider_gap() {
    let mut tracker = MissingShredTracker::new(
        128,
        0,
        Duration::from_millis(10),
        Duration::from_millis(100),
        0,
        16,
        0,
    );
    let base = std::time::Instant::now();
    tracker.on_data_shred(400, 100, 96, false, 0, base);
    tracker.on_data_shred(400, 102, 96, false, 0, base);
    tracker.on_data_shred(401, 220, 192, false, 0, base);
    tracker.on_data_shred(401, 260, 256, false, 0, base);

    let late = tracker.collect_requests(base + Duration::from_millis(20), 1, 1, 1);
    assert_eq!(late.len(), 1);
    assert_eq!(late[0].slot, 401);
    assert_eq!(late[0].index, 221);
}

#[test]
fn missing_tracker_defers_tip_gap_requests_until_slot_lag() {
    let mut tracker = MissingShredTracker::new(
        128,
        4,
        Duration::from_millis(10),
        Duration::from_millis(100),
        0,
        16,
        0,
    );
    let base = std::time::Instant::now();
    tracker.on_data_shred(700, 0, 0, false, 0, base);
    tracker.on_data_shred(700, 2, 0, false, 0, base);

    let tip_slot = tracker.collect_requests(base + Duration::from_millis(20), 16, 16, 16);
    assert!(tip_slot.is_empty());

    tracker.on_data_shred(704, 0, 0, false, 0, base + Duration::from_millis(30));
    let lagged = tracker.collect_requests(base + Duration::from_millis(50), 16, 16, 16);
    assert!(lagged.iter().any(|request| {
        request.slot == 700
            && request.index == 1
            && request.kind == super::MissingShredRequestKind::WindowIndex
    }));
}

#[test]
fn missing_tracker_seeds_forward_tip_probes() {
    let mut tracker = MissingShredTracker::new(
        128,
        0,
        Duration::from_millis(10),
        Duration::from_millis(100),
        0,
        16,
        2,
    );
    let base = std::time::Instant::now();
    tracker.on_data_shred(900, 0, 0, false, 0, base);

    let first = tracker.collect_requests(base + Duration::from_millis(12), 16, 16, 16);
    assert!(
        first.iter().any(|request| {
            request.slot == 900
                && request.index == 1
                && request.kind == super::MissingShredRequestKind::HighestWindowIndex
        }),
        "expected local highest probe"
    );

    let second = tracker.collect_requests(base + Duration::from_millis(30), 16, 16, 16);
    assert!(
        second.iter().any(|request| {
            request.slot == 901
                && request.index == 0
                && request.kind == super::MissingShredRequestKind::HighestWindowIndex
        }),
        "expected forward highest probe"
    );
}

#[test]
fn missing_tracker_forward_probes_do_not_advance_latest_slot() {
    let mut tracker = MissingShredTracker::new(
        128,
        1,
        Duration::from_millis(10),
        Duration::from_millis(100),
        0,
        16,
        4,
    );
    let base = std::time::Instant::now();
    tracker.on_data_shred(10, 0, 0, false, 0, base);
    tracker.on_data_shred(10, 2, 0, false, 0, base);

    let first = tracker.collect_requests(base + Duration::from_millis(20), 16, 16, 16);
    assert!(
        !first.iter().any(|request| {
            request.slot == 10 && request.kind == super::MissingShredRequestKind::WindowIndex
        }),
        "tip slot should not request WindowIndex while lag < min_slot_lag"
    );

    let second = tracker.collect_requests(base + Duration::from_millis(40), 16, 16, 16);
    assert!(
        !second.iter().any(|request| {
            request.slot == 10 && request.kind == super::MissingShredRequestKind::WindowIndex
        }),
        "forward probe seeding must not advance latest slot and unlock tip WindowIndex repairs"
    );
}

#[test]
fn missing_tracker_prioritizes_nearest_forward_probe() {
    let mut tracker = MissingShredTracker::new(
        128,
        0,
        Duration::from_millis(10),
        Duration::from_millis(100),
        0,
        16,
        2,
    );
    let base = std::time::Instant::now();
    tracker.on_data_shred(100, 0, 0, false, 0, base);

    let _ = tracker.collect_requests(base + Duration::from_millis(12), 16, 16, 16);
    let requests = tracker.collect_requests(base + Duration::from_millis(30), 2, 2, 1);
    assert!(
        requests.iter().any(|request| {
            request.slot == 101
                && request.kind == super::MissingShredRequestKind::HighestWindowIndex
        }),
        "expected nearest forward probe"
    );
    assert!(
        !requests.iter().any(|request| request.slot == 102),
        "farthest forward probe should not outrank nearest forward probe when budget is tight"
    );
}

#[test]
fn missing_tracker_respects_highest_request_budget() {
    let mut tracker = MissingShredTracker::new(
        128,
        0,
        Duration::from_millis(10),
        Duration::from_millis(100),
        0,
        16,
        0,
    );
    let base = std::time::Instant::now();
    tracker.seed_highest_probe_slot(1_000, base);
    tracker.seed_highest_probe_slot(1_001, base);
    tracker.on_data_shred(900, 0, 0, false, 0, base);
    tracker.on_data_shred(900, 2, 0, false, 0, base);

    let requests = tracker.collect_requests(base + Duration::from_millis(20), 4, 1, 1);
    let highest_count = requests
        .iter()
        .filter(|request| request.kind == super::MissingShredRequestKind::HighestWindowIndex)
        .count();
    assert_eq!(highest_count, 1);
    assert!(requests.iter().any(|request| {
        request.kind == super::MissingShredRequestKind::WindowIndex
            && request.slot == 900
            && request.index == 1
    }));
}

#[test]
fn missing_tracker_respects_forward_probe_budget() {
    let mut tracker = MissingShredTracker::new(
        128,
        0,
        Duration::from_millis(10),
        Duration::from_millis(100),
        0,
        16,
        3,
    );
    let base = std::time::Instant::now();
    tracker.on_data_shred(500, 0, 0, false, 0, base);

    let _ = tracker.collect_requests(base + Duration::from_millis(12), 16, 16, 16);
    let requests = tracker.collect_requests(base + Duration::from_millis(30), 8, 8, 1);

    let forward_probe_slots: Vec<u64> = requests
        .iter()
        .filter(|request| {
            request.kind == super::MissingShredRequestKind::HighestWindowIndex && request.slot > 500
        })
        .map(|request| request.slot)
        .collect();
    assert_eq!(forward_probe_slots.len(), 1);
    assert_eq!(forward_probe_slots[0], 501);
}

#[test]
fn repair_request_signs_payload() {
    let keypair = Keypair::new();
    let recipient = Keypair::new().pubkey();
    let payload =
        build_window_index_request(&keypair, recipient, 123, 7, 99).expect("request payload");
    assert!(payload.len() > 4 + 64);
    let signed_data = [&payload[..4], &payload[4 + 64..]].concat();
    let signature =
        solana_signature::Signature::try_from(&payload[4..4 + 64]).expect("signature bytes");
    assert!(signature.verify(keypair.pubkey().as_ref(), &signed_data));
}

#[test]
fn unix_timestamp_is_monotonicish() {
    let first = unix_timestamp_ms();
    let second = unix_timestamp_ms();
    assert!(second >= first);
}
