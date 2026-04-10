use std::{
    env,
    hint::black_box,
    time::{Duration, Instant},
};

use crate::repair::{MissingShredRequest, MissingShredRequestKind};

use super::OutstandingRepairRequests;

fn profile_iterations(default: usize) -> usize {
    env::var("SOF_PROFILE_ITERATIONS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[test]
#[ignore = "profiling fixture for outstanding repair request churn"]
fn outstanding_repairs_profile_fixture() {
    let iterations = profile_iterations(50_000);
    let mut outstanding = OutstandingRepairRequests::new(Duration::from_millis(150));
    let started = Instant::now();

    for slot in 0_u64..64 {
        let request = MissingShredRequest {
            slot,
            index: 0,
            kind: MissingShredRequestKind::HighestWindowIndex,
        };
        assert!(outstanding.try_reserve(&request, started));
    }

    for iteration in 0..iterations {
        let now = started + Duration::from_millis(u64::try_from(iteration % 500).unwrap_or(0));
        let request = MissingShredRequest {
            slot: u64::try_from(iteration % 64).unwrap_or(0),
            index: u32::try_from(iteration % 32).unwrap_or(0),
            kind: if iteration % 4 == 0 {
                MissingShredRequestKind::HighestWindowIndex
            } else {
                MissingShredRequestKind::WindowIndex
            },
        };

        black_box(outstanding.try_reserve(&request, now));
        if iteration % 3 == 0 {
            black_box(outstanding.on_shred_received(request.slot, request.index));
        }
        if iteration % 11 == 0 {
            black_box(outstanding.purge_expired(now));
        }
    }

    let elapsed = started.elapsed();
    let avg_ns_per_iteration = elapsed.as_nanos() / u128::try_from(iterations).unwrap_or(1);
    let avg_us_per_iteration = avg_ns_per_iteration as f64 / 1_000.0;
    eprintln!(
        "outstanding_repairs_profile_fixture iterations={} elapsed_us={} avg_ns_per_iteration={} avg_us_per_iteration={:.3} entries={}",
        iterations,
        elapsed.as_micros(),
        avg_ns_per_iteration,
        avg_us_per_iteration,
        outstanding.len(),
    );
}

#[test]
fn outstanding_repairs_dedup_within_timeout() {
    let mut outstanding = OutstandingRepairRequests::new(Duration::from_millis(150));
    let now = Instant::now();
    let request = MissingShredRequest {
        slot: 42,
        index: 3,
        kind: MissingShredRequestKind::WindowIndex,
    };

    assert!(outstanding.try_reserve(&request, now));
    assert!(!outstanding.try_reserve(&request, now + Duration::from_millis(50)));
    assert_eq!(outstanding.len(), 1);
    assert!(outstanding.try_reserve(&request, now + Duration::from_millis(151)));
}

#[test]
fn outstanding_repairs_clear_when_data_arrives() {
    let mut outstanding = OutstandingRepairRequests::new(Duration::from_millis(150));
    let now = Instant::now();
    let request = MissingShredRequest {
        slot: 900,
        index: 17,
        kind: MissingShredRequestKind::WindowIndex,
    };
    assert!(outstanding.try_reserve(&request, now));
    assert_eq!(outstanding.on_shred_received(900, 17), 1);
    assert_eq!(outstanding.len(), 0);
    assert!(outstanding.try_reserve(&request, now + Duration::from_millis(10)));
}

#[test]
fn outstanding_repairs_clear_highest_on_any_slot_shred() {
    let mut outstanding = OutstandingRepairRequests::new(Duration::from_millis(150));
    let now = Instant::now();
    let highest = MissingShredRequest {
        slot: 777,
        index: 0,
        kind: MissingShredRequestKind::HighestWindowIndex,
    };
    assert!(outstanding.try_reserve(&highest, now));
    assert_eq!(outstanding.len(), 1);

    assert_eq!(outstanding.on_shred_received(777, 120), 1);
    assert_eq!(outstanding.len(), 0);
}

#[test]
fn outstanding_repairs_clear_only_matching_highest_prefix_for_slot() {
    let mut outstanding = OutstandingRepairRequests::new(Duration::from_millis(150));
    let now = Instant::now();
    let first = MissingShredRequest {
        slot: 800,
        index: 10,
        kind: MissingShredRequestKind::HighestWindowIndex,
    };
    let second = MissingShredRequest {
        slot: 800,
        index: 25,
        kind: MissingShredRequestKind::HighestWindowIndex,
    };
    let other_slot = MissingShredRequest {
        slot: 801,
        index: 12,
        kind: MissingShredRequestKind::HighestWindowIndex,
    };

    assert!(outstanding.try_reserve(&first, now));
    assert!(outstanding.try_reserve(&second, now));
    assert!(outstanding.try_reserve(&other_slot, now));

    assert_eq!(outstanding.on_shred_received(800, 12), 1);
    assert_eq!(outstanding.len(), 2);

    assert!(!outstanding.try_reserve(&second, now + Duration::from_millis(10)));
    assert!(!outstanding.try_reserve(&other_slot, now + Duration::from_millis(10)));
    assert!(outstanding.try_reserve(&first, now + Duration::from_millis(10)));
}
