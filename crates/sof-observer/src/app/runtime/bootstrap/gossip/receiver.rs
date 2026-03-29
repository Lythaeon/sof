use super::*;
use thiserror::Error;

#[cfg(feature = "gossip-bootstrap")]
pub(crate) struct ProviderStreamGossipControlPlane {
    gossip_receiver_handles: Vec<JoinHandle<()>>,
    gossip_runtime: GossipRuntime,
    active_gossip_entrypoint: Option<String>,
}

#[cfg(feature = "gossip-bootstrap")]
impl ProviderStreamGossipControlPlane {
    pub(crate) fn cluster_info(&self) -> &ClusterInfo {
        self.gossip_runtime.cluster_info.as_ref()
    }

    pub(crate) fn active_entrypoint(&self) -> Option<String> {
        self.active_gossip_entrypoint.clone()
    }

    pub(crate) async fn shutdown(self) {
        crate::app::runtime::bootstrap::gossip::stop_gossip_runtime_components(
            self.gossip_receiver_handles,
            Some(self.gossip_runtime),
        )
        .await;
    }
}

#[derive(Debug, Error)]
pub(crate) enum ReceiverBootstrapError {
    #[error("bind address configuration failed: {reason}")]
    BindAddress { reason: String },
    #[cfg(feature = "gossip-bootstrap")]
    #[error("gossip runtime port plan failed: {reason}")]
    GossipPortPlan { reason: String },
    #[cfg(feature = "gossip-bootstrap")]
    #[error("failed to bootstrap from all SOF_GOSSIP_ENTRYPOINT values: {reason}")]
    GossipBootstrapExhausted { reason: String },
    #[cfg(not(feature = "gossip-bootstrap"))]
    #[error(
        "SOF_GOSSIP_ENTRYPOINT set but this binary was built without `gossip-bootstrap` feature"
    )]
    GossipFeatureDisabled,
}

