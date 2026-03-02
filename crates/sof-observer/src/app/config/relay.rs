use super::read_env_var;

#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_UDP_RELAY_ENABLED: bool = true;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_UDP_RELAY_REFRESH_MS: u64 = 2_000;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_UDP_RELAY_PEER_CANDIDATES: usize = 64;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_UDP_RELAY_FANOUT: usize = 8;
#[cfg(feature = "gossip-bootstrap")]
const UDP_RELAY_FANOUT_CAP: usize = 200;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_UDP_RELAY_MAX_SENDS_PER_SEC: u64 = 1_200;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_UDP_RELAY_MAX_PEERS_PER_IP: usize = 2;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_UDP_RELAY_REQUIRE_TURBINE_SOURCE_PORTS: bool = true;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_UDP_RELAY_SEND_ERROR_BACKOFF_MS: u64 = 1_000;
#[cfg(feature = "gossip-bootstrap")]
const DEFAULT_UDP_RELAY_SEND_ERROR_BACKOFF_THRESHOLD: u64 = 64;

#[cfg(feature = "gossip-bootstrap")]
pub fn read_udp_relay_enabled() -> bool {
    read_env_var("SOF_UDP_RELAY_ENABLED")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(DEFAULT_UDP_RELAY_ENABLED)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_udp_relay_refresh_ms() -> u64 {
    read_env_var("SOF_UDP_RELAY_REFRESH_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_UDP_RELAY_REFRESH_MS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_udp_relay_peer_candidates() -> usize {
    read_env_var("SOF_UDP_RELAY_PEER_CANDIDATES")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(4_096))
        .unwrap_or(DEFAULT_UDP_RELAY_PEER_CANDIDATES)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_udp_relay_fanout() -> usize {
    read_env_var("SOF_UDP_RELAY_FANOUT")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(UDP_RELAY_FANOUT_CAP))
        .unwrap_or(DEFAULT_UDP_RELAY_FANOUT)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_udp_relay_max_sends_per_sec() -> u64 {
    read_env_var("SOF_UDP_RELAY_MAX_SENDS_PER_SEC")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_UDP_RELAY_MAX_SENDS_PER_SEC)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_udp_relay_max_peers_per_ip() -> usize {
    read_env_var("SOF_UDP_RELAY_MAX_PEERS_PER_IP")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(64))
        .unwrap_or(DEFAULT_UDP_RELAY_MAX_PEERS_PER_IP)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_udp_relay_require_turbine_source_ports() -> bool {
    read_env_var("SOF_UDP_RELAY_REQUIRE_TURBINE_SOURCE_PORTS")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(DEFAULT_UDP_RELAY_REQUIRE_TURBINE_SOURCE_PORTS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_udp_relay_send_error_backoff_ms() -> u64 {
    read_env_var("SOF_UDP_RELAY_SEND_ERROR_BACKOFF_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_UDP_RELAY_SEND_ERROR_BACKOFF_MS)
}

#[cfg(feature = "gossip-bootstrap")]
pub fn read_udp_relay_send_error_backoff_threshold() -> u64 {
    read_env_var("SOF_UDP_RELAY_SEND_ERROR_BACKOFF_THRESHOLD")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(10_000))
        .unwrap_or(DEFAULT_UDP_RELAY_SEND_ERROR_BACKOFF_THRESHOLD)
}

pub fn read_relay_cache_window_ms() -> u64 {
    read_env_var("SOF_RELAY_CACHE_WINDOW_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(10_000)
}

pub fn read_relay_cache_max_shreds() -> usize {
    read_env_var("SOF_RELAY_CACHE_MAX_SHREDS")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(2_000_000))
        .unwrap_or(16_384)
}
