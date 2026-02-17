use super::*;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum ReceiverBootstrapError {
    #[error("relay runtime bootstrap failed: {reason}")]
    RelayRuntime { reason: String },
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
    tx: mpsc::Sender<RawPacketBatch>,
    relay_bus: Option<broadcast::Sender<Vec<u8>>>,
    relay_listen_addr: Option<SocketAddr>,
    tx_event_rx: mpsc::Receiver<TxObservedEvent>,
) -> Result<ReceiverRuntime, ReceiverBootstrapError> {
    let log_startup_steps = read_log_startup_steps();
    let relay_server_handle =
        maybe_start_relay_server(relay_bus, relay_listen_addr).map_err(|source| {
            ReceiverBootstrapError::RelayRuntime {
                reason: source.to_string(),
            }
        })?;
    let mut static_receiver_handles = Vec::new();
    let relay_connect_addrs =
        read_relay_connect_addrs().map_err(|source| ReceiverBootstrapError::RelayRuntime {
            reason: source.to_string(),
        })?;
    if relay_connect_addrs.is_empty() {
        tracing::info!("no SOF_RELAY_CONNECT upstreams configured");
    }
    for remote in relay_connect_addrs {
        tracing::info!(remote = %remote, "starting tcp relay client feed");
        static_receiver_handles.push(ingest::spawn_tcp_relay_receiver(remote, tx.clone()));
    }
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
            let prioritized_entrypoints = prioritize_gossip_entrypoints(&gossip_entrypoints).await;
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
            let bootstrap_stabilize_min_peers = read_gossip_bootstrap_stabilize_min_peers();
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
                )
                .await
                {
                    Ok((gossip_receivers, runtime, client)) => {
                        let stabilization = wait_for_runtime_stabilization(
                            runtime.ingest_telemetry.clone(),
                            bootstrap_stabilize_sustain,
                            bootstrap_stabilize_min_packets,
                            bootstrap_stabilize_max_wait,
                        )
                        .await;
                        let discovered_peers = runtime.cluster_info.all_peers().len();
                        let stabilized_by_peers = discovered_peers >= bootstrap_stabilize_min_peers;
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
                            tracing::warn!(
                                entrypoint = %entrypoint,
                                packets_seen = stabilization.packets_seen,
                                peers_discovered = discovered_peers,
                                min_peers = bootstrap_stabilize_min_peers,
                                "accepting gossip bootstrap runtime via peer discovery despite low packet flow"
                            );
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

    if static_receiver_handles.is_empty() && gossip_receiver_handles.is_empty() {
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
        relay_server = relay_listen_addr.is_some(),
        "receiver bootstrap complete; waiting for traffic"
    );

    Ok(ReceiverRuntime {
        static_receiver_handles,
        gossip_receiver_handles,
        #[cfg(feature = "gossip-bootstrap")]
        gossip_runtime,
        relay_server_handle,
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
    })
}