pub(crate) async fn start_receiver(
    tx: ingest::RawPacketBatchSender,
    tx_event_rx: mpsc::Receiver<TxObservedEvent>,
    #[cfg(feature = "kernel-bypass")] _control_plane_only_bootstrap: bool,
) -> Result<ReceiverRuntime, ReceiverBootstrapError> {
    let log_startup_steps = read_log_startup_steps();
    let mut static_receiver_handles = Vec::new();
    #[cfg(feature = "gossip-bootstrap")]
    let mut gossip_receiver_handles = Vec::new();
    #[cfg(not(feature = "gossip-bootstrap"))]
    let gossip_receiver_handles = Vec::new();
    #[cfg(feature = "gossip-bootstrap")]
    let mut active_gossip_entrypoint: Option<String> = None;

    #[cfg(feature = "gossip-bootstrap")]
    let mut gossip_runtime: Option<GossipRuntime> = None;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_client: Option<crate::repair::GossipRepairClient> = None;
    #[cfg(feature = "gossip-bootstrap")]
    let mut gossip_runtime_active_port_range: Option<PortRange> = None;
    #[cfg(feature = "gossip-bootstrap")]
    let mut gossip_runtime_primary_port_range: Option<PortRange> = None;
    #[cfg(feature = "gossip-bootstrap")]
    let mut gossip_runtime_secondary_port_range: Option<PortRange> = None;
    #[cfg(feature = "gossip-bootstrap")]
    let gossip_identity = Arc::new(Keypair::new());
    #[cfg(feature = "gossip-bootstrap")]
    let gossip_runtime_mode = read_gossip_runtime_mode();
    #[cfg(all(feature = "gossip-bootstrap", feature = "kernel-bypass"))]
    let control_plane_only_bootstrap = _control_plane_only_bootstrap
        || matches!(gossip_runtime_mode, GossipRuntimeMode::ControlPlaneOnly);
    #[cfg(all(feature = "gossip-bootstrap", not(feature = "kernel-bypass")))]
    let control_plane_only_bootstrap =
        matches!(gossip_runtime_mode, GossipRuntimeMode::ControlPlaneOnly);
    let gossip_entrypoints = read_gossip_entrypoints();
    if !gossip_entrypoints.is_empty() {
        if log_startup_steps {
            tracing::info!(
                step = "gossip_bootstrap_config",
                configured_entrypoints = gossip_entrypoints.len(),
                "gossip bootstrap requested"
            );
        }
        #[cfg(not(feature = "gossip-bootstrap"))]
        {
            return Err(ReceiverBootstrapError::GossipFeatureDisabled);
        }
        #[cfg(feature = "gossip-bootstrap")]
        {
            let port_plan = build_gossip_runtime_port_plan().map_err(|source| {
                ReceiverBootstrapError::GossipPortPlan {
                    reason: source.to_string(),
                }
            })?;
            gossip_runtime_primary_port_range = Some(port_plan.primary);
            gossip_runtime_secondary_port_range = port_plan.secondary;
            tracing::info!(
                primary_range = %format_port_range(port_plan.primary),
                secondary_range = port_plan
                    .secondary
                    .map(format_port_range)
                    .unwrap_or_else(|| "-".to_owned()),
                "gossip runtime port plan initialized"
            );
            let prioritized_entrypoints =
                prioritize_gossip_entrypoints(&gossip_entrypoints, None).await;
            if log_startup_steps {
                tracing::info!(
                    step = "gossip_bootstrap_prioritized",
                    total_candidates = prioritized_entrypoints.len(),
                    "gossip entrypoint ordering computed"
                );
            }
            let mut last_error: Option<String> = None;
            let bootstrap_stabilize_min_packets =
                read_gossip_runtime_switch_stabilize_min_packets();
            let bootstrap_stabilize_sustain =
                Duration::from_millis(read_gossip_runtime_switch_stabilize_ms());
            let bootstrap_stabilize_max_wait =
                Duration::from_millis(read_gossip_bootstrap_stabilize_max_wait_ms());
            let bootstrap_stabilize_min_peers = effective_bootstrap_stabilize_min_peers(
                control_plane_only_bootstrap,
                read_gossip_bootstrap_stabilize_min_peers_override(),
            );
            for (attempt, entrypoint) in prioritized_entrypoints.iter().enumerate() {
                if log_startup_steps {
                    tracing::info!(
                        step = "gossip_bootstrap_attempt",
                        attempt = attempt.saturating_add(1),
                        entrypoint = %entrypoint,
                        "attempting gossip bootstrap entrypoint"
                    );
                }
                match start_gossip_bootstrapped_receiver_guarded(
                    entrypoint,
                    tx.clone(),
                    gossip_identity.clone(),
                    Some(port_plan.primary),
                    control_plane_only_bootstrap,
                )
                .await
                {
                    Ok((gossip_receivers, runtime, client)) => {
                        let stabilization = if control_plane_only_bootstrap {
                            wait_for_runtime_stabilization_or_peer_discovery(
                                runtime.ingest_telemetry.clone(),
                                bootstrap_stabilize_sustain,
                                bootstrap_stabilize_min_packets,
                                bootstrap_stabilize_max_wait,
                                || runtime.cluster_info.all_peers().len(),
                                bootstrap_stabilize_min_peers,
                            )
                            .await
                        } else {
                            wait_for_runtime_stabilization(
                                runtime.ingest_telemetry.clone(),
                                bootstrap_stabilize_sustain,
                                bootstrap_stabilize_min_packets,
                                bootstrap_stabilize_max_wait,
                            )
                            .await
                        };
                        let discovered_peers = if control_plane_only_bootstrap {
                            stabilization.discovered_peers
                        } else {
                            runtime.cluster_info.all_peers().len()
                        };
                        let stabilized_by_peers = if control_plane_only_bootstrap {
                            stabilization.stabilized_by_peers
                        } else {
                            gossip_bootstrap_accepts_peer_discovery(
                                false,
                                stabilization.packets_seen,
                                discovered_peers,
                                bootstrap_stabilize_min_peers,
                            )
                        };
                        let accepted = stabilization.stabilized || stabilized_by_peers;
                        if !accepted {
                            let candidate_receivers = gossip_receivers.len();
                            tracing::warn!(
                                entrypoint = %entrypoint,
                                waited_ms = duration_to_ms_u64(stabilization.elapsed),
                                packets_seen = stabilization.packets_seen,
                                peers_discovered = discovered_peers,
                                sustain_ms = duration_to_ms_u64(bootstrap_stabilize_sustain),
                                min_packets = bootstrap_stabilize_min_packets,
                                min_peers = bootstrap_stabilize_min_peers,
                                max_wait_ms = duration_to_ms_u64(bootstrap_stabilize_max_wait),
                                "gossip bootstrap runtime did not stabilize; trying next entrypoint"
                            );
                            stop_gossip_runtime_components(gossip_receivers, Some(runtime)).await;
                            tracing::warn!(
                                entrypoint = %entrypoint,
                                receiver_tasks_stopped = candidate_receivers,
                                "stopped unstable gossip bootstrap runtime; continuing with next entrypoint"
                            );
                            last_error = Some(format!(
                                "entrypoint {entrypoint} did not receive packets during bootstrap stabilization"
                            ));
                            continue;
                        }
                        if !stabilization.stabilized && stabilized_by_peers {
                            if control_plane_only_bootstrap {
                                tracing::info!(
                                    entrypoint = %entrypoint,
                                    packets_seen = stabilization.packets_seen,
                                    peers_discovered = discovered_peers,
                                    min_peers = bootstrap_stabilize_min_peers,
                                    "accepting gossip bootstrap runtime via peer discovery for control-plane-only gossip mode"
                                );
                            } else {
                                tracing::warn!(
                                    entrypoint = %entrypoint,
                                    packets_seen = stabilization.packets_seen,
                                    peers_discovered = discovered_peers,
                                    min_peers = bootstrap_stabilize_min_peers,
                                    "accepting gossip bootstrap runtime via peer discovery despite low packet flow"
                                );
                            }
                        }
                        let mut gossip_receivers = gossip_receivers;
                        if attempt > 0 {
                            tracing::info!(
                                entrypoint = %entrypoint,
                                attempt = attempt.saturating_add(1),
                                "gossip bootstrap succeeded after fallback"
                            );
                        }
                        gossip_receiver_handles.append(&mut gossip_receivers);
                        gossip_runtime = Some(runtime);
                        repair_client = client;
                        active_gossip_entrypoint = Some(entrypoint.clone());
                        gossip_runtime_active_port_range = Some(port_plan.primary);
                        if log_startup_steps {
                            tracing::info!(
                                step = "gossip_bootstrap_active",
                                entrypoint = %entrypoint,
                                "gossip bootstrap entrypoint activated"
                            );
                        }
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(
                            entrypoint = %entrypoint,
                            attempt = attempt.saturating_add(1),
                            error = %error,
                            "failed gossip bootstrap entrypoint; trying next"
                        );
                        last_error = Some(error.to_string());
                    }
                }
            }
            if gossip_runtime.is_none()
                && let Some(error) = last_error
            {
                return Err(ReceiverBootstrapError::GossipBootstrapExhausted { reason: error });
            }
        }
    }

    #[cfg(feature = "gossip-bootstrap")]
    let skip_direct_listener_fallback = control_plane_only_bootstrap && gossip_runtime.is_some();
    #[cfg(not(feature = "gossip-bootstrap"))]
    let skip_direct_listener_fallback = false;
    if static_receiver_handles.is_empty()
        && gossip_receiver_handles.is_empty()
        && !skip_direct_listener_fallback
    {
        let bind_addr = read_bind_addr().map_err(|source| ReceiverBootstrapError::BindAddress {
            reason: source.to_string(),
        })?;
        tracing::info!(
            %bind_addr,
            "starting direct listener mode (recommended for proxy/tunnel feeds)"
        );
        static_receiver_handles.push(ingest::spawn_udp_receiver(bind_addr, tx));
    }
    tracing::info!(
        static_receivers = static_receiver_handles.len(),
        gossip_receivers = gossip_receiver_handles.len(),
        "receiver bootstrap complete; waiting for traffic"
    );

    #[cfg(feature = "gossip-bootstrap")]
    let gossip_ingest_telemetry = gossip_runtime
        .as_ref()
        .map(|runtime| runtime.ingest_telemetry.clone());

    let runtime = ReceiverRuntime {
        static_receiver_handles,
        gossip_receiver_handles,
        #[cfg(feature = "gossip-bootstrap")]
        gossip_ingest_telemetry,
        #[cfg(feature = "gossip-bootstrap")]
        gossip_runtime,
        #[cfg(feature = "gossip-bootstrap")]
        gossip_identity,
        #[cfg(feature = "gossip-bootstrap")]
        active_gossip_entrypoint,
        #[cfg(feature = "gossip-bootstrap")]
        gossip_runtime_primary_port_range,
        #[cfg(feature = "gossip-bootstrap")]
        gossip_runtime_secondary_port_range,
        #[cfg(feature = "gossip-bootstrap")]
        gossip_runtime_active_port_range,
        #[cfg(feature = "gossip-bootstrap")]
        repair_client,
        tx_event_rx,
    };
    #[cfg(feature = "gossip-bootstrap")]
    let mut runtime = runtime;
    #[cfg(feature = "gossip-bootstrap")]
    if read_gossip_bootstrap_only() && runtime.gossip_runtime.is_some() {
        let active_entrypoint = runtime.active_gossip_entrypoint.clone().unwrap_or_default();
        let gossip_receivers = runtime.gossip_receiver_handles.len();
        runtime.detach_gossip_control_plane();
        tracing::info!(
            entrypoint = %active_entrypoint,
            gossip_receivers,
            "gossip bootstrap-only mode enabled; detached gossip control-plane and kept direct receivers"
        );
    }
    #[cfg(feature = "gossip-bootstrap")]
    if read_gossip_control_plane_only() && runtime.gossip_runtime.is_some() {
        let active_entrypoint = runtime.active_gossip_entrypoint.clone().unwrap_or_default();
        tracing::info!(
            entrypoint = %active_entrypoint,
            "gossip control-plane-only mode enabled; kept gossip topology state without gossip shred ingest"
        );
    }
    Ok(runtime)
}

