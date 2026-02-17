#![no_main]

use std::{net::SocketAddr, sync::Arc};

use libfuzzer_sys::fuzz_target;
use sof::framework::{
    ClusterNodeInfo, ClusterTopologyEvent, ControlPlaneSource, DatasetEvent, LeaderScheduleEntry,
    LeaderScheduleEvent, ObservedRecentBlockhashEvent, ObserverPlugin, PluginDispatchMode,
    PluginHost, RawPacketEvent, ShredEvent,
};
use sof::shred::wire::{SIZE_OF_DATA_SHRED_HEADERS, parse_shred_header};
use solana_pubkey::Pubkey;

#[derive(Clone, Copy)]
struct NopPlugin;

impl ObserverPlugin for NopPlugin {
    fn name(&self) -> &'static str {
        "nop-plugin"
    }
}

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

fn take_u32(input: &mut &[u8]) -> Option<u32> {
    let bytes = take_bytes(input, 4)?;
    let bytes: [u8; 4] = bytes.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

fn take_u64(input: &mut &[u8]) -> Option<u64> {
    let bytes = take_bytes(input, 8)?;
    let bytes: [u8; 8] = bytes.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

fn take_pubkey(input: &mut &[u8]) -> Option<Pubkey> {
    let bytes = take_bytes(input, 32)?;
    let bytes: [u8; 32] = bytes.try_into().ok()?;
    Some(Pubkey::new_from_array(bytes))
}

fn take_socket_addr(input: &mut &[u8]) -> Option<SocketAddr> {
    let ip = take_bytes(input, 4)?;
    let port = take_u16(input)?;
    Some(SocketAddr::from(([ip[0], ip[1], ip[2], ip[3]], port)))
}

fn build_data_packet(slot: u64, index: u32, fec_set_index: u32, payload: &[u8]) -> Vec<u8> {
    let mut packet = vec![0_u8; sof::shred::wire::SIZE_OF_DATA_SHRED_PAYLOAD];
    packet[64] = sof::protocol::shred_wire::VARIANT_MERKLE_DATA;
    packet[65..73].copy_from_slice(&slot.to_le_bytes());
    packet[73..77].copy_from_slice(&index.to_le_bytes());
    packet[77..79].copy_from_slice(&1_u16.to_le_bytes());
    packet[79..83].copy_from_slice(&fec_set_index.to_le_bytes());
    packet[83..85].copy_from_slice(&0_u16.to_le_bytes());
    packet[85] = 0;
    let payload_len = payload.len().min(
        sof::shred::wire::SIZE_OF_DATA_SHRED_PAYLOAD.saturating_sub(SIZE_OF_DATA_SHRED_HEADERS),
    );
    let declared_size = SIZE_OF_DATA_SHRED_HEADERS.saturating_add(payload_len);
    let declared_size_u16 = u16::try_from(declared_size).unwrap_or(u16::MAX);
    packet[86..88].copy_from_slice(&declared_size_u16.to_le_bytes());
    packet[88..88 + payload_len].copy_from_slice(&payload[..payload_len]);
    packet
}

fn read_leader_entries(input: &mut &[u8], max: usize) -> Vec<LeaderScheduleEntry> {
    let count = usize::from(take_u8(input).unwrap_or(0) % u8::try_from(max).unwrap_or(u8::MAX));
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let Some(slot) = take_u64(input) else {
            break;
        };
        let Some(leader) = take_pubkey(input) else {
            break;
        };
        out.push(LeaderScheduleEntry { slot, leader });
    }
    out
}

fn read_nodes(input: &mut &[u8], max: usize) -> Vec<ClusterNodeInfo> {
    let count = usize::from(take_u8(input).unwrap_or(0) % u8::try_from(max).unwrap_or(u8::MAX));
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let Some(pubkey) = take_pubkey(input) else {
            break;
        };
        let wallclock = take_u64(input).unwrap_or(0);
        let shred_version = take_u16(input).unwrap_or(0);
        let gossip = if take_u8(input).unwrap_or(0) & 1 == 1 {
            take_socket_addr(input)
        } else {
            None
        };
        let tpu = if take_u8(input).unwrap_or(0) & 1 == 1 {
            take_socket_addr(input)
        } else {
            None
        };
        let tvu = if take_u8(input).unwrap_or(0) & 1 == 1 {
            take_socket_addr(input)
        } else {
            None
        };
        let rpc = if take_u8(input).unwrap_or(0) & 1 == 1 {
            take_socket_addr(input)
        } else {
            None
        };
        out.push(ClusterNodeInfo {
            pubkey,
            wallclock,
            shred_version,
            gossip,
            tpu,
            tvu,
            rpc,
        });
    }
    out
}

