#![no_main]

use libfuzzer_sys::fuzz_target;
use sof_tx::{
    providers::{LeaderTarget, StaticLeaderProvider},
    routing::{RoutingPolicy, select_targets},
};

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

fn target(port: u16) -> LeaderTarget {
    LeaderTarget::new(None, std::net::SocketAddr::from(([127, 0, 0, 1], port)))
}

fuzz_target!(|bytes: &[u8]| {
    let mut input = bytes;
    let current = take_u16(&mut input).map(target);
    let next_count = usize::from(take_u8(&mut input).unwrap_or(0) % 8);
    let mut next = Vec::with_capacity(next_count);
    for _ in 0..next_count {
        next.push(target(take_u16(&mut input).unwrap_or(9000)));
    }
    let backup_count = usize::from(take_u8(&mut input).unwrap_or(0) % 8);
    let mut backups = Vec::with_capacity(backup_count);
    for _ in 0..backup_count {
        backups.push(target(take_u16(&mut input).unwrap_or(9100)));
    }

    let policy = RoutingPolicy {
        next_leaders: usize::from(take_u8(&mut input).unwrap_or(0) % 8),
        backup_validators: usize::from(take_u8(&mut input).unwrap_or(0) % 8),
        max_parallel_sends: usize::from(take_u8(&mut input).unwrap_or(1) % 8).max(1),
    };
    let provider = StaticLeaderProvider::new(current.clone(), next);
    let selected = select_targets(&provider, &backups, policy);

    for window in selected.windows(2) {
        assert_ne!(window[0].tpu_addr, window[1].tpu_addr);
    }
    if let Some(current) = current
        && let Some(first) = selected.first()
    {
        assert_eq!(first.tpu_addr, current.tpu_addr);
    }
});
