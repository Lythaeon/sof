use super::*;
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

#[derive(Debug, Error)]
pub(super) enum RelayRuntimeError {
    #[error("invalid SOF_RELAY_CONNECT address `{value}`: {source}")]
    InvalidRelayConnect {
        value: String,
        source: ResolveSocketAddrError,
    },
    #[error("SOF_RELAY_LISTEN set but relay bus unavailable for {bind_addr}")]
    MissingRelayBus { bind_addr: SocketAddr },
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

pub(super) fn read_relay_connect_addrs() -> Result<Vec<SocketAddr>, RelayRuntimeError> {
    read_env_var("SOF_RELAY_CONNECT").map_or_else(
        || Ok(Vec::new()),
        |value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|segment| !segment.is_empty())
                .map(|segment| {
                    resolve_socket_addr(segment).map_err(|source| {
                        RelayRuntimeError::InvalidRelayConnect {
                            value: segment.to_owned(),
                            source,
                        }
                    })
                })
                .collect()
        },
    )
}

pub(super) fn maybe_start_relay_server(
    relay_bus: Option<broadcast::Sender<Vec<u8>>>,
    relay_listen_addr: Option<SocketAddr>,
) -> Result<Option<JoinHandle<()>>, RelayRuntimeError> {
    match (relay_listen_addr, relay_bus) {
        (Some(bind_addr), Some(packet_bus)) => {
            tracing::info!(%bind_addr, "starting tcp relay server mode");
            Ok(Some(crate::relay::spawn_tcp_relay_server(
                bind_addr, packet_bus,
            )))
        }
        (Some(bind_addr), None) => Err(RelayRuntimeError::MissingRelayBus { bind_addr }),
        (None, _) => Ok(None),
    }
}
