#[cfg(feature = "gossip-bootstrap")]
use super::*;
#[cfg(feature = "gossip-bootstrap")]
use thiserror::Error;

#[cfg(feature = "gossip-bootstrap")]
pub(super) fn is_bind_conflict_error(error: &impl std::fmt::Display) -> bool {
    let rendered = error.to_string();
    rendered.contains("Address already in use")
        || rendered.contains("bind_to port")
        || rendered.contains("gossip_addr")
}

#[cfg(feature = "gossip-bootstrap")]
pub(super) async fn stop_gossip_runtime_components(
    receiver_handles: Vec<JoinHandle<()>>,
    gossip_runtime: Option<GossipRuntime>,
) {
    for handle in receiver_handles {
        handle.abort();
        if handle.await.is_err() {
            // Receiver task was already aborted/cancelled.
        }
    }
    drop(gossip_runtime);
}

#[cfg(feature = "gossip-bootstrap")]
#[derive(Debug, Clone, Copy)]
pub(super) struct RuntimeStabilization {
    pub(super) stabilized: bool,
    pub(super) elapsed: Duration,
    pub(super) packets_seen: u64,
}

#[cfg(feature = "gossip-bootstrap")]
pub(super) async fn wait_for_runtime_stabilization(
    telemetry: ingest::ReceiverTelemetry,
    sustain: Duration,
    min_packets: u64,
    max_wait: Duration,
) -> RuntimeStabilization {
    let started_at = Instant::now();
    let (baseline_packets, _) = telemetry.snapshot();
    let mut first_packet_at: Option<Instant> = None;
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let (packets, last_packet_unix_ms) = telemetry.snapshot();
        let packets_seen = packets.saturating_sub(baseline_packets);
        if packets_seen > 0 && first_packet_at.is_none() {
            first_packet_at = Some(Instant::now());
        }
        let elapsed = started_at.elapsed();
        let last_packet_age_ms = if last_packet_unix_ms == 0 {
            u64::MAX
        } else {
            current_unix_ms().saturating_sub(last_packet_unix_ms)
        };
        let sustained = first_packet_at
            .map(|first_seen| Instant::now().saturating_duration_since(first_seen) >= sustain)
            .unwrap_or(false);
        let fresh = last_packet_age_ms <= 500;
        if sustained && fresh && packets_seen >= min_packets {
            return RuntimeStabilization {
                stabilized: true,
                elapsed,
                packets_seen,
            };
        }
        if elapsed >= max_wait {
            return RuntimeStabilization {
                stabilized: false,
                elapsed,
                packets_seen,
            };
        }
        ticker.tick().await;
    }
}

#[cfg(feature = "gossip-bootstrap")]
#[derive(Clone, Copy, Debug)]
pub(super) struct GossipRuntimePortPlan {
    pub(super) primary: PortRange,
    pub(super) secondary: Option<PortRange>,
}

