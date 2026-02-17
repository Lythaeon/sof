#[cfg(feature = "gossip-bootstrap")]
use super::read_bool_env;
use super::read_env_var;

#[cfg(feature = "gossip-bootstrap")]
pub fn read_repair_enabled() -> bool {
    read_bool_env("SOF_REPAIR_ENABLED", true)
}

pub fn read_repair_tick_ms() -> u64 {
    read_env_var("SOF_REPAIR_TICK_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(200)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_repair_source_hint_flush_ms() -> u64 {
    read_env_var("SOF_REPAIR_SOURCE_HINT_FLUSH_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(200)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_repair_source_hint_batch_size() -> usize {
    read_env_var("SOF_REPAIR_SOURCE_HINT_BATCH_SIZE")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(8_192))
        .unwrap_or(512)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_repair_source_hint_capacity() -> usize {
    read_env_var("SOF_REPAIR_SOURCE_HINT_CAPACITY")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(65_536))
        .unwrap_or(16_384)
}

pub fn read_repair_slot_window() -> u64 {
    read_env_var("SOF_REPAIR_SLOT_WINDOW")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(512)
}

pub fn read_repair_settle_ms() -> u64 {
    read_env_var("SOF_REPAIR_SETTLE_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(250)
}

pub fn read_repair_cooldown_ms() -> u64 {
    read_env_var("SOF_REPAIR_COOLDOWN_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(150)
}

pub fn read_repair_max_requests_per_tick() -> usize {
    read_env_var("SOF_REPAIR_MAX_REQUESTS_PER_TICK")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(64)
}

pub fn read_repair_outstanding_timeout_ms() -> u64 {
    read_env_var("SOF_REPAIR_OUTSTANDING_TIMEOUT_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(150)
}

pub fn read_repair_per_slot_cap() -> usize {
    read_env_var("SOF_REPAIR_PER_SLOT_CAP")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(2048))
        .unwrap_or(16)
}

pub fn read_repair_per_slot_cap_stalled() -> usize {
    read_env_var("SOF_REPAIR_PER_SLOT_CAP_STALLED")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(4096))
        .unwrap_or_else(|| read_repair_per_slot_cap().saturating_mul(4).clamp(32, 4096))
}

pub fn read_repair_dataset_stall_ms() -> u64 {
    read_env_var("SOF_REPAIR_DATASET_STALL_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(2_000)
}

pub fn read_repair_min_slot_lag() -> u64 {
    read_env_var("SOF_REPAIR_MIN_SLOT_LAG")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(4)
}

pub fn read_repair_min_slot_lag_stalled() -> u64 {
    read_env_var("SOF_REPAIR_MIN_SLOT_LAG_STALLED")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}

pub fn read_repair_tip_stall_ms() -> u64 {
    read_env_var("SOF_REPAIR_TIP_STALL_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(800)
}

pub fn read_repair_tip_probe_ahead_slots() -> usize {
    read_env_var("SOF_REPAIR_TIP_PROBE_AHEAD_SLOTS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(1_024))
        .unwrap_or(16)
}

pub fn read_repair_max_requests_per_tick_stalled() -> usize {
    read_env_var("SOF_REPAIR_MAX_REQUESTS_PER_TICK_STALLED")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(4096))
        .unwrap_or_else(|| {
            read_repair_max_requests_per_tick()
                .saturating_mul(4)
                .clamp(1, 4096)
        })
}

pub fn read_repair_max_highest_per_tick() -> usize {
    read_env_var("SOF_REPAIR_MAX_HIGHEST_PER_TICK")
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.min(4_096))
        .unwrap_or(32)
}

pub fn read_repair_max_highest_per_tick_stalled() -> usize {
    read_env_var("SOF_REPAIR_MAX_HIGHEST_PER_TICK_STALLED")
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.min(4_096))
        .unwrap_or_else(|| {
            read_repair_max_highest_per_tick()
                .saturating_mul(4)
                .clamp(128, 4_096)
        })
}

pub fn read_repair_max_forward_probe_per_tick() -> usize {
    read_env_var("SOF_REPAIR_MAX_FORWARD_PROBE_PER_TICK")
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.min(4_096))
        .unwrap_or(16)
}

pub fn read_repair_max_forward_probe_per_tick_stalled() -> usize {
    read_env_var("SOF_REPAIR_MAX_FORWARD_PROBE_PER_TICK_STALLED")
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.min(4_096))
        .unwrap_or_else(|| {
            read_repair_max_forward_probe_per_tick()
                .saturating_mul(8)
                .clamp(128, 4_096)
        })
}

pub fn read_repair_seed_slots() -> u64 {
    read_env_var("SOF_REPAIR_SEED_SLOTS")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(16)
}

pub fn read_repair_backfill_sets() -> usize {
    read_env_var("SOF_REPAIR_BACKFILL_SETS")
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.min(256))
        .unwrap_or(8)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_repair_peer_cache_ttl_ms() -> u64 {
    read_env_var("SOF_REPAIR_PEER_CACHE_TTL_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(10_000)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_repair_peer_cache_capacity() -> usize {
    read_env_var("SOF_REPAIR_PEER_CACHE_CAPACITY")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(4_096))
        .unwrap_or(128)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_repair_active_peers() -> usize {
    read_env_var("SOF_REPAIR_ACTIVE_PEERS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(2_048))
        .unwrap_or(256)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_repair_command_queue_capacity() -> usize {
    read_env_var("SOF_REPAIR_COMMAND_QUEUE")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(262_144))
        .unwrap_or(16_384)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_repair_result_queue_capacity() -> usize {
    read_env_var("SOF_REPAIR_RESULT_QUEUE")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(262_144))
        .unwrap_or(16_384)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_repair_peer_refresh_ms() -> u64 {
    read_env_var("SOF_REPAIR_PEER_REFRESH_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1_000)
}
