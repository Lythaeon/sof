use crate::runtime_env::read_env_var;
#[cfg(target_os = "linux")]
use nix::sys::socket::{setsockopt, sockopt::RxqOvfl};

pub(super) fn read_udp_receiver_core(local_port: u16) -> Option<usize> {
    if let Some(core_index) =
        read_env_var("SOF_UDP_RECEIVER_CORE").and_then(|value| value.parse::<usize>().ok())
    {
        return Some(core_index);
    }
    if read_udp_receiver_pin_by_port() {
        return Some(usize::from(local_port));
    }
    None
}

fn read_udp_receiver_pin_by_port() -> bool {
    read_bool_env("SOF_UDP_RECEIVER_PIN_BY_PORT", false)
}

pub(super) fn enable_rxq_ovfl_tracking(socket: &std::net::UdpSocket) -> bool {
    #[cfg(target_os = "linux")]
    {
        if !read_udp_track_rxq_ovfl() {
            return false;
        }
        match setsockopt(socket, RxqOvfl, &1) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "failed to enable SO_RXQ_OVFL telemetry; continuing without kernel drop counter"
                );
                false
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = socket;
        false
    }
}

pub(super) fn read_udp_rcvbuf_bytes() -> Option<usize> {
    read_env_var("SOF_UDP_RCVBUF")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .or(Some(64 * 1024 * 1024))
}

pub(super) fn read_udp_batch_size() -> usize {
    read_env_var("SOF_UDP_BATCH_SIZE")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(64)
}

pub(super) fn read_udp_batch_max_wait_ms() -> u64 {
    read_env_var("SOF_UDP_BATCH_MAX_WAIT_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1)
}

pub(super) fn read_udp_idle_wait_ms() -> u64 {
    read_env_var("SOF_UDP_IDLE_WAIT_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(100)
}

fn read_udp_track_rxq_ovfl() -> bool {
    read_bool_env("SOF_UDP_TRACK_RXQ_OVFL", false)
}

fn read_bool_env(name: &str, default: bool) -> bool {
    read_env_var(name)
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(default)
}
