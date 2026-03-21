use std::net::SocketAddr;

use super::read_env_var;

/// Reads the runtime-owned observability bind address from `SOF_OBSERVABILITY_BIND`.
#[must_use]
pub fn read_observability_bind_addr() -> Option<SocketAddr> {
    read_env_var("SOF_OBSERVABILITY_BIND").and_then(|value| value.parse::<SocketAddr>().ok())
}
