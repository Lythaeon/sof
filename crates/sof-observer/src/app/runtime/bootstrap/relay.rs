use super::*;
use std::net::ToSocketAddrs;
use thiserror::Error;

#[derive(Debug, Error)]
pub(super) enum ResolveSocketAddrError {
    #[error("invalid socket address or hostname `{value}`: {source}")]
    InvalidAddress {
        value: String,
        source: std::io::Error,
    },
    #[error("failed to resolve socket address or hostname `{value}`")]
    UnresolvedAddress { value: String },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub(super) struct ResolvedSocketAddr(SocketAddr);

impl ResolvedSocketAddr {
    #[must_use]
    pub const fn into_inner(self) -> SocketAddr {
        self.0
    }
}

impl TryFrom<&str> for ResolvedSocketAddr {
    type Error = ResolveSocketAddrError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        if let Ok(addr) = SocketAddr::from_str(raw) {
            return Ok(Self(addr));
        }
        let mut resolved =
            raw.to_socket_addrs()
                .map_err(|source| ResolveSocketAddrError::InvalidAddress {
                    value: raw.to_owned(),
                    source,
                })?;
        resolved
            .next()
            .map(Self)
            .ok_or_else(|| ResolveSocketAddrError::UnresolvedAddress {
                value: raw.to_owned(),
            })
    }
}

pub(super) fn resolve_socket_addr(raw: &str) -> Result<SocketAddr, ResolveSocketAddrError> {
    ResolvedSocketAddr::try_from(raw).map(ResolvedSocketAddr::into_inner)
}
