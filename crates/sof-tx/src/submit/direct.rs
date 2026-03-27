//! Direct-submit transport implementation.

use std::{
    collections::HashSet,
    fmt,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use sof_types::PubkeyBytes;
use solana_connection_cache::{
    connection_cache::NewConnectionConfig, nonblocking::client_connection::ClientConnection,
};
use solana_quic_client::{QuicConfig, QuicConnectionCache, QuicConnectionManager};
use tokio::{
    net::UdpSocket,
    task::JoinSet,
    time::{sleep, timeout},
};

use super::{DirectSubmitConfig, DirectSubmitTransport, SubmitTransportError};
use crate::{providers::LeaderTarget, routing::RoutingPolicy};

/// UDP-based direct transport that sends transaction bytes to TPU targets.
#[derive(Clone)]
pub struct UdpDirectTransport {
    /// Optional shared QUIC connection cache enabled by environment.
    quic_cache: Option<Arc<QuicConnectionCache>>,
}

impl fmt::Debug for UdpDirectTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UdpDirectTransport")
            .field("quic_enabled", &self.quic_cache.is_some())
            .finish()
    }
}

impl Default for UdpDirectTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl UdpDirectTransport {
    /// Creates a direct transport with optional shared QUIC cache.
    #[must_use]
    pub fn new() -> Self {
        let quic_enabled = std::env::var("SOF_TX_ENABLE_QUIC_DIRECT")
            .map(|value| {
                let normalized = value.trim().to_ascii_lowercase();
                matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false);
        Self {
            quic_cache: if quic_enabled {
                quic_connection_cache().ok().map(Arc::new)
            } else {
                None
            },
        }
    }
}

/// Number of pooled QUIC connections per target.
const QUIC_CONNECTION_POOL_SIZE: usize = 1;
/// Name tag used by connection-cache metrics.
const QUIC_CACHE_NAME: &str = "sof-tx-direct-quic";
/// Agave QUIC port offset used by TPU clients when `tpu_quic` is unavailable.
const AGAVE_QUIC_PORT_OFFSET: u16 = 6;
/// Minimum UDP send successes before accepting non-QUIC propagation.
const MIN_UDP_SUCCESSES_FOR_ACCEPT: u64 = 16;
/// Minimum QUIC send successes required before accepting direct propagation.
const MIN_QUIC_SUCCESSES_FOR_ACCEPT: u64 = 2;
/// Minimum distinct QUIC targets (identity/address) required before accepting propagation.
const MIN_DISTINCT_QUIC_TARGETS_FOR_ACCEPT: usize = 2;
/// Maximum number of QUIC-candidate targets per attempt, as a multiple of parallel send width.
const QUIC_CANDIDATE_PARALLEL_MULTIPLIER: usize = 8;

/// Per-target send outcome collected from concurrent UDP/QUIC attempts.
#[derive(Debug, Clone)]
struct TargetSendResult {
    /// The target that was attempted.
    target: LeaderTarget,
    /// Whether the UDP send call completed successfully.
    udp_success: bool,
    /// Whether at least one QUIC candidate send completed successfully.
    quic_success: bool,
}

#[async_trait]
impl DirectSubmitTransport for UdpDirectTransport {
    async fn submit_direct(
        &self,
        tx_bytes: &[u8],
        targets: &[LeaderTarget],
        policy: RoutingPolicy,
        config: &DirectSubmitConfig,
    ) -> Result<LeaderTarget, SubmitTransportError> {
        let config = config.clone().normalized();
        if targets.is_empty() {
            return Err(SubmitTransportError::Config {
                message: "no targets provided".to_owned(),
            });
        }

        let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await.map_err(|error| {
            SubmitTransportError::Failure {
                message: error.to_string(),
            }
        })?);
        let quic_cache = self.quic_cache.clone();
        let payload: Arc<[u8]> = Arc::from(tx_bytes.to_vec());
        let quic_enabled = quic_cache.is_some();
        let effective_global_timeout = if quic_enabled {
            config.global_timeout.max(Duration::from_secs(4))
        } else {
            config.global_timeout
        };

        let deadline = Instant::now()
            .checked_add(effective_global_timeout)
            .ok_or_else(|| SubmitTransportError::Failure {
                message: "failed to calculate direct-submit deadline".to_owned(),
            })?;
        let normalized_policy = policy.normalized();
        let quic_timeout = quic_timeout(config.per_target_timeout);
        let quic_candidate_count = targets.len().min(
            normalized_policy
                .max_parallel_sends
                .saturating_mul(QUIC_CANDIDATE_PARALLEL_MULTIPLIER)
                .max(32),
        );
        let available_distinct_quic_targets = targets
            .get(..quic_candidate_count)
            .map_or(0, count_distinct_quic_targets);
        let required_quic_successes =
            required_quic_successes(quic_candidate_count, available_distinct_quic_targets);
        let required_distinct_quic_targets =
            required_distinct_quic_targets(available_distinct_quic_targets);
        let mut udp_successes = 0_u64;
        let mut first_udp_success = None::<LeaderTarget>;
        let mut quic_successes = 0_u64;
        let mut first_quic_success = None::<LeaderTarget>;
        let mut quic_success_identities = HashSet::new();
        let mut quic_success_addrs = HashSet::new();

