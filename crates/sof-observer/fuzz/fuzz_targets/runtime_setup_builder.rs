#![no_main]

use std::net::SocketAddr;

use libfuzzer_sys::fuzz_target;
use sof::runtime::RuntimeSetup;

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

fn take_bool(input: &mut &[u8]) -> bool {
    take_u8(input).unwrap_or(0) & 1 == 1
}

fn take_socket_addr(input: &mut &[u8]) -> Option<SocketAddr> {
    let ip = take_bytes(input, 4)?;
    let port = take_u16(input)?;
    Some(SocketAddr::from(([ip[0], ip[1], ip[2], ip[3]], port)))
}

fn take_string(input: &mut &[u8], max_len: usize) -> Option<String> {
    let len = usize::from(take_u8(input)?).min(max_len);
    let bytes = take_bytes(input, len)?;
    Some(String::from_utf8_lossy(bytes).to_string())
}

fuzz_target!(|bytes: &[u8]| {
    let mut input = bytes;
    let mut setup = RuntimeSetup::new();
    let op_count = usize::from(take_u8(&mut input).unwrap_or(0));

    for _ in 0..op_count {
        let Some(op) = take_u8(&mut input) else {
            break;
        };

        setup = match op % 26 {
            0 => setup.with_env(
                take_string(&mut input, 32).unwrap_or_else(|| "KEY".to_owned()),
                take_string(&mut input, 64).unwrap_or_else(|| "VALUE".to_owned()),
            ),
            1 => setup.with_rust_log_filter(
                take_string(&mut input, 64).unwrap_or_else(|| "info".to_owned()),
            ),
            2 => {
                if let Some(addr) = take_socket_addr(&mut input) {
                    setup.with_bind_addr(addr)
                } else {
                    setup
                }
            }
            3 => {
                let count = usize::from(take_u8(&mut input).unwrap_or(0) % 4);
                let mut validators = Vec::with_capacity(count);
                for _ in 0..count {
                    validators.push(
                        take_string(&mut input, 64)
                            .unwrap_or_else(|| "11111111111111111111111111111111".to_owned()),
                    );
                }
                setup.with_gossip_validators(validators)
            }
            4 => {
                setup.with_udp_receiver_core(usize::from(take_u8(&mut input).unwrap_or(0)))
            }
            5 => {
                let count = usize::from(take_u8(&mut input).unwrap_or(0) % 4);
                let mut endpoints = Vec::with_capacity(count);
                for _ in 0..count {
                    endpoints.push(
                        take_string(&mut input, 64).unwrap_or_else(|| {
                            "entrypoint.mainnet-beta.solana.com:8001".to_owned()
                        }),
                    );
                }
                setup.with_gossip_entrypoints(endpoints)
            }
            6 => setup.with_gossip_port_range(
                take_u16(&mut input).unwrap_or(12_000),
                take_u16(&mut input).unwrap_or(12_100),
            ),
            7 => setup.with_shred_version(take_u16(&mut input).unwrap_or(0)),
            8 => setup.with_startup_step_logs(take_bool(&mut input)),
            9 => setup.with_repair_peer_traffic_logs(take_bool(&mut input)),
            10 => setup.with_repair_peer_traffic_every(take_u64(&mut input).unwrap_or(1)),
            11 => setup.with_worker_threads(usize::from(take_u8(&mut input).unwrap_or(1)).max(1)),
            12 => setup.with_dataset_workers(usize::from(take_u8(&mut input).unwrap_or(1)).max(1)),
            13 => setup.with_dataset_max_tracked_slots(
                usize::from(take_u16(&mut input).unwrap_or(128)).max(1),
            ),
            14 => setup
                .with_dataset_queue_capacity(usize::from(take_u16(&mut input).unwrap_or(1)).max(1)),
            15 => setup
                .with_fec_max_tracked_sets(usize::from(take_u16(&mut input).unwrap_or(1)).max(1)),
            16 => setup.with_log_all_txs(take_bool(&mut input)),
            17 => setup.with_log_all_txs(take_bool(&mut input)),
            18 => setup.with_log_non_vote_txs(take_bool(&mut input)),
            19 => setup.with_log_dataset_reconstruction(take_bool(&mut input)),
            20 => setup.with_live_shreds_enabled(take_bool(&mut input)),
            21 => setup.with_verify_shreds(take_bool(&mut input)),
            22 => setup.with_verify_strict_unknown(take_bool(&mut input)),
            23 => setup.with_verify_recovered_shreds(take_bool(&mut input)),
            24 => setup.with_verify_slot_window(take_u64(&mut input).unwrap_or(1)),
            _ => setup
                .with_verify_slot_window(take_u64(&mut input).unwrap_or(1))
                .with_verify_signature_cache_entries(
                    usize::from(take_u16(&mut input).unwrap_or(1)).max(1),
                )
                .with_shred_dedupe_capacity(usize::from(take_u16(&mut input).unwrap_or(1)).max(1))
                .with_shred_dedupe_ttl_ms(take_u64(&mut input).unwrap_or(1)),
        };
    }

    let setup_clone = setup.clone();
    let debug_repr = format!("{setup_clone:?}");
    assert!(!debug_repr.is_empty());
});
