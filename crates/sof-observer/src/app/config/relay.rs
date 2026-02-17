use std::{
    net::{AddrParseError, SocketAddr},
    str::FromStr,
};

use super::read_env_var;
use thiserror::Error;

/// Errors returned when parsing `SOF_RELAY_LISTEN`.
#[derive(Debug, Error)]
pub enum RelayListenAddressError {
    /// `SOF_RELAY_LISTEN` could not be parsed as a socket address.
    #[error("invalid SOF_RELAY_LISTEN address `{value}`: {source}")]
    InvalidAddress {
        value: String,
        source: AddrParseError,
    },
}

/// Reads the optional relay listen bind address from `SOF_RELAY_LISTEN`.
pub fn read_relay_listen_addr() -> Result<Option<SocketAddr>, RelayListenAddressError> {
    read_env_var("SOF_RELAY_LISTEN").map_or_else(
        || Ok(None),
        |value| {
            SocketAddr::from_str(&value)
                .map(Some)
                .map_err(|source| RelayListenAddressError::InvalidAddress { value, source })
        },
    )
}
