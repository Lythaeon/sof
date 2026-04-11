use std::{num::NonZeroUsize, path::PathBuf};

use super::{read_bool_env, read_env_var};
use crate::{
    framework::{DerivedStateReplayBackend, DerivedStateReplayDurability},
    runtime::{
        DerivedStateReplayConfig, DerivedStateRuntimeConfig, RuntimeDeliveryProfile, ShredTrustMode,
    },
};

fn read_optional_bool_env(name: &str) -> Option<bool> {
    read_env_var(name).map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

pub fn read_shred_trust_mode() -> ShredTrustMode {
    match read_env_var("SOF_SHRED_TRUST_MODE").as_deref() {
        Some("trusted_raw_shred_provider" | "trusted-raw-shred-provider") => {
            ShredTrustMode::TrustedRawShredProvider
        }
        _ => ShredTrustMode::PublicUntrusted,
    }
}

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

pub fn read_runtime_current_thread() -> bool {
    read_bool_env("SOF_RUNTIME_CURRENT_THREAD", false)
}

pub fn read_runtime_delivery_profile() -> RuntimeDeliveryProfile {
    read_env_var("SOF_RUNTIME_DELIVERY_PROFILE")
        .filter(|value| !value.trim().is_empty())
        .as_deref()
        .and_then(RuntimeDeliveryProfile::from_config_value)
        .unwrap_or_default()
}

pub fn read_runtime_core() -> Option<usize> {
    read_env_var("SOF_RUNTIME_CORE").and_then(|value| value.parse::<usize>().ok())
}

pub fn read_dataset_workers() -> usize {
    read_env_var("SOF_DATASET_WORKERS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(read_worker_threads)
}

pub fn read_packet_workers() -> usize {
    read_env_var("SOF_PACKET_WORKERS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(read_worker_threads)
}

pub fn read_packet_worker_queue_capacity() -> usize {
    read_env_var("SOF_PACKET_WORKER_QUEUE_CAPACITY")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(65_536))
        .unwrap_or(256)
}

pub fn read_packet_worker_batch_max_packets() -> usize {
    read_env_var("SOF_PACKET_WORKER_BATCH_MAX_PACKETS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(256))
        .unwrap_or(8)
}

pub fn read_dataset_max_tracked_slots() -> usize {
    read_env_var("SOF_DATASET_MAX_TRACKED_SLOTS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.clamp(128, 65_536))
        .unwrap_or(2_048)
}

pub fn read_dataset_retained_slot_lag() -> u64 {
    read_env_var("SOF_DATASET_RETAINED_SLOT_LAG")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.clamp(32, 8_192))
        .unwrap_or_else(|| {
            u64::try_from(read_dataset_max_tracked_slots() / 4)
                .unwrap_or(u64::MAX)
                .clamp(64, 256)
        })
}

pub fn read_fec_max_tracked_sets() -> usize {
    read_env_var("SOF_FEC_MAX_TRACKED_SETS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.clamp(512, 131_072))
        .unwrap_or(8_192)
}

pub fn read_fec_retained_slot_lag() -> u64 {
    read_env_var("SOF_FEC_RETAINED_SLOT_LAG")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.clamp(16, 8_192))
        .unwrap_or_else(|| {
            u64::try_from(read_fec_max_tracked_sets() / 16)
                .unwrap_or(u64::MAX)
                .clamp(32, 128)
        })
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

pub fn read_skip_vote_only_tx_detail_path() -> bool {
    read_bool_env("SOF_SKIP_VOTE_ONLY_TX_DETAIL_PATH", false)
}

pub fn read_inline_transaction_dispatch_override() -> Option<bool> {
    read_env_var("SOF_INLINE_TRANSACTION_DISPATCH")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
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
    read_optional_bool_env("SOF_VERIFY_SHREDS")
        .unwrap_or_else(|| matches!(read_shred_trust_mode(), ShredTrustMode::PublicUntrusted))
}

pub fn read_verify_strict_unknown() -> bool {
    read_bool_env("SOF_VERIFY_STRICT", false)
}

pub fn read_verify_recovered_shreds() -> bool {
    read_optional_bool_env("SOF_VERIFY_RECOVERED_SHREDS")
        .unwrap_or_else(|| matches!(read_shred_trust_mode(), ShredTrustMode::PublicUntrusted))
}

pub fn read_runtime_dataset_decode_failures_unhealthy_per_tick() -> u64 {
    read_env_var("SOF_RUNTIME_DATASET_DECODE_FAILURES_UNHEALTHY_PER_TICK")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(32)
}

pub fn read_runtime_dataset_tail_skips_unhealthy_per_tick() -> u64 {
    read_env_var("SOF_RUNTIME_DATASET_TAIL_SKIPS_UNHEALTHY_PER_TICK")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(256)
}

pub fn read_runtime_dataset_unhealthy_sustain_ticks() -> u64 {
    read_env_var("SOF_RUNTIME_DATASET_UNHEALTHY_SUSTAIN_TICKS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(3)
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

pub fn read_runtime_extension_queue_depth_warn() -> u64 {
    read_env_var("SOF_RUNTIME_EXTENSION_QUEUE_DEPTH_WARN")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(4_096)
}

pub fn read_runtime_extension_dispatch_lag_warn_us() -> u64 {
    read_env_var("SOF_RUNTIME_EXTENSION_DISPATCH_LAG_WARN_US")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(50_000)
}

pub fn read_runtime_extension_drop_warn_delta() -> u64 {
    read_env_var("SOF_RUNTIME_EXTENSION_DROP_WARN_DELTA")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(100)
}

pub fn read_derived_state_checkpoint_interval_ms() -> u64 {
    read_env_var("SOF_DERIVED_STATE_CHECKPOINT_INTERVAL_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30_000)
}

pub fn read_derived_state_recovery_interval_ms() -> u64 {
    read_env_var("SOF_DERIVED_STATE_RECOVERY_INTERVAL_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5_000)
}

pub fn read_derived_state_replay_max_envelopes() -> usize {
    read_env_var("SOF_DERIVED_STATE_REPLAY_MAX_ENVELOPES")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(8_192)
}

pub fn read_derived_state_replay_max_sessions() -> usize {
    read_env_var("SOF_DERIVED_STATE_REPLAY_MAX_SESSIONS")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(4)
}

pub fn read_derived_state_replay_backend() -> DerivedStateReplayBackend {
    read_env_var("SOF_DERIVED_STATE_REPLAY_BACKEND")
        .filter(|value| !value.trim().is_empty())
        .as_deref()
        .and_then(DerivedStateReplayBackend::from_config_value)
        .unwrap_or_default()
}

pub fn read_derived_state_replay_dir() -> PathBuf {
    read_env_var("SOF_DERIVED_STATE_REPLAY_DIR")
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".sof-derived-state-replay"))
}

pub fn read_derived_state_replay_durability() -> DerivedStateReplayDurability {
    read_env_var("SOF_DERIVED_STATE_REPLAY_DURABILITY")
        .filter(|value| !value.trim().is_empty())
        .as_deref()
        .and_then(DerivedStateReplayDurability::from_config_value)
        .unwrap_or_default()
}

pub fn read_derived_state_runtime_config() -> DerivedStateRuntimeConfig {
    DerivedStateRuntimeConfig {
        checkpoint_interval_ms: read_derived_state_checkpoint_interval_ms(),
        recovery_interval_ms: read_derived_state_recovery_interval_ms(),
        replay: DerivedStateReplayConfig {
            backend: read_derived_state_replay_backend(),
            replay_dir: read_derived_state_replay_dir(),
            durability: read_derived_state_replay_durability(),
            max_envelopes: read_derived_state_replay_max_envelopes(),
            max_sessions: read_derived_state_replay_max_sessions(),
        },
    }
}