fuzz_target!(|bytes: &[u8]| {
    let mut input = bytes;
    let queue_capacity = usize::from(take_u8(&mut input).unwrap_or(8) % 64).max(1);
    let plugin_count = usize::from(take_u8(&mut input).unwrap_or(1) % 4).max(1);
    let dispatch_mode = if take_u8(&mut input).unwrap_or(0) & 1 == 0 {
        PluginDispatchMode::Sequential
    } else {
        let limit = usize::from(take_u8(&mut input).unwrap_or(2) % 16).max(1);
        PluginDispatchMode::BoundedConcurrent(limit)
    };

    let mut builder = PluginHost::builder()
        .with_event_queue_capacity(queue_capacity)
        .with_dispatch_mode(dispatch_mode);
    for _ in 0..plugin_count {
        builder = builder.add_plugin(NopPlugin);
    }
    let host = builder.build();
    assert_eq!(host.len(), plugin_count);

    let mut expected_recent: Option<(u64, [u8; 32])> = None;
    let mut expected_leader: Option<LeaderScheduleEntry> = None;
    let mut hook_calls = 0_u64;
    let op_count = usize::from(take_u8(&mut input).unwrap_or(0));

    for _ in 0..op_count {
        let Some(kind) = take_u8(&mut input) else {
            break;
        };

        match kind % 6 {
            0 => {
                let Some(source) = take_socket_addr(&mut input) else {
                    break;
                };
                let Some(payload_len) = take_u8(&mut input).map(usize::from) else {
                    break;
                };
                let Some(payload) = take_bytes(&mut input, payload_len) else {
                    break;
                };
                hook_calls = hook_calls.saturating_add(1);
                host.on_raw_packet(RawPacketEvent {
                    source,
                    bytes: Arc::<[u8]>::from(payload),
                });
            }
            1 => {
                let Some(source) = take_socket_addr(&mut input) else {
                    break;
                };
                let slot = take_u64(&mut input).unwrap_or(0);
                let index = take_u32(&mut input).unwrap_or(0);
                let fec = take_u32(&mut input).unwrap_or(0);
                let Some(payload_len) = take_u8(&mut input).map(usize::from) else {
                    break;
                };
                let Some(payload) = take_bytes(&mut input, payload_len) else {
                    break;
                };
                let packet = build_data_packet(slot, index, fec, payload);
                if let Ok(parsed) = parse_shred_header(&packet) {
                    hook_calls = hook_calls.saturating_add(1);
                    host.on_shred(ShredEvent {
                        source,
                        packet: Arc::<[u8]>::from(packet),
                        parsed: Arc::new(parsed),
                    });
                }
            }
            2 => {
                let slot = take_u64(&mut input).unwrap_or(0);
                let start_index = take_u32(&mut input).unwrap_or(0);
                let span = take_u8(&mut input).unwrap_or(0);
                let end_index = start_index.saturating_add(u32::from(span));
                let shreds = usize::from(take_u8(&mut input).unwrap_or(0));
                let payload_len = usize::from(take_u16(&mut input).unwrap_or(0));
                let tx_count = u64::from(take_u16(&mut input).unwrap_or(0));
                hook_calls = hook_calls.saturating_add(1);
                host.on_dataset(DatasetEvent {
                    slot,
                    start_index,
                    end_index,
                    last_in_slot: take_u8(&mut input).unwrap_or(0) & 1 == 1,
                    shreds,
                    payload_len,
                    tx_count,
                });
            }
            3 => {
                let slot = take_u64(&mut input).unwrap_or(0);
                let mut recent_blockhash = [0_u8; 32];
                let available = input.len().min(32);
                if available > 0 {
                    if let Some(bytes) = take_bytes(&mut input, available) {
                        recent_blockhash[..available].copy_from_slice(bytes);
                    }
                }
                let dataset_tx_count = u64::from(take_u16(&mut input).unwrap_or(0));

                expected_recent = match expected_recent {
                    None => Some((slot, recent_blockhash)),
                    Some((current_slot, current_hash)) if slot < current_slot => {
                        Some((current_slot, current_hash))
                    }
                    Some((current_slot, current_hash))
                        if slot == current_slot && recent_blockhash == current_hash =>
                    {
                        Some((current_slot, current_hash))
                    }
                    Some((current_slot, current_hash))
                        if slot > current_slot && recent_blockhash == current_hash =>
                    {
                        Some((slot, current_hash))
                    }
                    Some(_) => Some((slot, recent_blockhash)),
                };

                hook_calls = hook_calls.saturating_add(1);
                host.on_recent_blockhash(ObservedRecentBlockhashEvent {
                    slot,
                    recent_blockhash,
                    dataset_tx_count,
                });
                assert_eq!(host.latest_observed_recent_blockhash(), expected_recent);
            }
            4 => {
                let source = if take_u8(&mut input).unwrap_or(0) & 1 == 0 {
                    ControlPlaneSource::Direct
                } else {
                    ControlPlaneSource::GossipBootstrap
                };
                let slot = Some(take_u64(&mut input).unwrap_or(0));
                let epoch = Some(u64::from(take_u16(&mut input).unwrap_or(0)));
                let active_entrypoint = if take_u8(&mut input).unwrap_or(0) & 1 == 1 {
                    Some(format!(
                        "{}.{}.{}.{}:{}",
                        take_u8(&mut input).unwrap_or(127),
                        take_u8(&mut input).unwrap_or(0),
                        take_u8(&mut input).unwrap_or(0),
                        take_u8(&mut input).unwrap_or(1),
                        take_u16(&mut input).unwrap_or(8001)
                    ))
                } else {
                    None
                };
                let added_nodes = read_nodes(&mut input, 8);
                let updated_nodes = read_nodes(&mut input, 8);
                let removed_count = usize::from(take_u8(&mut input).unwrap_or(0) % 8);
                let mut removed_pubkeys = Vec::with_capacity(removed_count);
                for _ in 0..removed_count {
                    if let Some(pubkey) = take_pubkey(&mut input) {
                        removed_pubkeys.push(pubkey);
                    }
                }
                let snapshot_nodes = read_nodes(&mut input, 8);
                let total_nodes = added_nodes
                    .len()
                    .saturating_add(updated_nodes.len())
                    .saturating_add(snapshot_nodes.len());

                hook_calls = hook_calls.saturating_add(1);
                host.on_cluster_topology(ClusterTopologyEvent {
                    source,
                    slot,
                    epoch,
                    active_entrypoint,
                    total_nodes,
                    added_nodes,
                    removed_pubkeys,
                    updated_nodes,
                    snapshot_nodes,
                });
            }
            _ => {
                let source = if take_u8(&mut input).unwrap_or(0) & 1 == 0 {
                    ControlPlaneSource::Direct
                } else {
                    ControlPlaneSource::GossipBootstrap
                };
                let slot = Some(take_u64(&mut input).unwrap_or(0));
                let epoch = Some(u64::from(take_u16(&mut input).unwrap_or(0)));
                let added_leaders = read_leader_entries(&mut input, 8);
                let updated_leaders = read_leader_entries(&mut input, 8);
                let snapshot_leaders = read_leader_entries(&mut input, 8);
                let removed_count = usize::from(take_u8(&mut input).unwrap_or(0) % 8);
                let mut removed_slots = Vec::with_capacity(removed_count);
                for _ in 0..removed_count {
                    removed_slots.push(u64::from(take_u16(&mut input).unwrap_or(0)));
                }

                let newest_entry = added_leaders
                    .iter()
                    .chain(updated_leaders.iter())
                    .chain(snapshot_leaders.iter())
                    .copied()
                    .max_by_key(|entry| entry.slot);
                if let Some(newest) = newest_entry {
                    expected_leader = match expected_leader {
                        None => Some(newest),
                        Some(current)
                            if newest.slot > current.slot
                                || (newest.slot == current.slot
                                    && newest.leader != current.leader) =>
                        {
                            Some(newest)
                        }
                        Some(current) => Some(current),
                    };
                }

                hook_calls = hook_calls.saturating_add(1);
                host.on_leader_schedule(LeaderScheduleEvent {
                    source,
                    slot,
                    epoch,
                    added_leaders,
                    removed_slots,
                    updated_leaders,
                    snapshot_leaders,
                });
                assert_eq!(host.latest_observed_tpu_leader(), expected_leader);
            }
        }
    }

    assert!(host.dropped_event_count() <= hook_calls);
});