        for round in 0..config.direct_target_rounds {
            if round > 0 {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let remaining = deadline.saturating_duration_since(now);
                let sleep_for = remaining.min(config.rebroadcast_interval);
                if !sleep_for.is_zero() {
                    sleep(sleep_for).await;
                }
            }
            let mut target_index = 0_usize;
            for chunk in targets.chunks(normalized_policy.max_parallel_sends) {
                let now = Instant::now();
                if now >= deadline {
                    if quic_cache.is_none()
                        && let Some(target) = first_udp_success
                    {
                        return Ok(target);
                    }
                    break;
                }
                let remaining = deadline.saturating_duration_since(now);
                let per_target_udp_timeout = remaining.min(config.per_target_timeout);
                let per_target_quic_timeout = remaining.min(quic_timeout);
                let mut in_flight = JoinSet::new();

                for target in chunk {
                    let socket = Arc::clone(&socket);
                    let payload = Arc::clone(&payload);
                    let target = target.clone();
                    let quic_cache = quic_cache.clone();
                    let use_quic = quic_cache.is_some() && target_index < quic_candidate_count;
                    target_index = target_index.saturating_add(1);
                    in_flight.spawn(async move {
                        send_target(
                            socket,
                            payload,
                            target,
                            per_target_udp_timeout,
                            per_target_quic_timeout,
                            quic_cache,
                            use_quic,
                        )
                        .await
                    });
                }

                while let Some(result) = in_flight.join_next().await {
                    if let Ok(send_result) = result {
                        if send_result.udp_success {
                            udp_successes = udp_successes.saturating_add(1);
                            if first_udp_success.is_none() {
                                first_udp_success = Some(send_result.target.clone());
                            }
                        }
                        if send_result.quic_success {
                            quic_successes = quic_successes.saturating_add(1);
                            if first_quic_success.is_none() {
                                first_quic_success = Some(send_result.target.clone());
                            }
                            if let Some(identity) = send_result.target.identity {
                                let _ = quic_success_identities.insert(identity);
                            } else {
                                let _ = quic_success_addrs.insert(send_result.target.tpu_addr);
                            }
                            let distinct_quic_targets = quic_success_identities
                                .len()
                                .saturating_add(quic_success_addrs.len());
                            if quic_successes >= required_quic_successes
                                && distinct_quic_targets >= required_distinct_quic_targets
                                && let Some(target) = first_quic_success.clone()
                            {
                                return Ok(target);
                            }
                        }
                        if quic_cache.is_none()
                            && udp_successes >= MIN_UDP_SUCCESSES_FOR_ACCEPT
                            && let Some(target) = first_udp_success.clone()
                        {
                            return Ok(target);
                        }
                    }
                }
            }
        }

        if quic_cache.is_some() {
            let distinct_quic_targets = quic_success_identities
                .len()
                .saturating_add(quic_success_addrs.len());
            if quic_successes >= required_quic_successes
                && distinct_quic_targets >= required_distinct_quic_targets
                && let Some(target) = first_quic_success
            {
                return Ok(target);
            }
            if let Some(target) = first_udp_success {
                return Ok(target);
            }
            return Err(SubmitTransportError::Failure {
                message: format!(
                    "direct propagation threshold not met (quic_successes={quic_successes}, distinct_quic_targets={distinct_quic_targets}, required_quic_successes={required_quic_successes}, required_distinct_quic_targets={required_distinct_quic_targets}, udp_successes={udp_successes}, quic_candidates={quic_candidate_count}, timeout_ms={})",
                    effective_global_timeout.as_millis()
                ),
            });
        }

        if let Some(target) = first_udp_success {
            return Ok(target);
        }

        Err(SubmitTransportError::Failure {
            message: format!(
                "all direct targets failed (udp_successes={udp_successes}, quic_successes=0, quic_candidates={quic_candidate_count})"
            ),
        })
    }
}

/// Builds the shared QUIC connection cache used for optional direct sends.
fn quic_connection_cache() -> Result<QuicConnectionCache, SubmitTransportError> {
    let config = QuicConfig::new().map_err(|error| SubmitTransportError::Failure {
        message: format!("failed to create quic config: {error}"),
    })?;
    let manager = QuicConnectionManager::new_with_connection_config(config);
    QuicConnectionCache::new(QUIC_CACHE_NAME, manager, QUIC_CONNECTION_POOL_SIZE).map_err(|error| {
        SubmitTransportError::Failure {
            message: format!("failed to create quic connection cache: {error}"),
        }
    })
}

/// Expands short caller timeouts so QUIC handshakes have a usable floor.
fn quic_timeout(per_target_timeout: Duration) -> Duration {
    let minimum = Duration::from_millis(1_000);
    if per_target_timeout < minimum {
        minimum
    } else {
        per_target_timeout
    }
}