#[cfg(feature = "gossip-bootstrap")]
pub(crate) async fn start_provider_stream_gossip_control_plane()
-> Result<Option<ProviderStreamGossipControlPlane>, ReceiverBootstrapError> {
    let gossip_entrypoints = read_gossip_entrypoints();
    if gossip_entrypoints.is_empty() {
        return Ok(None);
    }

    let log_startup_steps = read_log_startup_steps();
    let gossip_identity = Arc::new(Keypair::new());
    let (tx, _rx) = ingest::create_raw_packet_batch_queue();
    let port_plan = build_gossip_runtime_port_plan().map_err(|source| {
        ReceiverBootstrapError::GossipPortPlan {
            reason: source.to_string(),
        }
    })?;
    let prioritized_entrypoints = prioritize_gossip_entrypoints(&gossip_entrypoints, None).await;
    let bootstrap_stabilize_min_packets = read_gossip_runtime_switch_stabilize_min_packets();
    let bootstrap_stabilize_sustain =
        Duration::from_millis(read_gossip_runtime_switch_stabilize_ms());
    let bootstrap_stabilize_max_wait =
        Duration::from_millis(read_gossip_bootstrap_stabilize_max_wait_ms());
    let bootstrap_stabilize_min_peers = effective_bootstrap_stabilize_min_peers(
        true,
        read_gossip_bootstrap_stabilize_min_peers_override(),
    );
    let mut last_error: Option<String> = None;

    if log_startup_steps {
        tracing::info!(
            step = "provider_gossip_bootstrap_prioritized",
            total_candidates = prioritized_entrypoints.len(),
            "provider-stream gossip control-plane bootstrap ordering computed"
        );
    }

    for (attempt, entrypoint) in prioritized_entrypoints.iter().enumerate() {
        if log_startup_steps {
            tracing::info!(
                step = "provider_gossip_bootstrap_attempt",
                attempt = attempt.saturating_add(1),
                entrypoint = %entrypoint,
                "attempting provider-stream gossip control-plane bootstrap"
            );
        }
        match start_gossip_bootstrapped_receiver_guarded(
            entrypoint,
            tx.clone(),
            gossip_identity.clone(),
            Some(port_plan.primary),
            true,
        )
        .await
        {
            Ok((gossip_receivers, runtime, _repair_client)) => {
                let stabilization = wait_for_runtime_stabilization_or_peer_discovery(
                    runtime.ingest_telemetry.clone(),
                    bootstrap_stabilize_sustain,
                    bootstrap_stabilize_min_packets,
                    bootstrap_stabilize_max_wait,
                    || runtime.cluster_info.all_peers().len(),
                    bootstrap_stabilize_min_peers,
                )
                .await;
                let discovered_peers = stabilization.discovered_peers;
                let accepted = stabilization.stabilized || stabilization.stabilized_by_peers;
                if !accepted {
                    let candidate_receivers = gossip_receivers.len();
                    tracing::warn!(
                        entrypoint = %entrypoint,
                        waited_ms = duration_to_ms_u64(stabilization.elapsed),
                        packets_seen = stabilization.packets_seen,
                        peers_discovered = discovered_peers,
                        min_peers = bootstrap_stabilize_min_peers,
                        "provider-stream gossip control-plane bootstrap did not stabilize; trying next entrypoint"
                    );
                    stop_gossip_runtime_components(gossip_receivers, Some(runtime)).await;
                    tracing::warn!(
                        entrypoint = %entrypoint,
                        receiver_tasks_stopped = candidate_receivers,
                        "stopped unstable provider-stream gossip control-plane bootstrap; continuing with next entrypoint"
                    );
                    last_error = Some(format!(
                        "entrypoint {entrypoint} did not discover enough peers during provider-stream gossip bootstrap"
                    ));
                    continue;
                }
                if !stabilization.stabilized && stabilization.stabilized_by_peers {
                    tracing::info!(
                        entrypoint = %entrypoint,
                        packets_seen = stabilization.packets_seen,
                        peers_discovered = discovered_peers,
                        min_peers = bootstrap_stabilize_min_peers,
                        "accepting provider-stream gossip control-plane bootstrap via peer discovery"
                    );
                }
                if attempt > 0 {
                    tracing::info!(
                        entrypoint = %entrypoint,
                        attempt = attempt.saturating_add(1),
                        "provider-stream gossip control-plane bootstrap succeeded after fallback"
                    );
                }
                return Ok(Some(ProviderStreamGossipControlPlane {
                    gossip_receiver_handles: gossip_receivers,
                    gossip_runtime: runtime,
                    active_gossip_entrypoint: Some(entrypoint.clone()),
                }));
            }
            Err(error) => {
                tracing::warn!(
                    entrypoint = %entrypoint,
                    attempt = attempt.saturating_add(1),
                    error = %error,
                    "failed provider-stream gossip control-plane entrypoint; trying next"
                );
                last_error = Some(error.to_string());
            }
        }
    }

    if let Some(error) = last_error {
        return Err(ReceiverBootstrapError::GossipBootstrapExhausted { reason: error });
    }

    Ok(None)
}

