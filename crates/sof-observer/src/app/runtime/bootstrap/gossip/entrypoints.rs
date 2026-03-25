#[cfg(feature = "gossip-bootstrap")]
use super::*;
#[cfg(feature = "gossip-bootstrap")]
use std::net::IpAddr;
#[cfg(feature = "gossip-bootstrap")]
use std::net::ToSocketAddrs;
#[cfg(feature = "gossip-bootstrap")]
use thiserror::Error;

#[cfg(feature = "gossip-bootstrap")]
/// Errors returned while probing gossip entrypoints for liveness/shred version.
#[derive(Debug, Error)]
pub(super) enum GossipEntrypointProbeError {
    /// The entrypoint could not be resolved to a socket address.
    #[error(transparent)]
    ResolveEntrypoint(#[from] crate::app::runtime::bootstrap::relay::ResolveSocketAddrError),
    /// The shred-version RPC probe failed.
    #[error("failed to probe shred version: {reason}")]
    ShredVersionProbe { reason: String },
    /// The blocking probe task failed to join.
    #[error("gossip entrypoint probe task join failed: {source}")]
    TaskJoin { source: tokio::task::JoinError },
}

#[cfg(feature = "gossip-bootstrap")]
async fn probe_entrypoint_shred_version(
    entrypoint: String,
) -> Result<u16, GossipEntrypointProbeError> {
    tokio::task::spawn_blocking(move || {
        let entrypoint_addr = resolve_socket_addr(&entrypoint)?;
        get_cluster_shred_version(&entrypoint_addr)
            .map_err(|reason| GossipEntrypointProbeError::ShredVersionProbe { reason })
    })
    .await
    .map_err(|source| GossipEntrypointProbeError::TaskJoin { source })?
}

#[cfg(feature = "gossip-bootstrap")]
#[derive(Clone, Debug, Default)]
pub(crate) struct GossipEntrypointBias {
    ranked_source_ips: Vec<IpAddr>,
}

#[cfg(feature = "gossip-bootstrap")]
impl GossipEntrypointBias {
    pub(crate) fn new<I>(ranked_source_ips: I) -> Self
    where
        I: IntoIterator<Item = IpAddr>,
    {
        let mut ordered = Vec::new();
        let mut seen = HashSet::new();
        for ip in ranked_source_ips {
            if seen.insert(ip) {
                ordered.push(ip);
            }
        }
        Self {
            ranked_source_ips: ordered,
        }
    }

    fn rank_for_entrypoint(&self, entrypoint: &str) -> Option<usize> {
        let resolved_ip = entrypoint
            .to_socket_addrs()
            .ok()
            .and_then(|mut addrs| addrs.next())
            .map(|addr| addr.ip())?;
        self.ranked_source_ips
            .iter()
            .position(|ip| *ip == resolved_ip)
    }

    fn is_empty(&self) -> bool {
        self.ranked_source_ips.is_empty()
    }
}

#[cfg(feature = "gossip-bootstrap")]
pub(super) async fn prioritize_gossip_entrypoints(
    entrypoints: &[String],
    bias: Option<&GossipEntrypointBias>,
) -> Vec<String> {
    let expanded = expand_gossip_entrypoints(entrypoints);
    if expanded.len() <= 1 || !read_gossip_entrypoint_probe_enabled() {
        return expanded;
    }
    let bias = bias.filter(|bias| !bias.is_empty());
    let mut scored = Vec::with_capacity(expanded.len());
    for entrypoint in &expanded {
        let started_at = Instant::now();
        let result = probe_entrypoint_shred_version(entrypoint.clone()).await;
        let bias_rank = bias.and_then(|bias| bias.rank_for_entrypoint(entrypoint));
        match result {
            Ok(shred_version) => {
                let probe_ms = duration_to_ms_u64(started_at.elapsed());
                tracing::info!(
                    entrypoint = %entrypoint,
                    probe_ms,
                    source_bias_rank = bias_rank.unwrap_or(usize::MAX),
                    shred_version,
                    "gossip entrypoint probe succeeded"
                );
                scored.push((
                    0_u8,
                    bias_rank.unwrap_or(usize::MAX),
                    probe_ms,
                    entrypoint.clone(),
                ));
            }
            Err(error) => {
                tracing::warn!(
                    entrypoint = %entrypoint,
                    source_bias_rank = bias_rank.unwrap_or(usize::MAX),
                    error = %error,
                    "gossip entrypoint probe failed"
                );
                scored.push((
                    1_u8,
                    bias_rank.unwrap_or(usize::MAX),
                    u64::MAX,
                    entrypoint.clone(),
                ));
            }
        }
    }
    scored.sort_unstable_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.3.cmp(&right.3))
    });
    let ordered: Vec<String> = scored
        .into_iter()
        .map(|(_, _, _, entrypoint)| entrypoint)
        .collect();
    tracing::info!(
        order = %ordered.join(","),
        bias = %bias
            .map(|bias| {
                bias.ranked_source_ips
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default(),
        "gossip entrypoint probe order"
    );
    ordered
}

#[cfg(all(test, feature = "gossip-bootstrap"))]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn gossip_entrypoint_bias_dedupes_and_ranks_by_ip() {
        let bias = GossipEntrypointBias::new([
            IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(2, 2, 2, 2)),
            IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
        ]);
        assert_eq!(bias.ranked_source_ips.len(), 2);
        assert_eq!(bias.rank_for_entrypoint("1.1.1.1:8001"), Some(0));
        assert_eq!(bias.rank_for_entrypoint("2.2.2.2:8001"), Some(1));
        assert_eq!(bias.rank_for_entrypoint("3.3.3.3:8001"), None);
    }
}

#[cfg(feature = "gossip-bootstrap")]
pub(super) async fn probe_gossip_entrypoint_live(entrypoint: &str) -> bool {
    probe_entrypoint_shred_version(entrypoint.to_owned())
        .await
        .is_ok()
}

#[cfg(feature = "gossip-bootstrap")]
pub(super) fn expand_gossip_entrypoints(entrypoints: &[String]) -> Vec<String> {
    let mut expanded = Vec::new();
    let mut seen = HashSet::new();
    for entrypoint in entrypoints {
        let mut inserted_any = false;
        if let Ok(resolved) = entrypoint.to_socket_addrs() {
            for addr in resolved {
                let candidate = addr.to_string();
                if seen.insert(candidate.clone()) {
                    expanded.push(candidate);
                }
                inserted_any = true;
            }
        }
        if !inserted_any && seen.insert(entrypoint.clone()) {
            expanded.push(entrypoint.clone());
        }
    }
    expanded
}

#[cfg(feature = "gossip-bootstrap")]
pub(super) fn collect_runtime_switch_entrypoints(
    runtime: &ReceiverRuntime,
    configured_entrypoints: &[String],
    peer_candidates: usize,
) -> Vec<String> {
    let mut candidates = expand_gossip_entrypoints(configured_entrypoints);
    let mut seen: HashSet<String> = candidates.iter().cloned().collect();
    let Some(gossip_runtime) = runtime.gossip_runtime.as_ref() else {
        return candidates;
    };

    let mut peers = gossip_runtime.cluster_info.all_peers();
    peers.sort_unstable_by(|left, right| right.1.cmp(&left.1));
    for (contact_info, _) in peers.into_iter().take(peer_candidates) {
        let Some(gossip_addr) = contact_info.gossip() else {
            continue;
        };
        let candidate = gossip_addr.to_string();
        if seen.insert(candidate.clone()) {
            candidates.push(candidate);
        }
    }
    candidates
}