/// Counts the distinct identities or addresses in the QUIC candidate set.
fn count_distinct_quic_targets(targets: &[LeaderTarget]) -> usize {
    targets
        .iter()
        .map(|target| {
            target.identity.map_or(
                DistinctTargetKey::Addr(target.tpu_addr),
                DistinctTargetKey::Identity,
            )
        })
        .collect::<HashSet<_>>()
        .len()
}

/// Scales the QUIC success requirement down for small candidate sets.
fn required_quic_successes(candidate_count: usize, available_distinct_targets: usize) -> u64 {
    let required_by_candidates = u64::try_from(candidate_count).unwrap_or(u64::MAX);
    let required_by_distinct = u64::try_from(available_distinct_targets).unwrap_or(u64::MAX);
    MIN_QUIC_SUCCESSES_FOR_ACCEPT
        .min(required_by_candidates.max(1))
        .min(required_by_distinct.max(1))
}

/// Scales the distinct-target threshold down for sparse target sets.
fn required_distinct_quic_targets(available_distinct_targets: usize) -> usize {
    MIN_DISTINCT_QUIC_TARGETS_FOR_ACCEPT.min(available_distinct_targets.max(1))
}

/// Key used to count unique QUIC targets using the same identity/address semantics as submits.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
enum DistinctTargetKey {
    /// Deduplicate targets that share the same validator identity.
    Identity(PubkeyBytes),
    /// Fallback key for targets without a known validator identity.
    Addr(SocketAddr),
}

/// Sends one payload to a target over UDP and optionally QUIC in parallel.
async fn send_target(
    socket: Arc<UdpSocket>,
    payload: Arc<[u8]>,
    target: LeaderTarget,
    udp_timeout: Duration,
    quic_timeout: Duration,
    quic_cache: Option<Arc<QuicConnectionCache>>,
    use_quic: bool,
) -> TargetSendResult {
    let udp_success = matches!(
        timeout(
            udp_timeout,
            socket.send_to(payload.as_ref(), target.tpu_addr)
        )
        .await,
        Ok(Ok(_))
    );

    let quic_success = if use_quic {
        send_quic(quic_cache, payload.as_ref(), target.tpu_addr, quic_timeout).await
    } else {
        false
    };

    TargetSendResult {
        target,
        udp_success,
        quic_success,
    }
}

/// Attempts QUIC sends against the target's candidate QUIC addresses.
async fn send_quic(
    quic_cache: Option<Arc<QuicConnectionCache>>,
    payload: &[u8],
    target: SocketAddr,
    timeout_budget: Duration,
) -> bool {
    let Some(quic_cache) = quic_cache else {
        return false;
    };
    let candidate_addrs = quic_candidate_addrs(target);
    let payload: Arc<[u8]> = Arc::from(payload.to_vec());
    let mut in_flight = JoinSet::new();
    for addr in candidate_addrs {
        let connection = quic_cache.get_nonblocking_connection(&addr);
        let payload = Arc::clone(&payload);
        in_flight.spawn(async move {
            matches!(
                timeout(timeout_budget, connection.send_data(payload.as_ref())).await,
                Ok(Ok(()))
            )
        });
    }
    while let Some(result) = in_flight.join_next().await {
        if matches!(result, Ok(true)) {
            in_flight.abort_all();
            return true;
        }
    }
    false
}

/// Expands one TPU address into the candidate QUIC addresses to probe.
fn quic_candidate_addrs(target: SocketAddr) -> Vec<SocketAddr> {
    let mut addrs = Vec::with_capacity(2);
    addrs.push(target);
    if let Some(quic_fallback) = with_agave_quic_fallback(target)
        && quic_fallback != target
    {
        addrs.push(quic_fallback);
    }
    addrs
}

/// Applies the standard Agave TPU-to-QUIC port offset when it fits in `u16`.
fn with_agave_quic_fallback(addr: SocketAddr) -> Option<SocketAddr> {
    let mut quic_addr = addr;
    let port = quic_addr.port().checked_add(AGAVE_QUIC_PORT_OFFSET)?;
    quic_addr.set_port(port);
    Some(quic_addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quic_requirements_scale_down_for_single_target() {
        assert_eq!(required_quic_successes(1, 1), 1);
        assert_eq!(required_distinct_quic_targets(1), 1);
    }

    #[test]
    fn quic_requirements_keep_default_for_multi_target_sets() {
        assert_eq!(required_quic_successes(4, 4), MIN_QUIC_SUCCESSES_FOR_ACCEPT);
        assert_eq!(
            required_distinct_quic_targets(4),
            MIN_DISTINCT_QUIC_TARGETS_FOR_ACCEPT
        );
    }

    #[test]
    fn quic_distinct_target_count_deduplicates_same_identity() {
        let identity = PubkeyBytes::from_solana(solana_pubkey::Pubkey::new_unique());
        let targets = vec![
            LeaderTarget::new(Some(identity), SocketAddr::from(([127, 0, 0, 1], 9001))),
            LeaderTarget::new(Some(identity), SocketAddr::from(([127, 0, 0, 1], 9002))),
        ];
        assert_eq!(count_distinct_quic_targets(&targets), 1);
    }
}
