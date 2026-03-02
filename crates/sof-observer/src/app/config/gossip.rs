#[cfg(feature = "gossip-bootstrap")]
use std::collections::HashSet;
#[cfg(feature = "gossip-bootstrap")]
use std::num::NonZeroUsize;
#[cfg(feature = "gossip-bootstrap")]
use std::num::ParseIntError;

#[cfg(feature = "gossip-bootstrap")]
use solana_net_utils::PortRange;
#[cfg(feature = "gossip-bootstrap")]
use solana_pubkey::Pubkey;
#[cfg(feature = "gossip-bootstrap")]
use thiserror::Error;

#[cfg(not(feature = "gossip-bootstrap"))]
use super::read_env_var;
#[cfg(feature = "gossip-bootstrap")]
use super::{read_bool_env, read_env_var};

#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_MAINNET_GOSSIP_ENTRYPOINT_HOSTNAME: &str = "entrypoint.mainnet-beta.solana.com:8001";
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_MAINNET_GOSSIP_ENTRYPOINT_IPS: [&str; 4] = [
    "104.204.142.108:8001",
    "64.130.54.173:8001",
    "85.195.118.195:8001",
    "160.202.131.177:8001",
];
// Runtime defaults for gossip mode; keep these aligned with docs/operations/advanced-env.md.
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_PORT_RANGE_START: u16 = 12_000;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_PORT_RANGE_END: u16 = 12_100;
#[cfg(feature = "gossip-bootstrap")]
const TVU_SOCKETS_FALLBACK_PARALLELISM: usize = 1;
#[cfg(feature = "gossip-bootstrap")]
const TVU_SOCKETS_MIN: usize = 1;
#[cfg(feature = "gossip-bootstrap")]
const TVU_SOCKETS_MAX: usize = 16;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_RUNTIME_SWITCH_STALL_MS: u64 = 2_000;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_RUNTIME_SWITCH_DATASET_STALL_MS: u64 = 12_000;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_RUNTIME_SWITCH_COOLDOWN_MS: u64 = 15_000;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_RUNTIME_SWITCH_WARMUP_MS: u64 = 10_000;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_RUNTIME_SWITCH_OVERLAP_MS: u64 = 1_250;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_RUNTIME_SWITCH_SUSTAIN_MS: u64 = 1_500;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_RUNTIME_SWITCH_NO_TRAFFIC_GRACE_MS: u64 = 120_000;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_RUNTIME_SWITCH_STABILIZE_MS: u64 = 1_000;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_RUNTIME_SWITCH_STABILIZE_MIN_PACKETS: u64 = 8;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_RUNTIME_SWITCH_STABILIZE_MAX_WAIT_MS: u64 = 8_000;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_BOOTSTRAP_STABILIZE_MAX_WAIT_MS: u64 = 20_000;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_BOOTSTRAP_STABILIZE_MIN_PEERS: usize = 128;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_RUNTIME_SWITCH_PEER_CANDIDATES: usize = 64;
#[cfg(feature = "gossip-bootstrap")]
const GOSSIP_VALIDATORS_MAX: usize = 2_048;

fn parse_comma_separated_endpoints(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(feature = "gossip-bootstrap")]
/// Errors returned when parsing `SOF_PORT_RANGE`.
#[derive(Debug, Error)]
pub enum PortRangeParseError {
    /// `SOF_PORT_RANGE` did not contain a `start-end` pair.
    #[error("invalid SOF_PORT_RANGE `{value}`, expected start-end")]
    InvalidFormat { value: String },
    /// `SOF_PORT_RANGE` start value was not a valid `u16`.
    #[error("invalid SOF_PORT_RANGE start `{value}`: {source}")]
    InvalidStart {
        value: String,
        source: ParseIntError,
    },
    /// `SOF_PORT_RANGE` end value was not a valid `u16`.
    #[error("invalid SOF_PORT_RANGE end `{value}`: {source}")]
    InvalidEnd {
        value: String,
        source: ParseIntError,
    },
    /// `SOF_PORT_RANGE` start value was greater than end value.
    #[error("invalid SOF_PORT_RANGE, start must be <= end (got {start}-{end})")]
    StartGreaterThanEnd { start: u16, end: u16 },
}

