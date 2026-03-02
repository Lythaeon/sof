use std::num::NonZeroUsize;

use super::{read_bool_env, read_env_var};

pub fn read_worker_threads() -> usize {
    read_env_var("SOF_WORKER_THREADS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(NonZeroUsize::get)
                .unwrap_or(1)
        })
}

pub fn read_dataset_workers() -> usize {
    read_env_var("SOF_DATASET_WORKERS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(read_worker_threads)
}

pub fn read_dataset_max_tracked_slots() -> usize {
    read_env_var("SOF_DATASET_MAX_TRACKED_SLOTS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.clamp(128, 65_536))
        .unwrap_or(2_048)
}

pub fn read_fec_max_tracked_sets() -> usize {
    read_env_var("SOF_FEC_MAX_TRACKED_SETS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.clamp(512, 131_072))
        .unwrap_or(8_192)
}

pub fn read_dataset_queue_capacity() -> usize {
    read_env_var("SOF_DATASET_QUEUE_CAPACITY")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(262_144))
        .unwrap_or(8_192)
}

pub fn read_dataset_attempt_cache_capacity() -> usize {
    read_env_var("SOF_DATASET_ATTEMPT_CACHE_CAPACITY")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(131_072))
        .unwrap_or(8_192)
}

pub fn read_dataset_attempt_success_ttl_ms() -> u64 {
    read_env_var("SOF_DATASET_ATTEMPT_SUCCESS_TTL_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(30_000)
}

pub fn read_dataset_attempt_failure_ttl_ms() -> u64 {
    read_env_var("SOF_DATASET_ATTEMPT_FAILURE_TTL_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(3_000)
}

pub fn read_dataset_tail_min_shreds_without_anchor() -> usize {
    read_env_var("SOF_DATASET_TAIL_MIN_SHREDS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(2)
}

pub fn read_coverage_window_slots() -> u64 {
    read_env_var("SOF_COVERAGE_WINDOW_SLOTS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(256)
}

pub fn read_fork_window_slots() -> u64 {
    read_env_var("SOF_FORK_WINDOW_SLOTS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(2_048)
}

pub fn read_log_all_txs() -> bool {
    read_env_var("SOF_LOG_ALL_TXS")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

pub fn read_log_startup_steps() -> bool {
    read_bool_env("SOF_LOG_STARTUP_STEPS", true)
}

pub fn read_log_non_vote_txs() -> bool {
    read_bool_env("SOF_LOG_NON_VOTE_TXS", false)
}

pub fn read_log_dataset_reconstruction() -> bool {
    read_bool_env("SOF_LOG_DATASET_RECONSTRUCTION", false)
}

pub fn read_live_shreds_enabled() -> bool {
    read_bool_env("SOF_LIVE_SHREDS_ENABLED", true)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_log_repair_peer_traffic() -> bool {
    read_bool_env("SOF_LOG_REPAIR_PEER_TRAFFIC", false)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_log_repair_peer_traffic_every() -> u64 {
    read_env_var("SOF_LOG_REPAIR_PEER_TRAFFIC_EVERY")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(256)
}

pub fn read_verify_shreds() -> bool {
    read_bool_env("SOF_VERIFY_SHREDS", false)
}

pub fn read_verify_strict_unknown() -> bool {
    read_bool_env("SOF_VERIFY_STRICT", false)
}

pub fn read_verify_recovered_shreds() -> bool {
    read_bool_env("SOF_VERIFY_RECOVERED_SHREDS", false)
}

pub fn read_verify_signature_cache_entries() -> usize {
    read_env_var("SOF_VERIFY_SIGNATURE_CACHE")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(65_536)
}

pub fn read_verify_slot_leader_window() -> u64 {
    read_env_var("SOF_VERIFY_SLOT_WINDOW")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(4_096)
}

pub fn read_verify_unknown_retry_ms() -> u64 {
    read_env_var("SOF_VERIFY_UNKNOWN_RETRY_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(2_000)
}

pub fn read_shred_dedupe_capacity() -> usize {
    read_env_var("SOF_SHRED_DEDUP_CAPACITY")
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.min(2_000_000))
        .unwrap_or(262_144)
}

pub fn read_shred_dedupe_ttl_ms() -> u64 {
    read_env_var("SOF_SHRED_DEDUP_TTL_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(250)
}

pub fn read_tx_confirmed_depth_slots() -> u64 {
    read_env_var("SOF_TX_CONFIRMED_DEPTH_SLOTS")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(32)
}

pub fn read_tx_finalized_depth_slots() -> u64 {
    read_env_var("SOF_TX_FINALIZED_DEPTH_SLOTS")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(150)
}