#[cfg(feature = "gossip-bootstrap")]
#[derive(Debug, Error)]
pub(super) enum GossipRuntimePortPlanError {
    #[error(transparent)]
    PortRange(#[from] PortRangeParseError),
}

#[cfg(feature = "gossip-bootstrap")]
pub(super) fn build_gossip_runtime_port_plan()
-> Result<GossipRuntimePortPlan, GossipRuntimePortPlanError> {
    let mut primary = read_port_range()?;
    let configured_overlap = read_gossip_runtime_switch_overlap_ms() > 0;
    let secondary = match read_gossip_runtime_switch_port_range() {
        Some(candidate)
            if !port_ranges_overlap(primary, candidate)
                && range_contains_port(candidate, read_gossip_port().unwrap_or(candidate.0)) =>
        {
            Some(candidate)
        }
        Some(candidate) => {
            tracing::warn!(
                primary_range = %format_port_range(primary),
                candidate_range = %format_port_range(candidate),
                "ignoring SOF_GOSSIP_RUNTIME_SWITCH_PORT_RANGE because it overlaps primary range or excludes SOF_GOSSIP_PORT"
            );
            None
        }
        None if configured_overlap => {
            let split = split_port_range_for_overlap(primary);
            if let Some((left, _)) = split {
                primary = left;
            }
            if split.is_none() {
                tracing::warn!(
                    primary_range = %format_port_range(primary),
                    "unable to auto-split SOF_PORT_RANGE for overlap switching; using break-before-make fallback"
                );
            }
            split.map(|(_, right)| right)
        }
        None => None,
    };
    Ok(GossipRuntimePortPlan { primary, secondary })
}

#[cfg(feature = "gossip-bootstrap")]
fn split_port_range_for_overlap(primary: PortRange) -> Option<(PortRange, PortRange)> {
    let span = u32::from(primary.1.saturating_sub(primary.0)).saturating_add(1);
    let min_span = u32::try_from(read_tvu_receive_sockets().saturating_add(16))
        .unwrap_or(u32::MAX)
        .max(24);
    if span < min_span.saturating_mul(2) {
        return None;
    }
    let primary_span = span / 2;
    if primary_span == 0 {
        return None;
    }
    let primary_end = u32::from(primary.0)
        .saturating_add(primary_span)
        .saturating_sub(1);
    let secondary_start = primary_end.saturating_add(1);
    let secondary_end = u32::from(primary.1);
    if secondary_end < secondary_start {
        return None;
    }
    let secondary_span = secondary_end
        .saturating_sub(secondary_start)
        .saturating_add(1);
    if secondary_span < min_span {
        return None;
    }
    let left = (primary.0, u16::try_from(primary_end).ok()?);
    let right = (
        u16::try_from(secondary_start).ok()?,
        u16::try_from(secondary_end).ok()?,
    );
    Some((left, right))
}

#[cfg(feature = "gossip-bootstrap")]
const fn port_ranges_overlap(left: PortRange, right: PortRange) -> bool {
    left.0 <= right.1 && right.0 <= left.1
}

#[cfg(feature = "gossip-bootstrap")]
const fn range_contains_port(range: PortRange, port: u16) -> bool {
    port >= range.0 && port <= range.1
}

#[cfg(feature = "gossip-bootstrap")]
pub(super) fn format_port_range(range: PortRange) -> String {
    format!("{}-{}", range.0, range.1)
}

#[cfg(feature = "gossip-bootstrap")]
/// Runtime components produced by successful gossip bootstrap.
pub(super) type GossipBootstrapRuntime = (
    Vec<JoinHandle<()>>,
    GossipRuntime,
    Option<crate::repair::GossipRepairClient>,
);

#[cfg(feature = "gossip-bootstrap")]
/// Errors produced while starting one gossip-bootstrap runtime instance.
#[derive(Debug, Error)]
pub(super) enum GossipBootstrapStartError {
    /// Gossip entrypoint could not be resolved to a socket address.
    #[error(transparent)]
    ResolveEntrypoint(#[from] crate::app::runtime::bootstrap::relay::ResolveSocketAddrError),
    /// The blocking discovery task did not join.
    #[error("failed to join gossip bootstrap discovery task: {source}")]
    DiscoveryTaskJoin { source: tokio::task::JoinError },
    /// Public IP probe failed.
    #[error("failed to determine public IP via {entrypoint}: {reason}")]
    DiscoveryPublicIp {
        entrypoint: SocketAddr,
        reason: String,
    },
    /// `SOF_SHRED_VERSION` resolved to zero, which is invalid.
    #[error("shred version resolved to 0; set SOF_SHRED_VERSION explicitly")]
    ZeroShredVersion,
    /// Entrypoint shred-version discovery failed.
    #[error("failed to resolve shred version from entrypoint {entrypoint}: {reason}")]
    DiscoveryShredVersion {
        entrypoint: SocketAddr,
        reason: String,
    },
    /// Port-range parsing failed.
    #[error(transparent)]
    PortRange(#[from] PortRangeParseError),
    /// TVU socket count resolved to zero.
    #[error("SOF_TVU_SOCKETS must be non-zero")]
    ZeroTvuSockets,
    /// Retransmit socket count resolved to zero.
    #[error("internal error: retransmit sockets must be non-zero")]
    ZeroRetransmitSockets,
    /// QUIC endpoint count resolved to zero.
    #[error("DEFAULT_QUIC_ENDPOINTS must be non-zero")]
    ZeroQuicEndpoints,
    /// Bind-IP set construction failed.
    #[error("failed to build bind IP set: {reason}")]
    BindIpAddrs { reason: String },
    /// Gossip node did not allocate TVU sockets.
    #[error("node did not allocate tvu sockets")]
    NoTvuSocketsAllocated,
    /// Failed to clone a TVU socket.
    #[error("failed to clone tvu socket: {source}")]
    CloneTvuSocket { source: std::io::Error },
    /// Failed to set TVU socket nonblocking mode.
    #[error("failed to set tvu socket nonblocking: {source}")]
    SetTvuSocketNonblocking { source: std::io::Error },
    /// Failed to clone repair socket for receiver path.
    #[error("failed to clone repair socket for receiver: {source}")]
    CloneRepairReceiverSocket { source: std::io::Error },
    /// Failed to set repair receiver socket nonblocking mode.
    #[error("failed to set repair receiver socket nonblocking: {source}")]
    SetRepairReceiverSocketNonblocking { source: std::io::Error },
    /// Failed to clone repair socket for sender path.
    #[error("failed to clone repair socket for sender: {source}")]
    CloneRepairSenderSocket { source: std::io::Error },
    /// Failed to set repair sender socket nonblocking mode.
    #[error("failed to set repair sender socket nonblocking: {source}")]
    SetRepairSenderSocketNonblocking { source: std::io::Error },
    /// Failed to convert repair sender socket into Tokio UDP socket.
    #[error("failed to convert repair sender socket to tokio udp socket: {source}")]
    RepairSenderSocketToTokio { source: std::io::Error },
}

#[cfg(feature = "gossip-bootstrap")]
async fn resolve_shred_version(
    discovery_entrypoint: SocketAddr,
) -> Result<u16, GossipBootstrapStartError> {
    if let Some(shred_version_override) = read_shred_version_override() {
        if shred_version_override == 0 {
            return Err(GossipBootstrapStartError::ZeroShredVersion);
        }
        return Ok(shred_version_override);
    }

    let discovery_result =
        tokio::task::spawn_blocking(move || get_cluster_shred_version(&discovery_entrypoint))
            .await
            .map_err(|source| GossipBootstrapStartError::DiscoveryTaskJoin { source })?;
    match discovery_result {
        Ok(shred_version) if shred_version > 0 => Ok(shred_version),
        Ok(_) => Err(GossipBootstrapStartError::ZeroShredVersion),
        Err(reason) => Err(GossipBootstrapStartError::DiscoveryShredVersion {
            entrypoint: discovery_entrypoint,
            reason,
        }),
    }
}

#[cfg(feature = "gossip-bootstrap")]
/// Errors produced while spawning and guarding a gossip bootstrap task.
#[derive(Debug, Error)]
pub(super) enum GossipBootstrapGuardError {
    /// Inner bootstrap startup failed.
    #[error(transparent)]
    Bootstrap(#[from] GossipBootstrapStartError),
    /// Bootstrap task panicked.
    #[error("panic while bootstrapping gossip runtime from {entrypoint}: {source}")]
    BootstrapPanic {
        entrypoint: String,
        source: tokio::task::JoinError,
    },
    /// Bootstrap task failed to join.
    #[error("failed to join gossip bootstrap task for {entrypoint}: {source}")]
    TaskJoin {
        entrypoint: String,
        source: tokio::task::JoinError,
    },
}

#[cfg(feature = "gossip-bootstrap")]
async fn start_gossip_bootstrapped_receiver(
    entrypoint: &str,
    tx: mpsc::Sender<RawPacketBatch>,
    gossip_identity: Arc<Keypair>,
    port_range_override: Option<PortRange>,
) -> Result<GossipBootstrapRuntime, GossipBootstrapStartError> {
    let log_startup_steps = read_log_startup_steps();
    let entrypoint = resolve_socket_addr(entrypoint)?;
    let discovery_entrypoint = entrypoint;
    if log_startup_steps {
        tracing::info!(
            step = "gossip_discovery_probe_begin",
            entrypoint = %discovery_entrypoint,
            "probing shred version and public IP"
        );
    }
    let advertised_ip = tokio::task::spawn_blocking(move || {
        let advertised_ip = get_public_ip_addr_with_binding(
            &discovery_entrypoint,
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        )
        .map_err(|source| GossipBootstrapStartError::DiscoveryPublicIp {
            entrypoint: discovery_entrypoint,
            reason: source.to_string(),
        })?;
        Ok::<IpAddr, GossipBootstrapStartError>(advertised_ip)
    })
    .await
    .map_err(|source| GossipBootstrapStartError::DiscoveryTaskJoin { source })??;
    let shred_version = resolve_shred_version(discovery_entrypoint).await?;
    if log_startup_steps {
        tracing::info!(
            step = "gossip_discovery_probe_complete",
            entrypoint = %entrypoint,
            shred_version,
            advertised_ip = %advertised_ip,
            "gossip discovery probe succeeded"
        );
    }
    let port_range = match port_range_override {
        Some(port_range) => port_range,
        None => read_port_range()?,
    };
    let configured_gossip_port = read_gossip_port();
    let gossip_port = configured_gossip_port
        .filter(|port| range_contains_port(port_range, *port))
        .unwrap_or(port_range.0);
    if configured_gossip_port.is_some() && gossip_port != configured_gossip_port.unwrap_or_default()
    {
        tracing::warn!(
            configured_gossip_port = configured_gossip_port.unwrap_or_default(),
            port_range = %format_port_range(port_range),
            "SOF_GOSSIP_PORT is outside selected port range; using range start instead"
        );
    }
    let gossip_addr = SocketAddr::new(advertised_ip, gossip_port);
    let num_tvu_receive_sockets = NonZeroUsize::new(read_tvu_receive_sockets())
        .ok_or(GossipBootstrapStartError::ZeroTvuSockets)?;
    let num_tvu_retransmit_sockets =
        NonZeroUsize::new(1).ok_or(GossipBootstrapStartError::ZeroRetransmitSockets)?;
    let num_quic_endpoints = NonZeroUsize::new(DEFAULT_QUIC_ENDPOINTS)
        .ok_or(GossipBootstrapStartError::ZeroQuicEndpoints)?;

    let config = NodeConfig {
        advertised_ip,
        gossip_port: gossip_addr.port(),
        port_range,
        bind_ip_addrs: solana_net_utils::multihomed_sockets::BindIpAddrs::new(vec![IpAddr::V4(
            Ipv4Addr::UNSPECIFIED,
        )])
        .map_err(|reason| GossipBootstrapStartError::BindIpAddrs { reason })?,
        public_tpu_addr: None,
        public_tpu_forwards_addr: None,
        vortexor_receiver_addr: None,
        num_tvu_receive_sockets,
        num_tvu_retransmit_sockets,
        num_quic_endpoints,
    };
    let mut node = Node::new_with_external_ip(&gossip_identity.pubkey(), config);
    node.info.set_shred_version(shred_version);

    if node.sockets.tvu.is_empty() {
        return Err(GossipBootstrapStartError::NoTvuSocketsAllocated);
    }

    tracing::info!(
        gossip = %node.info.gossip().unwrap_or(gossip_addr),
        tvu = %node.info.tvu(solana_gossip::contact_info::Protocol::UDP).unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)),
        tvu_sockets = node.sockets.tvu.len(),
        shred_version,
        "started gossip bootstrap node"
    );

    let cluster_info = ClusterInfo::new(
        node.info.clone(),
        gossip_identity,
        SocketAddrSpace::Unspecified,
    );
    cluster_info.set_entrypoint(ContactInfo::new_gossip_entry_point(&entrypoint));
    let gossip_validators = read_gossip_validators();
    if let Some(validators) = gossip_validators.as_ref() {
        tracing::info!(
            validator_count = validators.len(),
            "gossip validator allowlist enabled"
        );
    }
    let cluster_info = Arc::new(cluster_info);
    let exit = Arc::new(AtomicBool::new(false));
    let gossip_service = GossipService::new(
        &cluster_info,
        None,
        node.sockets.gossip.clone(),
        gossip_validators,
        true,
        None,
        exit.clone(),
    );

    let ingest_telemetry = ingest::ReceiverTelemetry::default();
    let mut receiver_handles = Vec::with_capacity(node.sockets.tvu.len().saturating_add(1));
    for socket in &node.sockets.tvu {
        let std_socket = socket
            .try_clone()
            .map_err(|source| GossipBootstrapStartError::CloneTvuSocket { source })?;
        std_socket
            .set_nonblocking(true)
            .map_err(|source| GossipBootstrapStartError::SetTvuSocketNonblocking { source })?;
        receiver_handles.push(ingest::spawn_udp_receiver_from_std_with_telemetry(
            std_socket,
            tx.clone(),
            Some(ingest_telemetry.clone()),
        ));
    }
    let repair_receiver_socket = node
        .sockets
        .repair
        .try_clone()
        .map_err(|source| GossipBootstrapStartError::CloneRepairReceiverSocket { source })?;
    repair_receiver_socket
        .set_nonblocking(true)
        .map_err(
            |source| GossipBootstrapStartError::SetRepairReceiverSocketNonblocking { source },
        )?;
    receiver_handles.push(ingest::spawn_udp_receiver_from_std_with_telemetry(
        repair_receiver_socket,
        tx,
        Some(ingest_telemetry.clone()),
    ));

    let repair_client = if read_repair_enabled() {
        let repair_sender_socket = node
            .sockets
            .repair
            .try_clone()
            .map_err(|source| GossipBootstrapStartError::CloneRepairSenderSocket { source })?;
        repair_sender_socket
            .set_nonblocking(true)
            .map_err(
                |source| GossipBootstrapStartError::SetRepairSenderSocketNonblocking { source },
            )?;
        let repair_sender_socket = tokio::net::UdpSocket::from_std(repair_sender_socket)
            .map_err(|source| GossipBootstrapStartError::RepairSenderSocketToTokio { source })?;
        Some(crate::repair::GossipRepairClient::new(
            cluster_info.clone(),
            repair_sender_socket,
            cluster_info.keypair(),
            crate::repair::GossipRepairClientConfig {
                peer_cache_ttl: Duration::from_millis(read_repair_peer_cache_ttl_ms()),
                peer_cache_capacity: read_repair_peer_cache_capacity(),
                active_peer_count: read_repair_active_peers(),
                peer_sample_size: read_repair_peer_sample_size(),
                serve_max_bytes_per_sec: read_repair_serve_max_bytes_per_sec(),
                serve_unstaked_max_bytes_per_sec: read_repair_serve_unstaked_max_bytes_per_sec(),
                serve_max_requests_per_peer_per_sec:
                    read_repair_serve_max_requests_per_peer_per_sec(),
            },
        ))
    } else {
        None
    };
    Ok((
        receiver_handles,
        GossipRuntime {
            exit,
            gossip_service: Some(gossip_service),
            cluster_info,
            ingest_telemetry,
        },
        repair_client,
    ))
}

#[cfg(feature = "gossip-bootstrap")]
pub(super) async fn start_gossip_bootstrapped_receiver_guarded(
    entrypoint: &str,
    tx: mpsc::Sender<RawPacketBatch>,
    gossip_identity: Arc<Keypair>,
    port_range_override: Option<PortRange>,
) -> Result<GossipBootstrapRuntime, GossipBootstrapGuardError> {
    let entrypoint_owned = entrypoint.to_owned();
    let join = tokio::spawn(async move {
        start_gossip_bootstrapped_receiver(
            &entrypoint_owned,
            tx,
            gossip_identity,
            port_range_override,
        )
        .await
    });
    match join.await {
        Ok(result) => result.map_err(GossipBootstrapGuardError::from),
        Err(source) if source.is_panic() => Err(GossipBootstrapGuardError::BootstrapPanic {
            entrypoint: entrypoint.to_owned(),
            source,
        }),
        Err(source) => Err(GossipBootstrapGuardError::TaskJoin {
            entrypoint: entrypoint.to_owned(),
            source,
        }),
    }
}