#[cfg(feature = "gossip-bootstrap")]
fn effective_bootstrap_stabilize_min_peers(
    control_plane_only_bootstrap: bool,
    configured_min_peers: Option<usize>,
) -> usize {
    if control_plane_only_bootstrap {
        configured_min_peers.unwrap_or(1)
    } else {
        configured_min_peers.unwrap_or_else(read_gossip_bootstrap_stabilize_min_peers)
    }
}

#[cfg(feature = "gossip-bootstrap")]
const fn gossip_bootstrap_accepts_peer_discovery(
    control_plane_only_bootstrap: bool,
    packets_seen: u64,
    discovered_peers: usize,
    bootstrap_stabilize_min_peers: usize,
) -> bool {
    if control_plane_only_bootstrap {
        discovered_peers >= bootstrap_stabilize_min_peers
    } else {
        packets_seen > 0 && discovered_peers >= bootstrap_stabilize_min_peers
    }
}

#[cfg(all(test, feature = "gossip-bootstrap"))]
mod tests {
    use super::{effective_bootstrap_stabilize_min_peers, gossip_bootstrap_accepts_peer_discovery};

    #[test]
    fn control_plane_only_bootstrap_accepts_peer_discovery_without_packets() {
        assert!(gossip_bootstrap_accepts_peer_discovery(true, 0, 1, 1));
        assert!(!gossip_bootstrap_accepts_peer_discovery(true, 0, 0, 1));
    }

    #[test]
    fn full_gossip_bootstrap_still_requires_packets_for_peer_discovery_fallback() {
        assert!(!gossip_bootstrap_accepts_peer_discovery(false, 0, 8, 8));
        assert!(gossip_bootstrap_accepts_peer_discovery(false, 1, 8, 8));
    }

    #[test]
    fn control_plane_only_bootstrap_uses_low_default_min_peers() {
        assert_eq!(effective_bootstrap_stabilize_min_peers(true, None), 1);
    }

    #[test]
    fn control_plane_only_bootstrap_respects_explicit_min_peers_override() {
        assert_eq!(effective_bootstrap_stabilize_min_peers(true, Some(7)), 7);
    }

    #[test]
    fn full_gossip_bootstrap_keeps_workspace_default_min_peers() {
        assert_eq!(effective_bootstrap_stabilize_min_peers(false, None), 128);
    }
}

#[cfg(feature = "kernel-bypass")]
#[must_use]
pub(crate) fn start_external_receiver(
    tx_event_rx: mpsc::Receiver<TxObservedEvent>,
) -> ReceiverRuntime {
    ReceiverRuntime::external(tx_event_rx)
}
