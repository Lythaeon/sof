use super::*;
use thiserror::Error;

#[cfg(feature = "gossip-bootstrap")]
#[derive(Debug, Error)]
pub(crate) enum GossipRuntimeSwitchError {
    #[error("all runtime switch candidates failed: {reason}")]
    AllCandidatesFailed { reason: String },
    #[error("runtime switch exhausted candidate list")]
    ExhaustedCandidateList,
}

#[cfg(feature = "gossip-bootstrap")]
pub(crate) async fn maybe_switch_gossip_runtime(
    runtime: &mut ReceiverRuntime,
    packet_ingest_tx: &mpsc::Sender<RawPacketBatch>,
    entrypoints: &[String],
) -> Result<Option<String>, GossipRuntimeSwitchError> {
    let candidate_pool = collect_runtime_switch_entrypoints(
        runtime,
        entrypoints,
        read_gossip_runtime_switch_peer_candidates(),
    );
    if candidate_pool.len() <= 1 {
        return Ok(None);
    }
    let prioritized = prioritize_gossip_entrypoints(&candidate_pool).await;
    let previous_entrypoint = runtime.active_gossip_entrypoint.clone();
    let alternate_candidates: Vec<String> = prioritized
        .iter()
        .filter(|entrypoint| Some(entrypoint.as_str()) != previous_entrypoint.as_deref())
        .cloned()
        .collect();

    let mut candidates = Vec::new();
    let probe_enabled = read_gossip_entrypoint_probe_enabled();
    if probe_enabled {
        for entrypoint in alternate_candidates {
            if probe_gossip_entrypoint_live(&entrypoint).await {
                candidates.push(entrypoint);
            }
        }
    } else {
        candidates.extend(alternate_candidates);
    }

    if candidates.is_empty() {
        tracing::debug!(
            previous_entrypoint = previous_entrypoint.as_deref().unwrap_or(""),
            "no alternate gossip entrypoint passed preflight; skipping runtime switch"
        );
        return Ok(None);
    }
    if let Some(active_entrypoint) = previous_entrypoint.as_ref() {
        candidates.push(active_entrypoint.clone());
    }

    let configured_overlap = Duration::from_millis(read_gossip_runtime_switch_overlap_ms());
    let overlap = if configured_overlap > Duration::ZERO
        && runtime.gossip_runtime_secondary_port_range.is_some()
        && runtime.gossip_runtime_primary_port_range.is_some()
    {
        configured_overlap
    } else {
        Duration::ZERO
    };
    if configured_overlap > Duration::ZERO && overlap == Duration::ZERO {
        tracing::debug!(
            "gossip runtime overlap disabled because SOF_GOSSIP_RUNTIME_SWITCH_PORT_RANGE is unset"
        );
    }

    if overlap == Duration::ZERO {
        let selected_port_range = runtime
            .gossip_runtime_active_port_range
            .or(runtime.gossip_runtime_primary_port_range);
        runtime.stop_gossip_runtime().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut attempts = 0_usize;
        let mut last_error: Option<String> = None;
        for entrypoint in candidates {
            attempts = attempts.saturating_add(1);
            match start_gossip_bootstrapped_receiver_guarded(
                &entrypoint,
                packet_ingest_tx.clone(),
                runtime.gossip_identity.clone(),
                selected_port_range,
            )
            .await
            {
                Ok((receiver_handles, gossip_runtime, repair_client)) => {
                    runtime.replace_gossip_runtime(
                        receiver_handles,
                        gossip_runtime,
                        repair_client,
                        Some(entrypoint.clone()),
                        selected_port_range,
                    );
                    tracing::info!(
                        previous_entrypoint = previous_entrypoint.as_deref().unwrap_or(""),
                        new_entrypoint = %entrypoint,
                        overlap_ms = 0_u64,
                        "gossip runtime handoff complete"
                    );
                    return Ok(Some(entrypoint));
                }
                Err(error) => {
                    tracing::warn!(
                        entrypoint = %entrypoint,
                        error = %error,
                        "runtime switch candidate failed"
                    );
                    last_error = Some(error.to_string());
                }
            }
        }
        if attempts == 0 {
            return Ok(None);
        }
        if let Some(error) = last_error {
            return Err(GossipRuntimeSwitchError::AllCandidatesFailed { reason: error });
        }
        return Err(GossipRuntimeSwitchError::ExhaustedCandidateList);
    }

    let mut old_receiver_handles = Some(std::mem::take(&mut runtime.gossip_receiver_handles));
    let mut old_gossip_runtime = runtime.gossip_runtime.take();
    let mut old_repair_client = runtime.repair_client.take();
    let mut old_entrypoint = runtime.active_gossip_entrypoint.take();
    let mut old_port_range = runtime.gossip_runtime_active_port_range.take();
    let Some(primary_port_range) = runtime.gossip_runtime_primary_port_range else {
        return Ok(None);
    };
    let Some(secondary_port_range) = runtime.gossip_runtime_secondary_port_range else {
        return Ok(None);
    };
    let target_port_range = if old_port_range == Some(primary_port_range) {
        secondary_port_range
    } else {
        primary_port_range
    };
    let mut attempts = 0_usize;
    let mut last_error: Option<String> = None;
    let stabilize_min_packets = read_gossip_runtime_switch_stabilize_min_packets();
    let stabilize_sustain = Duration::from_millis(read_gossip_runtime_switch_stabilize_ms());
    let stabilize_max_wait =
        Duration::from_millis(read_gossip_runtime_switch_stabilize_max_wait_ms());
    let stabilize_effective_max_wait = stabilize_max_wait.min(
        overlap
            .saturating_add(Duration::from_millis(500))
            .max(Duration::from_millis(1_500)),
    );

    for entrypoint in candidates {
        attempts = attempts.saturating_add(1);
        match start_gossip_bootstrapped_receiver_guarded(
            &entrypoint,
            packet_ingest_tx.clone(),
            runtime.gossip_identity.clone(),
            Some(target_port_range),
        )
        .await
        {
            Ok((receiver_handles, new_gossip_runtime, repair_client)) => {
                let new_ingest_telemetry = new_gossip_runtime.ingest_telemetry.clone();
                runtime.replace_gossip_runtime(
                    receiver_handles,
                    new_gossip_runtime,
                    repair_client,
                    Some(entrypoint.clone()),
                    Some(target_port_range),
                );
                let stabilization = wait_for_runtime_stabilization(
                    new_ingest_telemetry,
                    stabilize_sustain,
                    stabilize_min_packets,
                    stabilize_effective_max_wait,
                )
                .await;
                if !stabilization.stabilized {
                    tracing::warn!(
                        previous_entrypoint = previous_entrypoint.as_deref().unwrap_or(""),
                        rejected_entrypoint = %entrypoint,
                        old_port_range = old_port_range
                            .map(format_port_range)
                            .unwrap_or_else(|| "-".to_owned()),
                        rejected_port_range = format_port_range(target_port_range),
                        waited_ms = duration_to_ms_u64(stabilization.elapsed),
                        packets_seen = stabilization.packets_seen,
                        sustain_ms = duration_to_ms_u64(stabilize_sustain),
                        min_packets = stabilize_min_packets,
                        configured_max_wait_ms = duration_to_ms_u64(stabilize_max_wait),
                        effective_max_wait_ms = duration_to_ms_u64(stabilize_effective_max_wait),
                        "new gossip runtime did not stabilize during overlap; reverting to previous runtime"
                    );
                    let new_receiver_handles = std::mem::take(&mut runtime.gossip_receiver_handles);
                    let new_runtime = runtime.gossip_runtime.take();
                    let _ = runtime.repair_client.take();
                    stop_gossip_runtime_components(new_receiver_handles, new_runtime).await;
                    if let Some(old_gossip_runtime) = old_gossip_runtime.take() {
                        runtime.replace_gossip_runtime(
                            old_receiver_handles.take().unwrap_or_default(),
                            old_gossip_runtime,
                            old_repair_client.take(),
                            old_entrypoint.take(),
                            old_port_range.take(),
                        );
                    } else {
                        runtime.gossip_receiver_handles =
                            old_receiver_handles.take().unwrap_or_default();
                        runtime.repair_client = old_repair_client.take();
                        runtime.active_gossip_entrypoint = old_entrypoint.take();
                        runtime.gossip_runtime_active_port_range = old_port_range.take();
                    }
                    return Ok(None);
                }
                stop_gossip_runtime_components(
                    old_receiver_handles.take().unwrap_or_default(),
                    old_gossip_runtime.take(),
                )
                .await;
                drop(old_repair_client.take());
                tracing::info!(
                    previous_entrypoint = previous_entrypoint.as_deref().unwrap_or(""),
                    new_entrypoint = %entrypoint,
                    overlap_ms = duration_to_ms_u64(overlap),
                    old_port_range = old_port_range
                        .map(format_port_range)
                        .unwrap_or_else(|| "-".to_owned()),
                    new_port_range = format_port_range(target_port_range),
                    stabilization_wait_ms = duration_to_ms_u64(stabilization.elapsed),
                    stabilization_packets = stabilization.packets_seen,
                    "gossip runtime handoff complete"
                );
                return Ok(Some(entrypoint));
            }
            Err(error) => {
                if overlap > Duration::ZERO && is_bind_conflict_error(&error) {
                    tracing::warn!(
                        entrypoint = %entrypoint,
                        overlap_ms = duration_to_ms_u64(overlap),
                        error = %error,
                        "runtime switch overlap bind conflict; keeping current gossip runtime"
                    );
                    if let Some(old_gossip_runtime) = old_gossip_runtime.take() {
                        runtime.replace_gossip_runtime(
                            old_receiver_handles.take().unwrap_or_default(),
                            old_gossip_runtime,
                            old_repair_client.take(),
                            old_entrypoint.take(),
                            old_port_range.take(),
                        );
                    } else {
                        runtime.gossip_receiver_handles =
                            old_receiver_handles.take().unwrap_or_default();
                        runtime.repair_client = old_repair_client.take();
                        runtime.active_gossip_entrypoint = old_entrypoint.take();
                        runtime.gossip_runtime_active_port_range = old_port_range.take();
                    }
                    return Ok(None);
                }
                tracing::warn!(
                    entrypoint = %entrypoint,
                    error = %error,
                    "runtime switch candidate failed"
                );
                last_error = Some(error.to_string());
            }
        }
    }

    if let Some(old_gossip_runtime) = old_gossip_runtime.take() {
        runtime.replace_gossip_runtime(
            old_receiver_handles.take().unwrap_or_default(),
            old_gossip_runtime,
            old_repair_client.take(),
            old_entrypoint.take(),
            old_port_range.take(),
        );
    } else {
        runtime.gossip_receiver_handles = old_receiver_handles.take().unwrap_or_default();
        runtime.repair_client = old_repair_client.take();
        runtime.active_gossip_entrypoint = old_entrypoint.take();
        runtime.gossip_runtime_active_port_range = old_port_range.take();
    }

    if attempts == 0 {
        return Ok(None);
    }
    if let Some(error) = last_error {
        return Err(GossipRuntimeSwitchError::AllCandidatesFailed { reason: error });
    }
    Err(GossipRuntimeSwitchError::ExhaustedCandidateList)
}