#[cfg(feature = "gossip-bootstrap")]
/// Reads the gossip/runtime bind range from `SOF_PORT_RANGE`.
pub fn read_port_range() -> Result<PortRange, PortRangeParseError> {
    match read_env_var("SOF_PORT_RANGE") {
        Some(value) => {
            let (start, end) =
                value
                    .split_once('-')
                    .ok_or_else(|| PortRangeParseError::InvalidFormat {
                        value: value.clone(),
                    })?;
            let start =
                start
                    .parse::<u16>()
                    .map_err(|source| PortRangeParseError::InvalidStart {
                        value: start.to_owned(),
                        source,
                    })?;
            let end = end
                .parse::<u16>()
                .map_err(|source| PortRangeParseError::InvalidEnd {
                    value: end.to_owned(),
                    source,
                })?;
            if start > end {
                return Err(PortRangeParseError::StartGreaterThanEnd { start, end });
            }
            Ok((start, end))
        }
        None => Ok((DEFAULT_PORT_RANGE_START, DEFAULT_PORT_RANGE_END)),
    }
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_port() -> Option<u16> {
    read_env_var("SOF_GOSSIP_PORT").and_then(|value| value.parse::<u16>().ok())
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_shred_version_override() -> Option<u16> {
    read_env_var("SOF_SHRED_VERSION").and_then(|value| value.parse::<u16>().ok())
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_tvu_receive_sockets() -> usize {
    // Keep fanout bounded to avoid over-fragmenting packet ingestion on large hosts.
    let suggested = std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(TVU_SOCKETS_FALLBACK_PARALLELISM)
        .clamp(TVU_SOCKETS_MIN, TVU_SOCKETS_MAX);
    read_env_var("SOF_TVU_SOCKETS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(suggested)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_entrypoints() -> Vec<String> {
    read_env_var("SOF_GOSSIP_ENTRYPOINT").map_or_else(
        || {
            let mut defaults = Vec::with_capacity(
                DEFAULT_MAINNET_GOSSIP_ENTRYPOINT_IPS
                    .len()
                    .saturating_add(1),
            );
            defaults.extend(
                DEFAULT_MAINNET_GOSSIP_ENTRYPOINT_IPS
                    .iter()
                    .map(|entrypoint| (*entrypoint).to_owned()),
            );
            defaults.push(DEFAULT_MAINNET_GOSSIP_ENTRYPOINT_HOSTNAME.to_owned());
            defaults
        },
        |value| parse_comma_separated_endpoints(&value),
    )
}

#[cfg(not(feature = "gossip-bootstrap"))]
pub fn read_gossip_entrypoints() -> Vec<String> {
    read_env_var("SOF_GOSSIP_ENTRYPOINT")
        .map_or_else(Vec::new, |value| parse_comma_separated_endpoints(&value))
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_entrypoint_probe_enabled() -> bool {
    read_bool_env("SOF_GOSSIP_ENTRYPOINT_PROBE", false)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_validators() -> Option<HashSet<Pubkey>> {
    let validators = read_env_var("SOF_GOSSIP_VALIDATORS")
        .map(|value| {
            parse_comma_separated_endpoints(&value)
                .into_iter()
                .take(GOSSIP_VALIDATORS_MAX)
                .filter_map(|value| value.parse::<Pubkey>().ok())
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    (!validators.is_empty()).then_some(validators)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_runtime_switch_enabled() -> bool {
    read_bool_env("SOF_GOSSIP_RUNTIME_SWITCH_ENABLED", false)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_runtime_switch_stall_ms() -> u64 {
    read_env_var("SOF_GOSSIP_RUNTIME_SWITCH_STALL_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_RUNTIME_SWITCH_STALL_MS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_runtime_switch_dataset_stall_ms() -> u64 {
    read_env_var("SOF_GOSSIP_RUNTIME_SWITCH_DATASET_STALL_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_RUNTIME_SWITCH_DATASET_STALL_MS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_runtime_switch_cooldown_ms() -> u64 {
    read_env_var("SOF_GOSSIP_RUNTIME_SWITCH_COOLDOWN_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_RUNTIME_SWITCH_COOLDOWN_MS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_runtime_switch_warmup_ms() -> u64 {
    read_env_var("SOF_GOSSIP_RUNTIME_SWITCH_WARMUP_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_RUNTIME_SWITCH_WARMUP_MS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_runtime_switch_overlap_ms() -> u64 {
    read_env_var("SOF_GOSSIP_RUNTIME_SWITCH_OVERLAP_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_RUNTIME_SWITCH_OVERLAP_MS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_runtime_switch_port_range() -> Option<PortRange> {
    read_env_var("SOF_GOSSIP_RUNTIME_SWITCH_PORT_RANGE")
        .and_then(|value| {
            value
                .split_once('-')
                .map(|(start, end)| (start.to_owned(), end.to_owned()))
        })
        .and_then(|(start, end)| {
            let start = start.parse::<u16>().ok()?;
            let end = end.parse::<u16>().ok()?;
            (start <= end).then_some((start, end))
        })
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_runtime_switch_sustain_ms() -> u64 {
    read_env_var("SOF_GOSSIP_RUNTIME_SWITCH_SUSTAIN_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_RUNTIME_SWITCH_SUSTAIN_MS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_runtime_switch_no_traffic_grace_ms() -> u64 {
    read_env_var("SOF_GOSSIP_RUNTIME_SWITCH_NO_TRAFFIC_GRACE_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_RUNTIME_SWITCH_NO_TRAFFIC_GRACE_MS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_runtime_switch_stabilize_ms() -> u64 {
    read_env_var("SOF_GOSSIP_RUNTIME_SWITCH_STABILIZE_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_RUNTIME_SWITCH_STABILIZE_MS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_runtime_switch_stabilize_min_packets() -> u64 {
    read_env_var("SOF_GOSSIP_RUNTIME_SWITCH_STABILIZE_MIN_PACKETS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_RUNTIME_SWITCH_STABILIZE_MIN_PACKETS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_runtime_switch_stabilize_max_wait_ms() -> u64 {
    read_env_var("SOF_GOSSIP_RUNTIME_SWITCH_STABILIZE_MAX_WAIT_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_RUNTIME_SWITCH_STABILIZE_MAX_WAIT_MS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_bootstrap_stabilize_max_wait_ms() -> u64 {
    read_env_var("SOF_GOSSIP_BOOTSTRAP_STABILIZE_MAX_WAIT_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_BOOTSTRAP_STABILIZE_MAX_WAIT_MS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_bootstrap_stabilize_min_peers() -> usize {
    read_env_var("SOF_GOSSIP_BOOTSTRAP_STABILIZE_MIN_PEERS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_BOOTSTRAP_STABILIZE_MIN_PEERS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_gossip_runtime_switch_peer_candidates() -> usize {
    read_env_var("SOF_GOSSIP_RUNTIME_SWITCH_PEER_CANDIDATES")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_RUNTIME_SWITCH_PEER_CANDIDATES)
}
