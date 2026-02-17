use std::time::{Duration, Instant};

use crate::repair::{MissingShredRequest, MissingShredRequestKind};

use super::OutstandingRepairRequests;

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
