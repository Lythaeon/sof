use super::*;
use thiserror::Error;

const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8001";

#[derive(Debug, Error)]
pub(super) enum BindAddressError {
    #[error("failed to parse default bind address `{value}`: {source}")]
    DefaultAddress {
        value: &'static str,
        source: std::net::AddrParseError,
    },
    #[error("invalid SOF_BIND address `{value}`: {source}")]
    InvalidAddress {
        value: String,
        source: std::net::AddrParseError,
    },
}

pub(super) fn read_bind_addr() -> Result<SocketAddr, BindAddressError> {
    read_env_var("SOF_BIND").map_or_else(
        || {
            SocketAddr::from_str(DEFAULT_BIND_ADDR).map_err(|source| {
                BindAddressError::DefaultAddress {
                    value: DEFAULT_BIND_ADDR,
                    source,
                }
            })
        },
        |value| {
            SocketAddr::from_str(&value)
                .map_err(|source| BindAddressError::InvalidAddress { value, source })
        },
    )
}
