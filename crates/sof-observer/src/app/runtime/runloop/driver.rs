#[cfg(feature = "gossip-bootstrap")]
use super::control_plane::{
    ClusterTopologyTracker, emit_observed_slot_leader_bytes_event, emit_slot_leader_diff_event,
};
use super::packet_workers::{
    DispatchWorkerBatchOutcome, PacketWorkerBatchResult, PacketWorkerInput, PacketWorkerPool,
    PacketWorkerPoolConfig, WorkerAcceptedShred, WorkerAcceptedShredKind,
};
use super::*;
use crate::reassembly::dataset::CompletedDataSet;
use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr},
    pin::Pin,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub(in crate::app::runtime) enum RuntimeRunloopError {
    #[error("receiver runtime bootstrap failed: {source}")]
    ReceiverBootstrap {
        source: bootstrap::gossip::ReceiverBootstrapError,
    },
    #[error("runtime startup failed: {reason}")]
    RunloopStartup { reason: String },
}

// Runtime coordination defaults kept local to the runloop for operational clarity.
const TX_EVENT_CHANNEL_CAPACITY: usize = 65_536;
const TELEMETRY_INTERVAL_SECS: u64 = 15;
const TELEMETRY_INFO_EVERY_TICKS: u64 = 4;
const TURBINE_PRIMARY_SOURCE_PORT: u16 = 8_899;
const TURBINE_SECONDARY_SOURCE_PORT: u16 = 8_900;
const INITIAL_DEBUG_SAMPLE_LOG_LIMIT: u64 = 5;
#[cfg(feature = "gossip-bootstrap")]
const INITIAL_REPAIR_TRAFFIC_LOG_LIMIT: u64 = 8;
const SUBSTANTIAL_DATASET_MIN_SHREDS: usize = 2;
const CONTROL_PLANE_EVENT_TICK_MS: u64 = 250;
#[cfg(feature = "gossip-bootstrap")]
const CONTROL_PLANE_EVENT_SNAPSHOT_SECS: u64 = 30;
const PACKET_WORKER_QUEUE_OVERFLOW_POLICY: &str = "drop_newest";
const PACKET_WORKER_FEC_PRESSURE_DIVISOR: u64 = 8;

const fn feed_watermarks_from_fork_snapshot(
    snapshot: crate::app::state::ForkTrackerSnapshot,
) -> FeedWatermarks {
    FeedWatermarks {
        canonical_tip_slot: snapshot.tip_slot,
        processed_slot: snapshot.tip_slot,
        confirmed_slot: snapshot.confirmed_slot,
        finalized_slot: snapshot.finalized_slot,
    }
}

fn emit_shutdown_checkpoint_barrier(
    derived_state_host: &DerivedStateHost,
    fork_snapshot: crate::app::state::ForkTrackerSnapshot,
) {
    derived_state_host
        .emit_shutdown_checkpoint_barrier(feed_watermarks_from_fork_snapshot(fork_snapshot));
}

pub(super) type ShutdownSignal = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

pub(in crate::app::runtime) async fn run_async_with_hosts(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
    shutdown_signal: Option<ShutdownSignal>,
    observability_handle: Option<RuntimeObservabilityHandle>,
) -> Result<(), RuntimeRunloopError> {
    let plugin_host_cleanup = plugin_host.clone();
    let result = run_async_with_hosts_inner(
        plugin_host,
        extension_host,
        derived_state_host,
        shutdown_signal,
        observability_handle,
        #[cfg(feature = "kernel-bypass")]
        None,
    )
    .await;
    plugin_host_cleanup.shutdown().await;
    result
}

#[cfg(feature = "kernel-bypass")]
pub(in crate::app::runtime) async fn run_async_with_hosts_and_kernel_bypass_ingress(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
    shutdown_signal: Option<ShutdownSignal>,
    observability_handle: Option<RuntimeObservabilityHandle>,
    packet_ingest_rx: ingest::RawPacketBatchReceiver,
) -> Result<(), RuntimeRunloopError> {
    let plugin_host_cleanup = plugin_host.clone();
    let result = run_async_with_hosts_inner(
        plugin_host,
        extension_host,
        derived_state_host,
        shutdown_signal,
        observability_handle,
        Some(packet_ingest_rx),
    )
    .await;
    plugin_host_cleanup.shutdown().await;
    result
}

async fn run_async_with_hosts_inner(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
    mut shutdown_signal: Option<ShutdownSignal>,
    observability_handle: Option<RuntimeObservabilityHandle>,
    #[cfg(feature = "kernel-bypass")] mut kernel_bypass_packet_ingest_rx: Option<
        ingest::RawPacketBatchReceiver,
    >,
) -> Result<(), RuntimeRunloopError> {
    init_tracing();
    let log_startup_steps = read_log_startup_steps();
    if log_startup_steps {
        tracing::info!(step = "runtime_init", "SOF runtime starting");
    }

    let (tx, default_rx) = ingest::create_raw_packet_batch_queue();
    #[cfg(feature = "kernel-bypass")]
    let kernel_bypass_ingress_enabled = kernel_bypass_packet_ingest_rx.is_some();
    #[cfg(all(feature = "kernel-bypass", feature = "gossip-bootstrap"))]
    let kernel_bypass_gossip_control_plane_enabled = kernel_bypass_ingress_enabled
        && crate::runtime_env::read_env_var("SOF_GOSSIP_ENTRYPOINT").is_some();
    #[cfg(all(feature = "kernel-bypass", not(feature = "gossip-bootstrap")))]
    let kernel_bypass_gossip_control_plane_enabled = false;
    #[cfg(feature = "kernel-bypass")]
    let mut kernel_bypass_internal_ingest_drain_task: Option<JoinHandle<()>> = None;
    #[cfg(feature = "kernel-bypass")]
    let mut rx = if let Some(packet_ingest_rx) = kernel_bypass_packet_ingest_rx.take() {
        if kernel_bypass_gossip_control_plane_enabled {
            let mut ignored_internal_rx = default_rx;
            kernel_bypass_internal_ingest_drain_task = Some(tokio::spawn(async move {
                while ignored_internal_rx.recv().await.is_some() {}
            }));
        }
        packet_ingest_rx
    } else {
        default_rx
    };
    #[cfg(not(feature = "kernel-bypass"))]
    let mut rx = default_rx;
    #[cfg(feature = "gossip-bootstrap")]
    let packet_ingest_tx = tx.clone();
    let (tx_event_tx, tx_event_rx) = mpsc::channel::<TxObservedEvent>(TX_EVENT_CHANNEL_CAPACITY);
    let dataset_workers = read_dataset_workers();
    let tx_event_drop_count = Arc::new(AtomicU64::new(0));
    let dataset_decode_fail_count = Arc::new(AtomicU64::new(0));
    let dataset_tail_skip_count = Arc::new(AtomicU64::new(0));
    let dataset_duplicate_drop_count = Arc::new(AtomicU64::new(0));
    let dataset_queue_drop_count = Arc::new(AtomicU64::new(0));
    let dataset_jobs_enqueued_count = Arc::new(AtomicU64::new(0));
    let dataset_jobs_started_count = Arc::new(AtomicU64::new(0));
    let dataset_jobs_completed_count = Arc::new(AtomicU64::new(0));
    let dataset_queue_capacity = read_dataset_queue_capacity();
    let dataset_attempt_cache_capacity = read_dataset_attempt_cache_capacity();
    let dataset_attempt_success_ttl = Duration::from_millis(read_dataset_attempt_success_ttl_ms());
    let dataset_attempt_failure_ttl = Duration::from_millis(read_dataset_attempt_failure_ttl_ms());
    let log_all_txs = read_log_all_txs();
    let log_non_vote_txs = read_log_non_vote_txs();
    let skip_vote_only_tx_detail_path = read_skip_vote_only_tx_detail_path();
    let log_dataset_reconstruction = read_log_dataset_reconstruction();
    let tx_confirmed_depth_slots = read_tx_confirmed_depth_slots();
    let tx_finalized_depth_slots = read_tx_finalized_depth_slots().max(tx_confirmed_depth_slots);
    let fork_window_slots = read_fork_window_slots();
    let tx_commitment_tracker = Arc::new(CommitmentSlotTracker::new());
    let mut fork_tracker = ForkTracker::new(
        fork_window_slots,
        tx_confirmed_depth_slots,
        tx_finalized_depth_slots,
    );
    let dataset_worker_shared = DatasetWorkerShared {
        derived_state_host: derived_state_host.clone(),
        plugin_host: plugin_host.clone(),
        tx_event_tx: tx_event_tx.clone(),
        tx_commitment_tracker: tx_commitment_tracker.clone(),
        tx_event_drop_count: tx_event_drop_count.clone(),
        dataset_decode_fail_count: dataset_decode_fail_count.clone(),
        dataset_tail_skip_count: dataset_tail_skip_count.clone(),
        dataset_duplicate_drop_count: dataset_duplicate_drop_count.clone(),
        dataset_jobs_started_count: dataset_jobs_started_count.clone(),
        dataset_jobs_completed_count: dataset_jobs_completed_count.clone(),
    };
    let mut dataset_worker_pool = spawn_dataset_workers(
        DatasetWorkerConfig {
            workers: dataset_workers,
            queue_capacity: dataset_queue_capacity,
            attempt_cache_capacity: dataset_attempt_cache_capacity,
            attempt_success_ttl: dataset_attempt_success_ttl,
            attempt_failure_ttl: dataset_attempt_failure_ttl,
            log_dataset_reconstruction,
            log_all_txs,
            log_non_vote_txs,
            skip_vote_only_tx_detail_path,
        },
        &dataset_worker_shared,
    );
    drop(dataset_worker_shared);
    drop(tx_event_tx);
    let plugin_hooks_enabled = !plugin_host.is_empty();
    if plugin_hooks_enabled {
        tracing::info!(plugins = ?plugin_host.plugin_names(), "observer plugins enabled");
        plugin_host.startup().await.map_err(|error| {
            tracing::error!(
                plugin = error.plugin,
                reason = %error.reason,
                "observer plugin startup failed"
            );
            RuntimeRunloopError::RunloopStartup {
                reason: error.to_string(),
            }
        })?;
    }
    let derived_state_hooks_enabled = !derived_state_host.is_empty();
    let derived_state_config = read_derived_state_runtime_config();
    let derived_state_checkpoint_interval_ms = derived_state_config.checkpoint_interval_ms;
    let derived_state_recovery_interval_ms = derived_state_config.recovery_interval_ms;
    let derived_state_replay_backend = derived_state_config.replay.backend;
    let derived_state_replay_dir = derived_state_config.replay.replay_dir.clone();
    let derived_state_replay_durability = derived_state_config.replay.durability;
    let derived_state_replay_max_envelopes = derived_state_config.replay.max_envelopes;
    let derived_state_replay_max_sessions = derived_state_config.replay.max_sessions;
    if derived_state_hooks_enabled {
        if derived_state_replay_max_envelopes > 0 {
            let runtime_replay_source: Arc<dyn crate::framework::DerivedStateReplaySource> =
                match derived_state_replay_backend {
                    crate::framework::DerivedStateReplayBackend::Disk => {
                        match DiskDerivedStateReplaySource::with_policy(
                            derived_state_replay_dir.clone(),
                            derived_state_replay_max_envelopes,
                            derived_state_replay_max_sessions,
                            derived_state_replay_durability,
                        ) {
                            Ok(replay_source) => Arc::new(replay_source),
                            Err(error) => {
                                tracing::warn!(
                                    backend = %crate::framework::DerivedStateReplayBackend::Disk,
                                    path = %derived_state_replay_dir.display(),
                                    error = %error,
                                    "failed to initialize disk-backed derived-state replay tail; falling back to memory"
                                );
                                Arc::new(
                                InMemoryDerivedStateReplaySource::with_max_envelopes_per_session(
                                    derived_state_replay_max_envelopes,
                                ),
                            )
                            }
                        }
                    }
                    crate::framework::DerivedStateReplayBackend::Memory => Arc::new(
                        InMemoryDerivedStateReplaySource::with_max_envelopes_per_session(
                            derived_state_replay_max_envelopes,
                        ),
                    ),
                };
            let installed = derived_state_host.install_runtime_replay_source(runtime_replay_source);
            tracing::info!(
                derived_state_replay_backend = %derived_state_replay_backend,
                derived_state_replay_durability = %derived_state_replay_durability,
                derived_state_replay_dir = %derived_state_replay_dir.display(),
                derived_state_replay_max_envelopes,
                derived_state_replay_max_sessions,
                installed_runtime_replay_source = installed,
                "derived-state runtime replay tail configured"
            );
        } else {
            tracing::info!(
                derived_state_replay_backend = %derived_state_replay_backend,
                derived_state_replay_dir = %derived_state_replay_dir.display(),
                derived_state_replay_max_envelopes,
                derived_state_replay_max_sessions,
                "derived-state runtime replay tail disabled; consumers will recover from checkpoints only"
            );
        }
        derived_state_host.initialize();
        tracing::info!(
            consumers = ?derived_state_host.consumer_names(),
            "derived-state consumers enabled"
        );
    }
    let extension_hooks_enabled = !extension_host.is_empty();
    if extension_hooks_enabled {
        let startup_report = extension_host.startup().await;
        tracing::info!(
            registered_extensions = startup_report.discovered_extensions,
            active_extensions = startup_report.active_extensions,
            failed_extensions = startup_report.failed_extensions,
            extension_failures = ?startup_report.failures,
            "runtime extensions startup completed"
        );
    }
    let extension_queue_depth_warn = read_runtime_extension_queue_depth_warn();
    let extension_dispatch_lag_warn_us = read_runtime_extension_dispatch_lag_warn_us();
    let extension_drop_warn_delta = read_runtime_extension_drop_warn_delta();
    let mut extension_last_dropped_events = 0_u64;
    let relay_cache_window_ms = read_relay_cache_window_ms();
    let relay_cache_max_shreds = read_relay_cache_max_shreds();
    let relay_cache = (relay_cache_window_ms > 0 && relay_cache_max_shreds > 0).then(|| {
        SharedRelayCache::new(RecentShredRingBuffer::new(
            relay_cache_max_shreds,
            Duration::from_millis(relay_cache_window_ms),
        ))
    });
    #[cfg(all(feature = "gossip-bootstrap", feature = "kernel-bypass"))]
    let udp_relay_enabled =
        if kernel_bypass_ingress_enabled && !kernel_bypass_gossip_control_plane_enabled {
            false
        } else {
            read_udp_relay_enabled()
        };
    #[cfg(all(feature = "gossip-bootstrap", not(feature = "kernel-bypass")))]
    let udp_relay_enabled = read_udp_relay_enabled();
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_enabled = false;
    #[cfg(feature = "gossip-bootstrap")]
    let udp_relay_refresh_ms = read_udp_relay_refresh_ms();
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_refresh_ms = 0_u64;
    #[cfg(feature = "gossip-bootstrap")]
    let udp_relay_peer_candidates = read_udp_relay_peer_candidates();
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_peer_candidates = 0_usize;
    #[cfg(feature = "gossip-bootstrap")]
    let udp_relay_fanout = read_udp_relay_fanout();
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_fanout = 0_usize;
    #[cfg(feature = "gossip-bootstrap")]
    let udp_relay_max_sends_per_sec = read_udp_relay_max_sends_per_sec();
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_max_sends_per_sec = 0_u64;
    #[cfg(feature = "gossip-bootstrap")]
    let udp_relay_max_peers_per_ip = read_udp_relay_max_peers_per_ip();
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_max_peers_per_ip = 0_usize;
    #[cfg(feature = "gossip-bootstrap")]
    let udp_relay_require_turbine_source_ports = read_udp_relay_require_turbine_source_ports();
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_require_turbine_source_ports = false;
    #[cfg(feature = "gossip-bootstrap")]
    let udp_relay_send_error_backoff_ms = read_udp_relay_send_error_backoff_ms();
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_send_error_backoff_ms = 0_u64;
    #[cfg(feature = "gossip-bootstrap")]
    let udp_relay_send_error_backoff_threshold = read_udp_relay_send_error_backoff_threshold();
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_send_error_backoff_threshold = 0_u64;
    if log_startup_steps {
        tracing::info!(
            step = "receiver_bootstrap_begin",
            "starting receiver bootstrap"
        );
    }
    #[cfg(feature = "kernel-bypass")]
    if kernel_bypass_ingress_enabled {
        if kernel_bypass_gossip_control_plane_enabled {
            tracing::info!(
                "kernel-bypass ingress enabled; keeping gossip control-plane bootstrap active"
            );
        } else {
            tracing::info!("kernel-bypass ingress enabled; SOF UDP receiver bootstrap is bypassed");
        }
    }
    #[cfg(feature = "kernel-bypass")]
    let mut runtime =
        if kernel_bypass_ingress_enabled && !kernel_bypass_gossip_control_plane_enabled {
            start_external_receiver(tx_event_rx)
        } else {
            start_receiver(
                tx,
                tx_event_rx,
                kernel_bypass_ingress_enabled && kernel_bypass_gossip_control_plane_enabled,
            )
            .await
            .map_err(|source| RuntimeRunloopError::ReceiverBootstrap { source })?
        };
    #[cfg(not(feature = "kernel-bypass"))]
    let mut runtime = start_receiver(tx, tx_event_rx)
        .await
        .map_err(|source| RuntimeRunloopError::ReceiverBootstrap { source })?;
    if log_startup_steps {
        #[cfg(feature = "gossip-bootstrap")]
        tracing::info!(
            step = "receiver_bootstrap_complete",
            static_receivers = runtime.static_receiver_handles.len(),
            gossip_receivers = runtime.gossip_receiver_handles.len(),
            gossip_entrypoint = runtime.active_gossip_entrypoint.as_deref().unwrap_or("-"),
            "receiver bootstrap completed"
        );
        #[cfg(not(feature = "gossip-bootstrap"))]
        tracing::info!(
            step = "receiver_bootstrap_complete",
            static_receivers = runtime.static_receiver_handles.len(),
            gossip_receivers = runtime.gossip_receiver_handles.len(),
            "receiver bootstrap completed"
        );
    }
    if let Some(observability_handle) = observability_handle.as_ref() {
        observability_handle.mark_ready();
    }
    let verify_enabled = read_verify_shreds();
    let live_shreds_enabled = read_live_shreds_enabled();
    if !live_shreds_enabled && verify_enabled {
        tracing::warn!("SOF_VERIFY_SHREDS=true ignored because SOF_LIVE_SHREDS_ENABLED=false");
    }
    let verify_enabled = live_shreds_enabled && verify_enabled;
    let verify_strict_unknown = read_verify_strict_unknown();
    let verify_recovered_shreds = read_verify_recovered_shreds();
    let verify_signature_cache_entries = read_verify_signature_cache_entries();
    let verify_unknown_retry = Duration::from_millis(read_verify_unknown_retry_ms());
    let dedupe_capacity = read_shred_dedupe_capacity();
    let dedupe_ttl_ms = read_shred_dedupe_ttl_ms();
    let verify_slot_leader_window = read_verify_slot_leader_window();
    tracing::info!(
        verify_shreds = verify_enabled,
        verify_recovered_shreds,
        verify_strict_unknown,
        "shred verification configuration"
    );

    #[cfg(all(feature = "gossip-bootstrap", feature = "kernel-bypass"))]
    let repair_enabled_configured =
        if kernel_bypass_ingress_enabled && !kernel_bypass_gossip_control_plane_enabled {
            false
        } else {
            read_repair_enabled()
        };
    #[cfg(all(feature = "gossip-bootstrap", not(feature = "kernel-bypass")))]
    let repair_enabled_configured = read_repair_enabled();
    #[cfg(feature = "gossip-bootstrap")]
    if !live_shreds_enabled && repair_enabled_configured {
        tracing::warn!("SOF_REPAIR_ENABLED=true ignored because SOF_LIVE_SHREDS_ENABLED=false");
    }
    #[cfg(feature = "gossip-bootstrap")]
    let repair_enabled = live_shreds_enabled && repair_enabled_configured;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_enabled = false;
    #[cfg(feature = "gossip-bootstrap")]
    if !repair_enabled {
        runtime.repair_client = None;
    }
    #[cfg(feature = "gossip-bootstrap")]
    let (
        mut repair_command_tx,
        mut repair_result_rx,
        mut repair_peer_snapshot,
        mut repair_driver_handle,
    ) = if repair_enabled {
        runtime.repair_client.take().map_or_else(
            || {
                tracing::warn!("repair enabled but no repair client available");
                (None, None, None, None)
            },
            |repair_client| {
                let (command_tx, result_rx, peer_snapshot, driver_handle) =
                    spawn_repair_driver(repair_client, relay_cache.clone());
                (
                    Some(command_tx),
                    Some(result_rx),
                    Some(peer_snapshot),
                    Some(driver_handle),
                )
            },
        )
    } else {
        (None, None, None, None)
    };
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_driver_enabled = repair_enabled && repair_command_tx.is_some();
    #[cfg(not(feature = "gossip-bootstrap"))]
    let mut repair_result_rx = None::<mpsc::Receiver<()>>;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_driver_enabled = false;
    let repair_min_slot_lag = read_repair_min_slot_lag();
    let repair_min_slot_lag_stalled = read_repair_min_slot_lag_stalled();
    let repair_tip_stall_ms = read_repair_tip_stall_ms();
    let repair_stall_sustain_ms = read_repair_stall_sustain_ms();
    let repair_tip_probe_ahead_slots = read_repair_tip_probe_ahead_slots();
    let repair_per_slot_cap = read_repair_per_slot_cap();
    let repair_per_slot_cap_stalled = read_repair_per_slot_cap_stalled();
    let repair_dataset_stall_ms = read_repair_dataset_stall_ms();
    let mut missing_tracker = if repair_enabled {
        Some(MissingShredTracker::new(
            read_repair_slot_window(),
            repair_min_slot_lag,
            Duration::from_millis(read_repair_settle_ms()),
            Duration::from_millis(read_repair_cooldown_ms()),
            read_repair_backfill_sets(),
            repair_per_slot_cap,
            repair_tip_probe_ahead_slots,
        ))
    } else {
        None
    };
    let repair_max_requests_per_tick = read_repair_max_requests_per_tick();
    let repair_max_requests_per_tick_stalled = read_repair_max_requests_per_tick_stalled();
    let repair_max_highest_per_tick = read_repair_max_highest_per_tick();
    let repair_max_highest_per_tick_stalled = read_repair_max_highest_per_tick_stalled();
    let repair_max_forward_probe_per_tick = read_repair_max_forward_probe_per_tick();
    let repair_max_forward_probe_per_tick_stalled =
        read_repair_max_forward_probe_per_tick_stalled();
    let repair_outstanding_timeout_ms = read_repair_outstanding_timeout_ms();
    let mut outstanding_repairs = repair_enabled.then(|| {
        OutstandingRepairRequests::new(Duration::from_millis(repair_outstanding_timeout_ms))
    });
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_dynamic_stalled = false;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_dynamic_stalled = false;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_dynamic_min_slot_lag = repair_min_slot_lag;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_dynamic_min_slot_lag = repair_min_slot_lag;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_dynamic_max_requests_per_tick = repair_max_requests_per_tick;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_dynamic_max_requests_per_tick = repair_max_requests_per_tick;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_dynamic_max_highest_per_tick = repair_max_highest_per_tick;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_dynamic_max_highest_per_tick = repair_max_highest_per_tick;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_dynamic_max_forward_probe_per_tick = repair_max_forward_probe_per_tick;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_dynamic_max_forward_probe_per_tick = repair_max_forward_probe_per_tick;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_dynamic_per_slot_cap = repair_per_slot_cap;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_dynamic_per_slot_cap = repair_per_slot_cap;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_dynamic_dataset_stalled = false;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_dynamic_dataset_stalled = false;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_stall_started_at: Option<Instant> = None;
    #[cfg(feature = "gossip-bootstrap")]
    let repair_stall_sustain = Duration::from_millis(repair_stall_sustain_ms);
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_last_shred_count_snapshot: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_dynamic_stream_progress = false;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_dynamic_stream_progress = false;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_dynamic_stream_healthy = false;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_dynamic_stream_healthy = false;
    let mut latest_shred_updated_at = Instant::now();
    let mut last_dataset_reconstructed_at = Instant::now();
    #[cfg(feature = "gossip-bootstrap")]
    let gossip_entrypoints = read_gossip_entrypoints();
    #[cfg(feature = "gossip-bootstrap")]
    let gossip_runtime_switch_enabled =
        repair_enabled && read_gossip_runtime_switch_enabled() && !read_gossip_bootstrap_only();
    #[cfg(feature = "gossip-bootstrap")]
    let gossip_runtime_switch_stall_ms = read_gossip_runtime_switch_stall_ms();
    #[cfg(feature = "gossip-bootstrap")]
    let gossip_runtime_switch_dataset_stall_ms = read_gossip_runtime_switch_dataset_stall_ms();
    #[cfg(feature = "gossip-bootstrap")]
    let gossip_runtime_switch_cooldown =
        Duration::from_millis(read_gossip_runtime_switch_cooldown_ms());
    #[cfg(feature = "gossip-bootstrap")]
    let gossip_runtime_switch_warmup =
        Duration::from_millis(read_gossip_runtime_switch_warmup_ms());
    #[cfg(feature = "gossip-bootstrap")]
    let gossip_runtime_switch_overlap =
        Duration::from_millis(read_gossip_runtime_switch_overlap_ms());
    #[cfg(feature = "gossip-bootstrap")]
    let gossip_runtime_switch_sustain =
        Duration::from_millis(read_gossip_runtime_switch_sustain_ms());
    #[cfg(feature = "gossip-bootstrap")]
    let gossip_runtime_switch_no_traffic_grace_ms =
        read_gossip_runtime_switch_no_traffic_grace_ms();
    #[cfg(feature = "gossip-bootstrap")]
    let mut last_gossip_runtime_switch_attempt = Instant::now();
    #[cfg(feature = "gossip-bootstrap")]
    let mut gossip_runtime_started_at = Instant::now();
    #[cfg(feature = "gossip-bootstrap")]
    let mut gossip_runtime_stall_started_at: Option<Instant> = None;

    let dataset_max_tracked_slots = read_dataset_max_tracked_slots();
    let dataset_retained_slot_lag = read_dataset_retained_slot_lag();
    let fec_max_tracked_sets = read_fec_max_tracked_sets();
    let fec_retained_slot_lag = read_fec_retained_slot_lag();
    let packet_workers = read_packet_workers();
    let packet_worker_queue_capacity = read_packet_worker_queue_capacity();
    let dataset_tail_min_shreds_without_anchor = read_dataset_tail_min_shreds_without_anchor();
    let mut shred_dedupe_cache = (dedupe_capacity > 0 && dedupe_ttl_ms > 0).then(|| {
        ShredDedupeCache::new(
            dedupe_capacity,
            Duration::from_millis(dedupe_ttl_ms),
            dataset_retained_slot_lag,
        )
    });
    sync_shred_dedupe_runtime_metrics(shred_dedupe_cache.as_ref());
    let mut packet_worker_pool = PacketWorkerPool::new(PacketWorkerPoolConfig {
        workers: packet_workers,
        queue_capacity: packet_worker_queue_capacity,
        verify_enabled,
        verify_recovered_shreds,
        verify_strict_unknown,
        verify_signature_cache_entries,
        verify_slot_leader_window,
        verify_unknown_retry,
        fec_max_tracked_sets,
        fec_retained_slot_lag,
    });
    let packet_worker_assignment_slot_window =
        usize::try_from(fec_retained_slot_lag.max(32)).unwrap_or(usize::MAX);
    let mut packet_worker_assignments =
        PacketWorkerAssignments::new(packet_worker_assignment_slot_window);
    let mut packet_batch_dispatch_scratch =
        PacketBatchDispatchScratch::new(packet_worker_pool.worker_count());
    let mut dataset_reassembler = DataSetReassembler::new(dataset_max_tracked_slots)
        .with_retained_slot_lag(dataset_retained_slot_lag)
        .with_tail_min_shreds_without_anchor(dataset_tail_min_shreds_without_anchor);
    let mut packet_count: u64 = 0;
    let mut source_port_8899_packets: u64 = 0;
    let mut source_port_8900_packets: u64 = 0;
    let mut source_port_other_packets: u64 = 0;
    let mut data_count: u64 = 0;
    let mut code_count: u64 = 0;
    let mut source_port_8899_data: u64 = 0;
    let mut source_port_8900_data: u64 = 0;
    let mut source_port_other_data: u64 = 0;
    let mut source_port_8899_code: u64 = 0;
    let mut source_port_8900_code: u64 = 0;
    let mut source_port_other_code: u64 = 0;
    let mut recovered_data_count: u64 = 0;
    let mut data_complete_count: u64 = 0;
    let mut last_in_slot_count: u64 = 0;
    let mut dataset_ranges_emitted: u64 = 0;
    let mut dataset_ranges_emitted_from_recovered: u64 = 0;
    let mut parse_error_count: u64 = 0;
    let mut parse_too_short_count: u64 = 0;
    let mut parse_invalid_variant_count: u64 = 0;
    let mut parse_invalid_data_size_count: u64 = 0;
    let mut parse_invalid_coding_header_count: u64 = 0;
    let mut parse_other_count: u64 = 0;
    let mut relay_cache_inserts: u64 = 0;
    let mut relay_cache_replacements: u64 = 0;
    let mut relay_cache_evictions: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_candidates: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_candidates: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_refreshes: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_refreshes: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_forwarded_packets: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_forwarded_packets: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_send_attempts: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_send_attempts: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_send_errors: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_send_errors: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_rate_limited_packets: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_rate_limited_packets: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_source_filtered_packets: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_source_filtered_packets: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_backoff_events: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_backoff_events: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_backoff_drops: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let udp_relay_backoff_drops: u64 = 0;
    let mut dedupe_ingress_duplicate_drop_count: u64 = 0;
    let mut dedupe_ingress_conflict_drop_count: u64 = 0;
    let mut dedupe_canonical_duplicate_drop_count: u64 = 0;
    let mut dedupe_canonical_conflict_drop_count: u64 = 0;
    let mut vote_only_count: u64 = 0;
    let mut mixed_count: u64 = 0;
    let mut non_vote_count: u64 = 0;
    let mut verify_verified_count: u64 = 0;
    let mut verify_unknown_leader_count: u64 = 0;
    let mut verify_invalid_merkle_count: u64 = 0;
    let mut verify_invalid_signature_count: u64 = 0;
    let mut verify_malformed_count: u64 = 0;
    let mut verify_dropped_count: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let log_repair_peer_traffic = read_log_repair_peer_traffic();
    #[cfg(feature = "gossip-bootstrap")]
    let log_repair_peer_traffic_every = read_log_repair_peer_traffic_every().max(1);
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_request_sent_logs: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_response_ping_logs: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_requests_total: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_requests_total: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_requests_enqueued: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_requests_enqueued: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_requests_sent: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_requests_sent: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_requests_no_peer: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_requests_no_peer: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_request_errors: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_request_errors: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_request_queue_drops: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_request_queue_drops: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_requests_port_8899: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_requests_port_8899: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_requests_port_8900: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_requests_port_8900: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_requests_port_other: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_requests_port_other: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_requests_window_index: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_requests_window_index: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_requests_highest_window_index: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_requests_highest_window_index: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_requests_skipped_outstanding: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_requests_skipped_outstanding: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_outstanding_purged: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_outstanding_purged: u64 = 0;
    let mut repair_outstanding_cleared_on_receive: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_response_pings: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_response_pings: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_response_ping_errors: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_response_ping_errors: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_ping_queue_drops: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_ping_queue_drops: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_serve_requests_enqueued: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_serve_requests_enqueued: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_serve_requests_handled: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_serve_requests_handled: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_serve_responses_sent: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_serve_responses_sent: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_serve_cache_misses: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_serve_cache_misses: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_serve_rate_limited: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_serve_rate_limited: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_serve_rate_limited_peer: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_serve_rate_limited_peer: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_serve_rate_limited_bytes: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_serve_rate_limited_bytes: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_serve_errors: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_serve_errors: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_serve_queue_drops: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_serve_queue_drops: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_source_hint_drops: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_source_hint_drops: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_source_hint_enqueued: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_source_hint_enqueued: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_source_hint_buffer_drops: u64 = 0;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let repair_source_hint_buffer_drops: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut gossip_runtime_switch_attempts: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut gossip_runtime_switch_success: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut gossip_runtime_switch_failures: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let repair_source_hint_batch_size = read_repair_source_hint_batch_size();
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_source_hints = RepairSourceHintBuffer::new(read_repair_source_hint_capacity());
    #[cfg(feature = "gossip-bootstrap")]
    let repair_source_hint_flush_interval =
        Duration::from_millis(read_repair_source_hint_flush_ms());
    #[cfg(feature = "gossip-bootstrap")]
    let mut repair_source_hint_last_flush = Instant::now();
    let mut latest_shred_slot: Option<u64> = None;
    let mut fork_status_transitions_total: u64 = 0;
    let mut fork_reorg_count: u64 = 0;
    let mut fork_orphaned_slots_total: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut emitted_slot_leaders: HashMap<u64, [u8; 32]> = HashMap::new();
    let mut coverage_window = SlotCoverageWindow::new(read_coverage_window_slots());
    let mut telemetry_tick = interval(Duration::from_secs(TELEMETRY_INTERVAL_SECS));
    let mut telemetry_tick_count: u64 = 0;
    let mut repair_tick = interval(Duration::from_millis(read_repair_tick_ms()));
    let mut control_plane_tick = interval(Duration::from_millis(CONTROL_PLANE_EVENT_TICK_MS));
    let derived_state_checkpoint_enabled =
        derived_state_hooks_enabled && derived_state_checkpoint_interval_ms > 0;
    let derived_state_recovery_enabled =
        derived_state_hooks_enabled && derived_state_recovery_interval_ms > 0;
    let mut derived_state_checkpoint_tick = interval(Duration::from_millis(
        derived_state_checkpoint_interval_ms.max(250),
    ));
    let mut derived_state_recovery_tick = interval(Duration::from_millis(
        derived_state_recovery_interval_ms.max(250),
    ));
    let mut logged_waiting_for_packets = false;
    tracing::info!(
        live_shreds_enabled,
        verify_enabled,
        verify_recovered_shreds,
        verify_strict_unknown,
        repair_enabled,
        dataset_workers,
        dataset_queue_capacity,
        dataset_attempt_cache_capacity,
        packet_workers,
        packet_worker_queue_capacity,
        dataset_retained_slot_lag,
        fec_retained_slot_lag,
        skip_vote_only_tx_detail_path,
        packet_worker_queue_overflow_policy = PACKET_WORKER_QUEUE_OVERFLOW_POLICY,
        dedupe_capacity,
        dedupe_ttl_ms,
        relay_cache_enabled = relay_cache.is_some(),
        relay_cache_window_ms,
        relay_cache_max_shreds,
        udp_relay_enabled,
        udp_relay_refresh_ms,
        udp_relay_peer_candidates,
        udp_relay_fanout,
        udp_relay_max_sends_per_sec,
        udp_relay_max_peers_per_ip,
        udp_relay_require_turbine_source_ports,
        udp_relay_send_error_backoff_ms,
        udp_relay_send_error_backoff_threshold,
        derived_state_recovery_interval_ms,
        derived_state_replay_backend = %derived_state_replay_backend,
        derived_state_replay_durability = %derived_state_replay_durability,
        derived_state_replay_dir = %derived_state_replay_dir.display(),
        derived_state_replay_max_envelopes,
        derived_state_replay_max_sessions,
        tx_confirmed_depth_slots,
        tx_finalized_depth_slots,
        fork_window_slots,
        "observer runtime initialized"
    );
    if log_startup_steps {
        tracing::info!(
            step = "event_loop_ready",
            "runtime event loop started; waiting for ingress"
        );
    }
    telemetry_tick.tick().await;
    repair_tick.tick().await;
    control_plane_tick.tick().await;
    if derived_state_checkpoint_enabled {
        derived_state_checkpoint_tick.tick().await;
    }
    if derived_state_recovery_enabled {
        derived_state_recovery_tick.tick().await;
    }
    #[cfg(feature = "gossip-bootstrap")]
    let mut topology_tracker = ClusterTopologyTracker::new(
        Duration::from_millis(CONTROL_PLANE_EVENT_TICK_MS),
        Duration::from_secs(CONTROL_PLANE_EVENT_SNAPSHOT_SECS),
    );
    #[cfg(feature = "gossip-bootstrap")]
    let udp_relay_refresh = Duration::from_millis(udp_relay_refresh_ms.max(250));
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_last_refresh = {
        let now = Instant::now();
        now.checked_sub(udp_relay_refresh).unwrap_or(now)
    };
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_peers: Vec<SocketAddr> = Vec::new();
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_rr_cursor: usize = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_window_started = Instant::now();
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_sends_in_window: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_send_error_streak: u64 = 0;
    #[cfg(feature = "gossip-bootstrap")]
    let mut udp_relay_backoff_until: Option<Instant> = None;
    let mut ingest_closed = false;
    let mut packet_workers_closed = false;
    let mut dataset_workers_shutdown = false;
    let mut tx_events_closed = false;
    #[cfg(feature = "gossip-bootstrap")]
    let udp_relay_socket: Option<std::net::UdpSocket> = if udp_relay_enabled {
        match std::net::UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)) {
            Ok(socket) => match socket.set_nonblocking(true) {
                Ok(()) => Some(socket),
                Err(error) => {
                    tracing::warn!(error = %error, "failed to set nonblocking on udp relay socket");
                    None
                }
            },
            Err(error) => {
                tracing::warn!(error = %error, "failed to bind udp relay socket; disabling udp relay forwarding");
                None
            }
        }
    } else {
        None
    };

    'event_loop: loop {
        tokio::select! {
            biased;
            () = async {
                if let Some(signal) = shutdown_signal.as_mut() {
                    signal.as_mut().await;
                }
            }, if shutdown_signal.is_some() => {
                tracing::info!("observer runtime shutdown signal received");
                if let Some(observability_handle) = observability_handle.as_ref() {
                    observability_handle.mark_not_ready();
                }
                break 'event_loop;
            }
            maybe_worker_result = packet_worker_pool.recv(), if !packet_workers_closed => {
                let Some(worker_result) = maybe_worker_result else {
                    packet_workers_closed = true;
                    if !dataset_workers_shutdown {
                        dataset_worker_pool.shutdown().await;
                        dataset_workers_shutdown = true;
                    }
                    if tx_events_closed {
                        break 'event_loop;
                    }
                    continue;
                };
                let observed_at = Instant::now();
                verify_verified_count = verify_verified_count
                    .saturating_add(worker_result.verify_verified_count);
                verify_unknown_leader_count = verify_unknown_leader_count
                    .saturating_add(worker_result.verify_unknown_leader_count);
                verify_invalid_merkle_count = verify_invalid_merkle_count
                    .saturating_add(worker_result.verify_invalid_merkle_count);
                verify_invalid_signature_count = verify_invalid_signature_count
                    .saturating_add(worker_result.verify_invalid_signature_count);
                verify_malformed_count = verify_malformed_count
                    .saturating_add(worker_result.verify_malformed_count);
                verify_dropped_count = verify_dropped_count
                    .saturating_add(worker_result.verify_dropped_count);
                let summary = PacketWorkerResultContext {
                    tx_commitment_tracker: tx_commitment_tracker.as_ref(),
                    plugin_host: &plugin_host,
                    derived_state_host: &derived_state_host,
                    plugin_hooks_enabled,
                    derived_state_hooks_enabled,
                    latest_shred_slot: &mut latest_shred_slot,
                    latest_shred_updated_at: &mut latest_shred_updated_at,
                    fork_tracker: &mut fork_tracker,
                    coverage_window: &mut coverage_window,
                    missing_tracker: &mut missing_tracker,
                    outstanding_repairs: &mut outstanding_repairs,
                    repair_outstanding_cleared_on_receive: &mut repair_outstanding_cleared_on_receive,
                    shred_dedupe_cache: &mut shred_dedupe_cache,
                    dedupe_canonical_duplicate_drop_count: &mut dedupe_canonical_duplicate_drop_count,
                    dedupe_canonical_conflict_drop_count: &mut dedupe_canonical_conflict_drop_count,
                    dataset_reassembler: &mut dataset_reassembler,
                    dataset_retained_slot_lag,
                    dataset_worker_queues: dataset_worker_pool.queues(),
                    dataset_jobs_enqueued_count: dataset_jobs_enqueued_count.as_ref(),
                    dataset_queue_drop_count: dataset_queue_drop_count.as_ref(),
                    last_dataset_reconstructed_at: &mut last_dataset_reconstructed_at,
                    data_count: &mut data_count,
                    code_count: &mut code_count,
                    recovered_data_count: &mut recovered_data_count,
                    data_complete_count: &mut data_complete_count,
                    last_in_slot_count: &mut last_in_slot_count,
                    source_port_8899_data: &mut source_port_8899_data,
                    source_port_8900_data: &mut source_port_8900_data,
                    source_port_other_data: &mut source_port_other_data,
                    source_port_8899_code: &mut source_port_8899_code,
                    source_port_8900_code: &mut source_port_8900_code,
                    source_port_other_code: &mut source_port_other_code,
                    fork_status_transitions_total: &mut fork_status_transitions_total,
                    fork_reorg_count: &mut fork_reorg_count,
                    fork_orphaned_slots_total: &mut fork_orphaned_slots_total,
                    #[cfg(feature = "gossip-bootstrap")]
                    emitted_slot_leaders: &mut emitted_slot_leaders,
                    #[cfg(feature = "gossip-bootstrap")]
                    verify_slot_leader_window,
                    #[cfg(feature = "gossip-bootstrap")]
                    repair_driver_enabled,
                    #[cfg(feature = "gossip-bootstrap")]
                    repair_source_hints: &mut repair_source_hints,
                    #[cfg(feature = "gossip-bootstrap")]
                    repair_command_tx: repair_command_tx.as_ref(),
                    #[cfg(feature = "gossip-bootstrap")]
                    repair_source_hint_batch_size,
                    #[cfg(feature = "gossip-bootstrap")]
                    repair_source_hint_flush_interval,
                    #[cfg(feature = "gossip-bootstrap")]
                    repair_source_hint_drops: &mut repair_source_hint_drops,
                    #[cfg(feature = "gossip-bootstrap")]
                    repair_source_hint_enqueued: &mut repair_source_hint_enqueued,
                    #[cfg(feature = "gossip-bootstrap")]
                    repair_source_hint_buffer_drops: &mut repair_source_hint_buffer_drops,
                    #[cfg(feature = "gossip-bootstrap")]
                    repair_source_hint_last_flush: &mut repair_source_hint_last_flush,
                }
                .process(
                    worker_result,
                    observed_at,
                    &mut packet_batch_dispatch_scratch,
                );
                crate::runtime_metrics::observe_recovered_data_packets(
                    summary.recovered_data_packets,
                );
                dataset_ranges_emitted = dataset_ranges_emitted.saturating_add(
                    summary.completed_dataset_count,
                );
                crate::runtime_metrics::observe_completed_datasets(
                    summary.completed_dataset_count,
                );
                dataset_ranges_emitted_from_recovered = dataset_ranges_emitted_from_recovered
                    .saturating_add(summary.completed_datasets_from_recovered);
            }
            maybe_packet_batch = rx.recv(), if !ingest_closed => {
                let Some(packet_batch) = maybe_packet_batch else {
                    ingest_closed = true;
                    packet_worker_pool.close_inputs();
                    continue;
                };
                packet_batch_dispatch_scratch.refresh(&packet_worker_pool);
                for packet in packet_batch {
                    let observed_at = Instant::now();
                    let source_addr = packet.source;
                    let packet_bytes = packet.bytes;
                    let shared_observer_packet = (plugin_hooks_enabled || extension_hooks_enabled)
                        .then(|| Arc::clone(&packet_bytes));
                    if plugin_hooks_enabled
                        && let Some(shared_packet) = shared_observer_packet.as_ref()
                    {
                        plugin_host.on_raw_packet(RawPacketEvent {
                            source: source_addr,
                            bytes: Arc::clone(shared_packet),
                        });
                    }
                    if extension_hooks_enabled
                        && let Some(shared_packet) = shared_observer_packet.as_ref()
                    {
                        extension_host.on_observer_packet_shared(
                            source_addr,
                            Arc::clone(shared_packet),
                        );
                    }
                    packet_count = packet_count.saturating_add(1);
                    if logged_waiting_for_packets {
                        tracing::info!(
                            packets = packet_count,
                            source = %source_addr,
                            "ingress traffic detected"
                        );
                        logged_waiting_for_packets = false;
                    }
                    if !live_shreds_enabled {
                        continue;
                    }
                    #[cfg(feature = "gossip-bootstrap")]
                    if repair_driver_enabled
                        && crate::repair::is_repair_response_ping_packet(packet_bytes.as_ref())
                        && let Some(command_tx) = repair_command_tx.as_ref()
                    {
                        match command_tx.try_send(RepairCommand::HandleResponsePing {
                            packet: Arc::clone(&packet_bytes),
                            from_addr: source_addr,
                        }) {
                            Ok(()) => {
                                continue;
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                repair_ping_queue_drops = repair_ping_queue_drops.saturating_add(1);
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                repair_response_ping_errors =
                                    repair_response_ping_errors.saturating_add(1);
                            }
                        }
                    }
                    #[cfg(feature = "gossip-bootstrap")]
                    if repair_driver_enabled
                        && crate::repair::is_supported_repair_request_packet(packet_bytes.as_ref())
                        && let Some(command_tx) = repair_command_tx.as_ref()
                    {
                        match command_tx.try_send(RepairCommand::HandleServeRequest {
                            packet: Arc::clone(&packet_bytes),
                            from_addr: source_addr,
                        }) {
                            Ok(()) => {
                                repair_serve_requests_enqueued =
                                    repair_serve_requests_enqueued.saturating_add(1);
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                repair_serve_queue_drops = repair_serve_queue_drops.saturating_add(1);
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                repair_serve_errors = repair_serve_errors.saturating_add(1);
                            }
                        }
                        continue;
                    }
                    match source_addr.port() {
                        TURBINE_PRIMARY_SOURCE_PORT => {
                            source_port_8899_packets = source_port_8899_packets.saturating_add(1);
                        }
                        TURBINE_SECONDARY_SOURCE_PORT => {
                            source_port_8900_packets = source_port_8900_packets.saturating_add(1);
                        }
                        _ => {
                            source_port_other_packets = source_port_other_packets.saturating_add(1);
                        }
                    }
                    let parsed_shred = match parse_shred_header(packet_bytes.as_ref()) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            parse_error_count = parse_error_count.saturating_add(1);
                            match error {
                                ParseError::PacketTooShort { .. } => {
                                    parse_too_short_count = parse_too_short_count.saturating_add(1);
                                }
                                ParseError::InvalidShredVariant(_) => {
                                    parse_invalid_variant_count =
                                        parse_invalid_variant_count.saturating_add(1);
                                }
                                ParseError::InvalidDataSize(_) => {
                                    parse_invalid_data_size_count =
                                        parse_invalid_data_size_count.saturating_add(1);
                                }
                                ParseError::InvalidCodingHeader { .. } => {
                                    parse_invalid_coding_header_count =
                                        parse_invalid_coding_header_count.saturating_add(1);
                                }
                            }
                            parse_other_count = parse_error_count
                                .saturating_sub(parse_too_short_count)
                                .saturating_sub(parse_invalid_variant_count)
                                .saturating_sub(parse_invalid_data_size_count)
                                .saturating_sub(parse_invalid_coding_header_count);
                            if parse_error_count <= INITIAL_DEBUG_SAMPLE_LOG_LIMIT {
                                tracing::debug!(source = %source_addr, error = %error, "dropping non-shred or malformed packet");
                            }
                            continue;
                        }
                    };
                    if let Some(cache) = shred_dedupe_cache.as_mut() {
                        match cache.observe_shred(packet_bytes.as_ref(), &parsed_shred, observed_at)
                        {
                            ShredDedupeObservation::Accepted => {}
                            ShredDedupeObservation::Duplicate => {
                                dedupe_ingress_duplicate_drop_count =
                                    dedupe_ingress_duplicate_drop_count.saturating_add(1);
                                crate::runtime_metrics::observe_shred_dedupe_drops(
                                    ShredDedupeStage::Ingress,
                                    1,
                                    0,
                                );
                                continue;
                            }
                            ShredDedupeObservation::Conflict => {
                                dedupe_ingress_conflict_drop_count =
                                    dedupe_ingress_conflict_drop_count.saturating_add(1);
                                crate::runtime_metrics::observe_shred_dedupe_drops(
                                    ShredDedupeStage::Ingress,
                                    0,
                                    1,
                                );
                                if dedupe_ingress_conflict_drop_count
                                    <= INITIAL_DEBUG_SAMPLE_LOG_LIMIT
                                {
                                    tracing::warn!(
                                        source = %source_addr,
                                        slot = parsed_shred_slot(&parsed_shred),
                                        index = parsed_shred_index(&parsed_shred),
                                        fec_set_index = parsed_shred_fec_set_index(&parsed_shred),
                                        "dropping conflicting duplicate shred before dispatch"
                                    );
                                }
                                continue;
                            }
                        }
                    }
                    if let Some(cache) = relay_cache.as_ref() {
                        let outcome = cache.insert(&packet_bytes, &parsed_shred, observed_at);
                        if outcome.inserted {
                            relay_cache_inserts = relay_cache_inserts.saturating_add(1);
                        }
                        if outcome.replaced {
                            relay_cache_replacements =
                                relay_cache_replacements.saturating_add(1);
                        }
                        relay_cache_evictions = relay_cache_evictions
                            .saturating_add(u64::try_from(outcome.evicted).unwrap_or(u64::MAX));
                    }
                    if plugin_hooks_enabled {
                        plugin_host.on_shred(ShredEvent {
                            source: source_addr,
                            packet: Arc::clone(&packet_bytes),
                            parsed: Arc::new(parsed_shred.clone()),
                        });
                    }
                    #[cfg(feature = "gossip-bootstrap")]
                    if udp_relay_enabled
                        && udp_relay_socket.is_some()
                        && !udp_relay_peers.is_empty()
                    {
                        let source_is_turbine = matches!(
                            source_addr.port(),
                            TURBINE_PRIMARY_SOURCE_PORT | TURBINE_SECONDARY_SOURCE_PORT
                        );
                        if udp_relay_require_turbine_source_ports && !source_is_turbine {
                            udp_relay_source_filtered_packets =
                                udp_relay_source_filtered_packets.saturating_add(1);
                        } else if udp_relay_backoff_until
                            .is_some_and(|backoff_until| observed_at < backoff_until)
                        {
                            udp_relay_backoff_drops = udp_relay_backoff_drops.saturating_add(1);
                        } else {
                            udp_relay_backoff_until = None;
                            if observed_at.saturating_duration_since(udp_relay_window_started)
                                >= Duration::from_secs(1)
                            {
                                udp_relay_window_started = observed_at;
                                udp_relay_sends_in_window = 0;
                            }
                            let sends_remaining = udp_relay_max_sends_per_sec
                                .saturating_sub(udp_relay_sends_in_window);
                            if sends_remaining == 0 {
                                udp_relay_rate_limited_packets =
                                    udp_relay_rate_limited_packets.saturating_add(1);
                            } else if let Some(socket) = udp_relay_socket.as_ref() {
                                let fanout = udp_relay_fanout
                                    .min(udp_relay_peers.len())
                                    .min(usize::try_from(sends_remaining).unwrap_or(usize::MAX));
                                if fanout == 0 {
                                    udp_relay_rate_limited_packets =
                                        udp_relay_rate_limited_packets.saturating_add(1);
                                } else {
                                    let mut sent_any = false;
                                    let peers_len = udp_relay_peers.len();
                                    if udp_relay_rr_cursor >= peers_len {
                                        udp_relay_rr_cursor = 0;
                                    }
                                    let mut cursor = udp_relay_rr_cursor;
                                    for _ in 0..fanout {
                                        if udp_relay_sends_in_window >= udp_relay_max_sends_per_sec
                                        {
                                            udp_relay_rate_limited_packets =
                                                udp_relay_rate_limited_packets.saturating_add(1);
                                            break;
                                        }
                                        let Some(&peer) = udp_relay_peers.get(cursor) else {
                                            break;
                                        };
                                        cursor = cursor.checked_add(1).unwrap_or(0);
                                        if cursor >= peers_len {
                                            cursor = 0;
                                        }
                                        if peer == source_addr {
                                            continue;
                                        }
                                        udp_relay_send_attempts =
                                            udp_relay_send_attempts.saturating_add(1);
                                        match socket.send_to(packet_bytes.as_ref(), peer) {
                                            Ok(_) => {
                                                udp_relay_send_error_streak = 0;
                                                udp_relay_sends_in_window =
                                                    udp_relay_sends_in_window.saturating_add(1);
                                                sent_any = true;
                                            }
                                            Err(error) => {
                                                udp_relay_send_errors =
                                                    udp_relay_send_errors.saturating_add(1);
                                                udp_relay_send_error_streak =
                                                    udp_relay_send_error_streak.saturating_add(1);
                                                if udp_relay_send_error_streak
                                                    >= udp_relay_send_error_backoff_threshold
                                                {
                                                    udp_relay_backoff_events =
                                                        udp_relay_backoff_events.saturating_add(1);
                                                    udp_relay_backoff_until = observed_at.checked_add(
                                                        Duration::from_millis(
                                                            udp_relay_send_error_backoff_ms,
                                                        ),
                                                    );
                                                    udp_relay_send_error_streak = 0;
                                                    break;
                                                }
                                                if udp_relay_send_errors
                                                    <= INITIAL_DEBUG_SAMPLE_LOG_LIMIT
                                                {
                                                    tracing::debug!(
                                                        peer = %peer,
                                                        error = %error,
                                                        "udp relay send failed"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    udp_relay_rr_cursor = cursor;
                                    if sent_any {
                                        udp_relay_forwarded_packets =
                                            udp_relay_forwarded_packets.saturating_add(1);
                                    }
                                }
                            }
                        }
                    }

                    let worker_index = packet_worker_assignments.worker_for(
                        parsed_shred_slot(&parsed_shred),
                        parsed_shred_fec_set_index(&parsed_shred),
                        packet_batch_dispatch_scratch.worker_loads(),
                    );
                    if let Some(batch) =
                        packet_batch_dispatch_scratch.worker_batch_mut(worker_index)
                    {
                        batch.push(PacketWorkerInput {
                            source: source_addr,
                            packet_bytes,
                            parsed_header: parsed_shred,
                        });
                        packet_batch_dispatch_scratch.bump_worker_load(worker_index);
                    }
                }
                for worker_index in 0..packet_batch_dispatch_scratch.worker_count() {
                    let packets = packet_batch_dispatch_scratch.take_worker_batch(worker_index);
                    match packet_worker_pool.dispatch_worker_batch(worker_index, packets) {
                        DispatchWorkerBatchOutcome::Enqueued => {}
                        DispatchWorkerBatchOutcome::Dropped(packets) => {
                            packet_batch_dispatch_scratch
                                .recycle_worker_batch(worker_index, packets);
                        }
                        DispatchWorkerBatchOutcome::Closed(packets) => {
                            packet_batch_dispatch_scratch
                                .recycle_worker_batch(worker_index, packets);
                            break 'event_loop;
                        }
                    }
                }
            }
            maybe_repair_result = async {
                if !repair_driver_enabled {
                    return None;
                }
                match repair_result_rx.as_mut() {
                    Some(result_rx) => result_rx.recv().await,
                    None => None,
                }
            }, if repair_driver_enabled => {
                #[cfg(feature = "gossip-bootstrap")]
                {
                    let Some(result) = maybe_repair_result else {
                        repair_result_rx = None;
                        continue;
                    };
                    match result {
                        RepairOutcome::RequestSent { peer_addr, .. } => {
                            repair_requests_sent = repair_requests_sent.saturating_add(1);
                            if log_repair_peer_traffic {
                                repair_request_sent_logs =
                                    repair_request_sent_logs.saturating_add(1);
                                if repair_request_sent_logs <= INITIAL_REPAIR_TRAFFIC_LOG_LIMIT
                                    || repair_request_sent_logs
                                        .is_multiple_of(log_repair_peer_traffic_every)
                                {
                                    tracing::info!(
                                        peer = %peer_addr,
                                        sent = repair_requests_sent,
                                        "repair request sent to peer"
                                    );
                                }
                            }
                            match peer_addr.port() {
                                TURBINE_PRIMARY_SOURCE_PORT => {
                                    repair_requests_port_8899 =
                                        repair_requests_port_8899.saturating_add(1);
                                }
                                TURBINE_SECONDARY_SOURCE_PORT => {
                                    repair_requests_port_8900 =
                                        repair_requests_port_8900.saturating_add(1);
                                }
                                _ => {
                                    repair_requests_port_other =
                                        repair_requests_port_other.saturating_add(1);
                                }
                            }
                        }
                        RepairOutcome::RequestNoPeer { request } => {
                            repair_requests_no_peer = repair_requests_no_peer.saturating_add(1);
                            if let Some(outstanding_repairs) = outstanding_repairs.as_mut() {
                                outstanding_repairs.release(&request);
                            }
                        }
                        RepairOutcome::RequestError { request, error } => {
                            repair_request_errors = repair_request_errors.saturating_add(1);
                            if let Some(outstanding_repairs) = outstanding_repairs.as_mut() {
                                outstanding_repairs.release(&request);
                            }
                            if repair_request_errors <= INITIAL_DEBUG_SAMPLE_LOG_LIMIT {
                                tracing::warn!(
                                    slot = request.slot,
                                    index = request.index,
                                    kind = ?request.kind,
                                    error = %error,
                                    "failed to send repair request"
                                );
                            }
                        }
                        RepairOutcome::ResponsePingHandledFrom { source } => {
                            repair_response_pings = repair_response_pings.saturating_add(1);
                            if log_repair_peer_traffic {
                                repair_response_ping_logs =
                                    repair_response_ping_logs.saturating_add(1);
                                if repair_response_ping_logs <= INITIAL_REPAIR_TRAFFIC_LOG_LIMIT
                                    || repair_response_ping_logs
                                        .is_multiple_of(log_repair_peer_traffic_every)
                                {
                                    tracing::info!(
                                        source = %source,
                                        handled = repair_response_pings,
                                        "repair ping handled and pong sent"
                                    );
                                }
                            }
                        }
                        RepairOutcome::ResponsePingError { source, error } => {
                            repair_response_ping_errors =
                                repair_response_ping_errors.saturating_add(1);
                            if repair_response_ping_errors <= INITIAL_DEBUG_SAMPLE_LOG_LIMIT {
                                tracing::warn!(
                                    source = %source,
                                    error = %error,
                                    "failed to respond to repair ping"
                                );
                            }
                        }
                        RepairOutcome::ServeRequestHandled { source, request } => {
                            repair_serve_requests_handled =
                                repair_serve_requests_handled.saturating_add(1);
                            if request.served_index.is_some() {
                                repair_serve_responses_sent =
                                    repair_serve_responses_sent.saturating_add(1);
                            } else if request.rate_limited {
                                repair_serve_rate_limited =
                                    repair_serve_rate_limited.saturating_add(1);
                                if request.rate_limited_by_peer {
                                    repair_serve_rate_limited_peer =
                                        repair_serve_rate_limited_peer.saturating_add(1);
                                }
                                if request.rate_limited_by_bytes {
                                    repair_serve_rate_limited_bytes =
                                        repair_serve_rate_limited_bytes.saturating_add(1);
                                }
                            } else {
                                repair_serve_cache_misses =
                                    repair_serve_cache_misses.saturating_add(1);
                            }
                            if log_repair_peer_traffic {
                                let kind = match request.kind {
                                    crate::repair::ServedRepairRequestKind::WindowIndex => {
                                        "window_index"
                                    }
                                    crate::repair::ServedRepairRequestKind::HighestWindowIndex => {
                                        "highest_window_index"
                                    }
                                };
                                tracing::info!(
                                    source = %source,
                                    kind,
                                    slot = request.slot,
                                    requested_index = request.requested_index,
                                    served_index = request.served_index.unwrap_or_default(),
                                    served = request.served_index.is_some(),
                                    rate_limited = request.rate_limited,
                                    rate_limited_by_peer = request.rate_limited_by_peer,
                                    rate_limited_by_bytes = request.rate_limited_by_bytes,
                                    unstaked_sender = request.unstaked_sender,
                                    "repair serve request processed"
                                );
                            }
                        }
                        RepairOutcome::ServeRequestError { source, error } => {
                            repair_serve_errors = repair_serve_errors.saturating_add(1);
                            if repair_serve_errors <= INITIAL_DEBUG_SAMPLE_LOG_LIMIT {
                                tracing::warn!(
                                    source = %source,
                                    error = %error,
                                    "failed to serve repair request"
                                );
                            }
                        }
                    }
                }
                #[cfg(not(feature = "gossip-bootstrap"))]
                {
                    let _ = maybe_repair_result;
                }
            }
            _ = repair_tick.tick(), if !ingest_closed && repair_enabled => {
                #[cfg(feature = "gossip-bootstrap")]
                {
                    if let (Some(tracker), Some(command_tx), Some(outstanding_repairs)) = (
                        missing_tracker.as_mut(),
                        repair_command_tx.as_ref(),
                        outstanding_repairs.as_mut(),
                    )
                    {
                        let tick_now = Instant::now();
                        let latest_shred_age_ms =
                            duration_to_ms_u64(tick_now.saturating_duration_since(latest_shred_updated_at));
                        let dataset_stall_age_ms = duration_to_ms_u64(
                            tick_now.saturating_duration_since(last_dataset_reconstructed_at),
                        );
                        let observed_shreds = data_count.saturating_add(code_count);
                        let stream_progress = observed_shreds > repair_last_shred_count_snapshot;
                        repair_last_shred_count_snapshot = observed_shreds;
                        repair_dynamic_stream_progress = stream_progress;
                        let tip_stalled =
                            latest_shred_age_ms >= repair_tip_stall_ms && !stream_progress;
                        let dataset_stalled =
                            dataset_stall_age_ms >= repair_dataset_stall_ms && !stream_progress;
                        repair_dynamic_stream_healthy = stream_progress
                            && latest_shred_age_ms < repair_tip_stall_ms
                            && dataset_stall_age_ms < repair_dataset_stall_ms;
                        let stall_observed = tip_stalled || dataset_stalled;
                        if stall_observed {
                            let _ = repair_stall_started_at.get_or_insert(tick_now);
                        } else {
                            repair_stall_started_at = None;
                        }
                        let stalled = repair_stall_started_at
                            .map(|stalled_at| {
                                tick_now.saturating_duration_since(stalled_at) >= repair_stall_sustain
                            })
                            .unwrap_or(false);
                        repair_dynamic_stalled = stalled;
                        repair_dynamic_dataset_stalled = dataset_stalled;
                        repair_dynamic_min_slot_lag = if stalled {
                            repair_min_slot_lag_stalled
                        } else {
                            repair_min_slot_lag
                        };
                        let repair_window_open = stall_observed || stalled;
                        repair_dynamic_max_requests_per_tick = if !repair_window_open {
                            0
                        } else if stalled {
                            repair_max_requests_per_tick_stalled
                        } else {
                            repair_max_requests_per_tick
                        };
                        repair_dynamic_max_highest_per_tick = if !repair_window_open {
                            0
                        } else if stalled {
                            repair_max_highest_per_tick_stalled
                        } else {
                            repair_max_highest_per_tick
                        };
                        repair_dynamic_max_forward_probe_per_tick = if !repair_window_open {
                            0
                        } else if stalled {
                            repair_max_forward_probe_per_tick_stalled
                        } else {
                            repair_max_forward_probe_per_tick
                        };
                        repair_dynamic_per_slot_cap = if stalled {
                            repair_per_slot_cap_stalled
                        } else {
                            repair_per_slot_cap
                        };
                        tracker.set_min_slot_lag(repair_dynamic_min_slot_lag);
                        tracker.set_per_slot_request_cap(repair_dynamic_per_slot_cap);
                        let purged = outstanding_repairs.purge_expired(tick_now);
                        repair_outstanding_purged = repair_outstanding_purged
                            .saturating_add(u64::try_from(purged).unwrap_or(u64::MAX));
                        let requests = tracker.collect_requests(
                            tick_now,
                            repair_dynamic_max_requests_per_tick,
                            repair_dynamic_max_highest_per_tick,
                            repair_dynamic_max_forward_probe_per_tick,
                        );
                        for request in requests {
                            if !outstanding_repairs.try_reserve(&request, tick_now) {
                                repair_requests_skipped_outstanding =
                                    repair_requests_skipped_outstanding.saturating_add(1);
                                continue;
                            }
                            repair_requests_total = repair_requests_total.saturating_add(1);
                            match request.kind {
                                MissingShredRequestKind::WindowIndex => {
                                    repair_requests_window_index =
                                        repair_requests_window_index.saturating_add(1);
                                }
                                MissingShredRequestKind::HighestWindowIndex => {
                                    repair_requests_highest_window_index =
                                        repair_requests_highest_window_index.saturating_add(1);
                                }
                            }
                            match command_tx.try_send(RepairCommand::Request { request }) {
                                Ok(()) => {
                                    repair_requests_enqueued =
                                        repair_requests_enqueued.saturating_add(1);
                                }
                                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                    repair_request_queue_drops =
                                        repair_request_queue_drops.saturating_add(1);
                                    outstanding_repairs.release(&request);
                                }
                                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                    repair_request_errors =
                                        repair_request_errors.saturating_add(1);
                                    outstanding_repairs.release(&request);
                                }
                            }
                        }
                    }

                    if gossip_runtime_switch_enabled {
                        let now = Instant::now();
                        let latest_shred_age_ms =
                            duration_to_ms_u64(now.saturating_duration_since(latest_shred_updated_at));
                        let latest_dataset_age_ms = duration_to_ms_u64(
                            now.saturating_duration_since(last_dataset_reconstructed_at),
                        );
                        let runtime_age_ms = duration_to_ms_u64(
                            now.saturating_duration_since(gossip_runtime_started_at),
                        );
                        let ingest_packets_seen = runtime
                            .gossip_ingest_telemetry
                            .as_ref()
                            .map(|telemetry| telemetry.snapshot().0)
                            .unwrap_or(0);
                        let no_ingest_seen = ingest_packets_seen == 0;
                        let switch_for_shred_stall =
                            latest_shred_age_ms >= gossip_runtime_switch_stall_ms;
                        let switch_for_dataset_stall =
                            latest_shred_slot.is_some()
                                && latest_dataset_age_ms >= gossip_runtime_switch_dataset_stall_ms;
                        let switch_stalled = switch_for_shred_stall || switch_for_dataset_stall;
                        if switch_stalled {
                            let _ = gossip_runtime_stall_started_at.get_or_insert(now);
                        } else {
                            gossip_runtime_stall_started_at = None;
                        }
                        let stall_ready_for_switch = gossip_runtime_stall_started_at
                            .map(|stalled_at| {
                                now.saturating_duration_since(stalled_at)
                                    >= gossip_runtime_switch_sustain
                            })
                            .unwrap_or(false);
                        let runtime_switch_warmup = if no_ingest_seen {
                            Duration::from_millis(gossip_runtime_switch_no_traffic_grace_ms)
                        } else {
                            gossip_runtime_switch_warmup
                        };
                        let runtime_ready_for_switch = runtime.gossip_runtime.is_none()
                            || now.saturating_duration_since(gossip_runtime_started_at)
                                >= runtime_switch_warmup;
                        if switch_stalled
                            && stall_ready_for_switch
                            && runtime_ready_for_switch
                            && last_gossip_runtime_switch_attempt.elapsed() >= gossip_runtime_switch_cooldown
                        {
                            last_gossip_runtime_switch_attempt = Instant::now();
                            gossip_runtime_switch_attempts =
                                gossip_runtime_switch_attempts.saturating_add(1);
                            if repair_enabled {
                                stop_repair_driver(
                                    &mut repair_command_tx,
                                    &mut repair_result_rx,
                                    &mut repair_peer_snapshot,
                                    &mut repair_driver_handle,
                                )
                                .await;
                                repair_driver_enabled = false;
                            }
                            match maybe_switch_gossip_runtime(
                                &mut runtime,
                                &packet_ingest_tx,
                                &gossip_entrypoints,
                            )
                            .await
                            {
                                Ok(Some(switched_to)) => {
                                    gossip_runtime_switch_success =
                                        gossip_runtime_switch_success.saturating_add(1);
                                    gossip_runtime_started_at = Instant::now();
                                    gossip_runtime_stall_started_at = None;
                                    latest_shred_updated_at = gossip_runtime_started_at;
                                    last_dataset_reconstructed_at = gossip_runtime_started_at;
                                    if repair_enabled
                                        && let Some(repair_client) = runtime.repair_client.take()
                                    {
                                        replace_repair_driver(
                                            repair_client,
                                            relay_cache.clone(),
                                            &mut repair_command_tx,
                                            &mut repair_result_rx,
                                            &mut repair_peer_snapshot,
                                            &mut repair_driver_handle,
                                        );
                                        repair_driver_enabled =
                                            repair_command_tx.is_some();
                                    }
                                    if let Some(cache) = shred_dedupe_cache.as_mut() {
                                        cache.clear();
                                    }
                                    sync_shred_dedupe_runtime_metrics(shred_dedupe_cache.as_ref());
                                    outstanding_repairs = Some(OutstandingRepairRequests::new(
                                        Duration::from_millis(repair_outstanding_timeout_ms),
                                    ));
                                    tracing::warn!(
                                        switched_to = %switched_to,
                                        latest_shred_age_ms,
                                        latest_dataset_age_ms,
                                        runtime_age_ms,
                                        overlap_ms = duration_to_ms_u64(gossip_runtime_switch_overlap),
                                        switch_for_shred_stall,
                                        switch_for_dataset_stall,
                                        "gossip runtime switched after stall detection"
                                    );
                                }
                                Ok(None) => {
                                    if repair_enabled
                                        && !repair_driver_enabled
                                        && let Some(repair_client) = runtime.repair_client.take()
                                    {
                                        replace_repair_driver(
                                            repair_client,
                                            relay_cache.clone(),
                                            &mut repair_command_tx,
                                            &mut repair_result_rx,
                                            &mut repair_peer_snapshot,
                                            &mut repair_driver_handle,
                                        );
                                        repair_driver_enabled = repair_command_tx.is_some();
                                    }
                                }
                                Err(error) => {
                                    gossip_runtime_switch_failures =
                                        gossip_runtime_switch_failures.saturating_add(1);
                                    if repair_enabled
                                        && !repair_driver_enabled
                                        && let Some(repair_client) = runtime.repair_client.take()
                                    {
                                        replace_repair_driver(
                                            repair_client,
                                            relay_cache.clone(),
                                            &mut repair_command_tx,
                                            &mut repair_result_rx,
                                            &mut repair_peer_snapshot,
                                            &mut repair_driver_handle,
                                        );
                                        repair_driver_enabled = repair_command_tx.is_some();
                                    }
                                    tracing::warn!(
                                        error = %error,
                                        latest_shred_age_ms,
                                        latest_dataset_age_ms,
                                        runtime_age_ms,
                                        switch_for_shred_stall,
                                        switch_for_dataset_stall,
                                        "gossip runtime switch attempt failed"
                                    );
                                }
                            }
                        }
                    }
                }
            }
            _ = control_plane_tick.tick(), if !ingest_closed => {
                #[cfg(feature = "gossip-bootstrap")]
                {
                    let now = Instant::now();
                    if udp_relay_enabled
                        && udp_relay_fanout > 0
                        && now.saturating_duration_since(udp_relay_last_refresh)
                            >= udp_relay_refresh
                    {
                        udp_relay_last_refresh = now;
                        let peers = runtime
                            .gossip_runtime
                            .as_ref()
                            .map(|gossip_runtime| {
                                collect_udp_relay_peers(
                                    gossip_runtime.cluster_info.as_ref(),
                                    runtime.gossip_identity.pubkey(),
                                    udp_relay_peer_candidates,
                                    udp_relay_fanout,
                                    udp_relay_max_peers_per_ip,
                                )
                            })
                            .unwrap_or_default();
                        udp_relay_candidates =
                            u64::try_from(peers.total_candidates).unwrap_or(u64::MAX);
                        udp_relay_peers = peers.selected_peers;
                        udp_relay_refreshes = udp_relay_refreshes.saturating_add(1);
                    }
                }
                #[cfg(feature = "gossip-bootstrap")]
                if plugin_hooks_enabled
                    && let Some(gossip_runtime) = runtime.gossip_runtime.as_ref()
                {
                    let now = Instant::now();
                    if let Some(topology_event) = topology_tracker.maybe_build_event(
                        gossip_runtime.cluster_info.as_ref(),
                        latest_shred_slot,
                        runtime.active_gossip_entrypoint.clone(),
                        now,
                    ) {
                        if derived_state_hooks_enabled {
                            derived_state_host.on_cluster_topology(topology_event.clone());
                        }
                        plugin_host.on_cluster_topology(topology_event);
                    }
                }
            }
            _ = derived_state_checkpoint_tick.tick(), if !ingest_closed && derived_state_checkpoint_enabled => {
                let fork_snapshot = fork_tracker.snapshot();
                derived_state_host.emit_checkpoint_barrier(
                    CheckpointBarrierReason::Periodic,
                    feed_watermarks_from_fork_snapshot(fork_snapshot),
                );
            }
            _ = derived_state_recovery_tick.tick(), if !ingest_closed && derived_state_recovery_enabled => {
                if derived_state_host.has_unhealthy_consumers() {
                    let recovery_report = derived_state_host.recover_consumers();
                    tracing::info!(
                        attempted = recovery_report.attempted,
                        recovered = recovery_report.recovered,
                        still_pending = recovery_report.still_pending,
                        rebuild_required = recovery_report.rebuild_required,
                        pending_recovery = ?derived_state_host.consumers_pending_recovery(),
                        rebuild_consumers = ?derived_state_host.consumers_requiring_rebuild(),
                        "derived-state recovery attempt completed"
                    );
                }
            }
            _ = telemetry_tick.tick(), if !ingest_closed => {
                if packet_count == 0 && !logged_waiting_for_packets {
                    tracing::info!(
                        "waiting for ingress packets; check SOF_BIND / SOF_GOSSIP_ENTRYPOINT configuration"
                    );
                    logged_waiting_for_packets = true;
                }
                #[cfg(feature = "gossip-bootstrap")]
                {
                    if let Some(peer_snapshot) = repair_peer_snapshot.as_ref() {
                        packet_worker_pool
                            .update_known_pubkeys(peer_snapshot.shared_get().known_pubkeys.clone());
                    }
                }
                #[cfg(feature = "gossip-bootstrap")]
                let (repair_peer_total, repair_peer_active) = repair_peer_snapshot
                    .as_ref()
                    .map(|snapshot| {
                        let snapshot = snapshot.shared_get();
                        (
                            u64::try_from(snapshot.total_candidates).unwrap_or(u64::MAX),
                            u64::try_from(snapshot.active_candidates).unwrap_or(u64::MAX),
                        )
                    })
                    .unwrap_or((0, 0));
                #[cfg(not(feature = "gossip-bootstrap"))]
                let (repair_peer_total, repair_peer_active) = (0_u64, 0_u64);
                #[cfg(feature = "gossip-bootstrap")]
                let gossip_active_entrypoint =
                    runtime.active_gossip_entrypoint.as_deref().unwrap_or("");
                #[cfg(not(feature = "gossip-bootstrap"))]
                let gossip_active_entrypoint = "";
                #[cfg(feature = "gossip-bootstrap")]
                let (gossip_switch_attempts, gossip_switch_successes, gossip_switch_fails) = (
                    gossip_runtime_switch_attempts,
                    gossip_runtime_switch_success,
                    gossip_runtime_switch_failures,
                );
                #[cfg(not(feature = "gossip-bootstrap"))]
                let (gossip_switch_attempts, gossip_switch_successes, gossip_switch_fails) =
                    (0_u64, 0_u64, 0_u64);
                #[cfg(feature = "gossip-bootstrap")]
                let (
                    gossip_switch_enabled,
                    gossip_switch_stall_ms,
                    gossip_switch_dataset_stall_ms,
                    gossip_switch_warmup_ms,
                    gossip_switch_overlap_ms,
                    gossip_switch_sustain_ms,
                ) = (
                    gossip_runtime_switch_enabled,
                    gossip_runtime_switch_stall_ms,
                    gossip_runtime_switch_dataset_stall_ms,
                    duration_to_ms_u64(gossip_runtime_switch_warmup),
                    duration_to_ms_u64(gossip_runtime_switch_overlap),
                    duration_to_ms_u64(gossip_runtime_switch_sustain),
                );
                #[cfg(not(feature = "gossip-bootstrap"))]
                let (
                    gossip_switch_enabled,
                    gossip_switch_stall_ms,
                    gossip_switch_dataset_stall_ms,
                    gossip_switch_warmup_ms,
                    gossip_switch_overlap_ms,
                    gossip_switch_sustain_ms,
                ) = (false, 0_u64, 0_u64, 0_u64, 0_u64, 0_u64);
                #[cfg(feature = "gossip-bootstrap")]
                let gossip_runtime_age_ms = duration_to_ms_u64(
                    Instant::now().saturating_duration_since(gossip_runtime_started_at),
                );
                #[cfg(not(feature = "gossip-bootstrap"))]
                let gossip_runtime_age_ms = 0_u64;
                #[cfg(feature = "gossip-bootstrap")]
                let gossip_runtime_stall_age_ms = gossip_runtime_stall_started_at
                    .map(|stalled_at| duration_to_ms_u64(Instant::now().saturating_duration_since(stalled_at)))
                    .unwrap_or(0_u64);
                #[cfg(not(feature = "gossip-bootstrap"))]
                let gossip_runtime_stall_age_ms = 0_u64;
                #[cfg(feature = "gossip-bootstrap")]
                let (
                    udp_relay_peers_telemetry,
                    udp_relay_refresh_ms_telemetry,
                    udp_relay_peer_candidates_telemetry,
                    udp_relay_fanout_telemetry,
                    udp_relay_max_sends_per_sec_telemetry,
                    udp_relay_max_peers_per_ip_telemetry,
                    udp_relay_require_turbine_source_ports_telemetry,
                    udp_relay_send_error_backoff_ms_telemetry,
                    udp_relay_send_error_backoff_threshold_telemetry,
                ) = (
                    u64::try_from(udp_relay_peers.len()).unwrap_or(u64::MAX),
                    udp_relay_refresh_ms,
                    u64::try_from(udp_relay_peer_candidates).unwrap_or(u64::MAX),
                    u64::try_from(udp_relay_fanout).unwrap_or(u64::MAX),
                    udp_relay_max_sends_per_sec,
                    u64::try_from(udp_relay_max_peers_per_ip).unwrap_or(u64::MAX),
                    if udp_relay_require_turbine_source_ports {
                        1
                    } else {
                        0
                    },
                    udp_relay_send_error_backoff_ms,
                    udp_relay_send_error_backoff_threshold,
                );
                #[cfg(not(feature = "gossip-bootstrap"))]
                let (
                    udp_relay_peers_telemetry,
                    udp_relay_refresh_ms_telemetry,
                    udp_relay_peer_candidates_telemetry,
                    udp_relay_fanout_telemetry,
                    udp_relay_max_sends_per_sec_telemetry,
                    udp_relay_max_peers_per_ip_telemetry,
                    udp_relay_require_turbine_source_ports_telemetry,
                    udp_relay_send_error_backoff_ms_telemetry,
                    udp_relay_send_error_backoff_threshold_telemetry,
                ) = (0_u64, 0_u64, 0_u64, 0_u64, 0_u64, 0_u64, 0_u64, 0_u64, 0_u64);
                let coverage = coverage_window.snapshot();
                let fork_snapshot = fork_tracker.snapshot();
                let dataset_jobs_enqueued = dataset_jobs_enqueued_count.load(Ordering::Relaxed);
                let dataset_jobs_started = dataset_jobs_started_count.load(Ordering::Relaxed);
                let dataset_jobs_completed = dataset_jobs_completed_count.load(Ordering::Relaxed);
                let dataset_queue_drops = dataset_queue_drop_count.load(Ordering::Relaxed);
                let dataset_worker_count = dataset_worker_pool.queues().len();
                let dataset_queue_capacity_total =
                    dataset_queue_capacity.saturating_mul(dataset_worker_count);
                let dataset_jobs_pending = dataset_jobs_enqueued
                    .saturating_sub(dataset_jobs_completed.saturating_add(dataset_queue_drops));
                let dataset_queue_depth = dataset_worker_pool
                    .queues()
                    .iter()
                    .map(DatasetDispatchQueue::len)
                    .sum::<usize>();
                #[cfg(feature = "gossip-bootstrap")]
                let (
                    ingest_packets_seen,
                    ingest_last_packet_unix_ms,
                    ingest_sent_packets,
                    ingest_sent_batches,
                    ingest_dropped_packets,
                    ingest_dropped_batches,
                    ingest_rxq_ovfl_drops,
                ) = runtime
                    .gossip_ingest_telemetry
                    .as_ref()
                    .map(|telemetry| {
                        let (packets_seen, last_packet_unix_ms) = telemetry.snapshot();
                        (
                            packets_seen,
                            last_packet_unix_ms,
                            telemetry.sent_packets(),
                            telemetry.sent_batches(),
                            telemetry.dropped_packets(),
                            telemetry.dropped_batches(),
                            telemetry.rxq_ovfl_drops(),
                        )
                    })
                    .unwrap_or((0, 0, 0, 0, 0, 0, 0));
                #[cfg(not(feature = "gossip-bootstrap"))]
                let (
                    ingest_packets_seen,
                    ingest_last_packet_unix_ms,
                    ingest_sent_packets,
                    ingest_sent_batches,
                    ingest_dropped_packets,
                    ingest_dropped_batches,
                    ingest_rxq_ovfl_drops,
                ) = (0_u64, 0_u64, 0_u64, 0_u64, 0_u64, 0_u64, 0_u64);
                let ingest_last_packet_age_ms = if ingest_last_packet_unix_ms == 0 {
                    u64::MAX
                } else {
                    current_unix_ms().saturating_sub(ingest_last_packet_unix_ms)
                };
                let extension_dispatch = if extension_hooks_enabled {
                    collect_extension_dispatch_telemetry(
                        extension_host.dispatch_metrics_by_extension(),
                    )
                } else {
                    ExtensionDispatchTelemetrySnapshot::default()
                };
                let derived_state_last_sequence = derived_state_host
                    .last_emitted_sequence()
                    .map_or(0_u64, |sequence| sequence.0);
                let derived_state_healthy_consumers =
                    u64::try_from(derived_state_host.healthy_consumer_count()).unwrap_or(u64::MAX);
                let derived_state_unhealthy_names = derived_state_host.unhealthy_consumer_names();
                let derived_state_unhealthy_consumers =
                    u64::try_from(derived_state_unhealthy_names.len()).unwrap_or(u64::MAX);
                let derived_state_pending_recovery_names =
                    derived_state_host.consumers_pending_recovery();
                let derived_state_pending_recovery =
                    u64::try_from(derived_state_pending_recovery_names.len()).unwrap_or(u64::MAX);
                let derived_state_rebuild_names =
                    derived_state_host.consumers_requiring_rebuild();
                let derived_state_rebuild_required =
                    u64::try_from(derived_state_rebuild_names.len()).unwrap_or(u64::MAX);
                let derived_state_fault_total = derived_state_host.fault_count();
                let derived_state_consumer_telemetry = derived_state_host.consumer_telemetry();
                let derived_state_replay_telemetry =
                    derived_state_host.replay_telemetry().unwrap_or_default();
                sync_shred_dedupe_runtime_metrics(shred_dedupe_cache.as_ref());
                let runtime_stage_metrics = crate::runtime_metrics::snapshot();
                telemetry_tick_count = telemetry_tick_count.saturating_add(1);
                let dataset_queue_pressure = dataset_queue_capacity_total > 0
                    && dataset_queue_depth >= (dataset_queue_capacity_total / 2).max(1);
                let packet_worker_queue_pressure = packet_worker_queue_capacity > 0
                    && runtime_stage_metrics.packet_worker_queue_depth
                        >= u64::try_from((packet_worker_queue_capacity / 2).max(1))
                            .unwrap_or(u64::MAX);
                let dedupe_capacity_pressure =
                    runtime_stage_metrics.shred_dedupe_capacity_evictions_total > 0;
                let telemetry_requires_warning = ingest_dropped_packets > 0
                    || ingest_dropped_batches > 0
                    || ingest_rxq_ovfl_drops > 0
                    || dataset_queue_drops > 0
                    || dataset_queue_pressure
                    || packet_worker_queue_pressure
                    || dedupe_capacity_pressure
                    || runtime_stage_metrics.packet_worker_dropped_batches_total > 0
                    || runtime_stage_metrics.packet_worker_dropped_packets_total > 0
                    || tx_event_drop_count.load(Ordering::Relaxed) > 0
                    || extension_dispatch.dropped_events > 0
                    || derived_state_unhealthy_consumers > 0
                    || derived_state_pending_recovery > 0
                    || derived_state_rebuild_required > 0
                    || derived_state_fault_total > 0;
                let telemetry_log_now = telemetry_requires_warning
                    || telemetry_tick_count.checked_rem(TELEMETRY_INFO_EVERY_TICKS).unwrap_or(0)
                        == 0;
                if telemetry_log_now && telemetry_requires_warning {
                    tracing::warn!(
                        packets = packet_count,
                        source_8899_packets = source_port_8899_packets,
                        source_8900_packets = source_port_8900_packets,
                        source_other_packets = source_port_other_packets,
                        data = data_count,
                        code = code_count,
                        source_8899_data = source_port_8899_data,
                        source_8900_data = source_port_8900_data,
                        source_other_data = source_port_other_data,
                        source_8899_code = source_port_8899_code,
                        source_8900_code = source_port_8900_code,
                        source_other_code = source_port_other_code,
                        ingest_packets_seen,
                        ingest_sent_packets,
                        ingest_sent_batches,
                        ingest_dropped_packets,
                        ingest_dropped_batches,
                        ingest_rxq_ovfl_drops,
                        ingest_last_packet_age_ms,
                        recovered_data = recovered_data_count,
                        data_complete = data_complete_count,
                        last_in_slot = last_in_slot_count,
                        dataset_ranges_emitted,
                        dataset_ranges_emitted_from_recovered,
                        parse_errors = parse_error_count,
                        parse_too_short = parse_too_short_count,
                        parse_invalid_variant = parse_invalid_variant_count,
                        parse_invalid_data_size = parse_invalid_data_size_count,
                        parse_invalid_coding = parse_invalid_coding_header_count,
                        parse_other = parse_other_count,
                        relay_cache_enabled = relay_cache.is_some(),
                        relay_cache_window_ms = relay_cache_window_ms,
                        relay_cache_max_shreds = relay_cache_max_shreds,
                        relay_cache_entries = relay_cache.as_ref().map_or(0, SharedRelayCache::len),
                        relay_cache_inserts = relay_cache_inserts,
                        relay_cache_replacements = relay_cache_replacements,
                        relay_cache_evictions = relay_cache_evictions,
                        udp_relay_enabled = udp_relay_enabled,
                        udp_relay_refresh_ms = udp_relay_refresh_ms_telemetry,
                        udp_relay_peer_candidates = udp_relay_peer_candidates_telemetry,
                        udp_relay_fanout = udp_relay_fanout_telemetry,
                        udp_relay_max_sends_per_sec =
                            udp_relay_max_sends_per_sec_telemetry,
                        udp_relay_max_peers_per_ip = udp_relay_max_peers_per_ip_telemetry,
                        udp_relay_require_turbine_source_ports =
                            udp_relay_require_turbine_source_ports_telemetry,
                        udp_relay_send_error_backoff_ms =
                            udp_relay_send_error_backoff_ms_telemetry,
                        udp_relay_send_error_backoff_threshold =
                            udp_relay_send_error_backoff_threshold_telemetry,
                        udp_relay_candidates = udp_relay_candidates,
                        udp_relay_peers = udp_relay_peers_telemetry,
                        udp_relay_refreshes = udp_relay_refreshes,
                        udp_relay_forwarded_packets = udp_relay_forwarded_packets,
                        udp_relay_send_attempts = udp_relay_send_attempts,
                        udp_relay_send_errors = udp_relay_send_errors,
                        udp_relay_rate_limited_packets =
                            udp_relay_rate_limited_packets,
                        udp_relay_source_filtered_packets = udp_relay_source_filtered_packets,
                        udp_relay_backoff_events = udp_relay_backoff_events,
                        udp_relay_backoff_drops = udp_relay_backoff_drops,
                        dedupe_enabled = shred_dedupe_cache.is_some(),
                        dedupe_capacity = dedupe_capacity,
                        dedupe_ttl_ms = dedupe_ttl_ms,
                        dedupe_entries = runtime_stage_metrics.shred_dedupe_entries,
                        dedupe_max_entries =
                            runtime_stage_metrics.shred_dedupe_max_entries,
                        dedupe_queue_depth =
                            runtime_stage_metrics.shred_dedupe_queue_depth,
                        dedupe_max_queue_depth =
                            runtime_stage_metrics.shred_dedupe_max_queue_depth,
                        dedupe_capacity_evictions =
                            runtime_stage_metrics.shred_dedupe_capacity_evictions_total,
                        dedupe_expired_evictions =
                            runtime_stage_metrics.shred_dedupe_expired_evictions_total,
                        dedupe_ingress_duplicate_drops =
                            dedupe_ingress_duplicate_drop_count,
                        dedupe_ingress_conflict_drops =
                            dedupe_ingress_conflict_drop_count,
                        dedupe_canonical_duplicate_drops =
                            dedupe_canonical_duplicate_drop_count,
                        dedupe_canonical_conflict_drops =
                            dedupe_canonical_conflict_drop_count,
                        dedupe_duplicate_drops = dedupe_ingress_duplicate_drop_count
                            .saturating_add(dedupe_canonical_duplicate_drop_count),
                        dedupe_conflict_drops = dedupe_ingress_conflict_drop_count
                            .saturating_add(dedupe_canonical_conflict_drop_count),
                        tx_event_drops = tx_event_drop_count.load(Ordering::Relaxed),
                        runtime_extension_active = extension_dispatch.active_extensions,
                        runtime_extension_dispatched = extension_dispatch.dispatched_events,
                        runtime_extension_dropped = extension_dispatch.dropped_events,
                        runtime_extension_queue_depth = extension_dispatch.queue_depth,
                        runtime_extension_max_queue_depth = extension_dispatch.max_queue_depth,
                        runtime_extension_max_avg_dispatch_lag_us =
                            extension_dispatch.max_avg_dispatch_lag_us,
                        runtime_extension_max_dispatch_lag_us =
                            extension_dispatch.max_dispatch_lag_us,
                        derived_state_enabled = derived_state_hooks_enabled,
                        derived_state_checkpoint_interval_ms = derived_state_checkpoint_interval_ms,
                        derived_state_recovery_interval_ms = derived_state_recovery_interval_ms,
                        derived_state_healthy_consumers = derived_state_healthy_consumers,
                        derived_state_unhealthy_consumers = derived_state_unhealthy_consumers,
                        derived_state_pending_recovery = derived_state_pending_recovery,
                        derived_state_rebuild_required = derived_state_rebuild_required,
                        derived_state_fault_total = derived_state_fault_total,
                        derived_state_last_sequence = derived_state_last_sequence,
                        derived_state_replay_enabled = derived_state_replay_telemetry.enabled,
                        derived_state_replay_backend = %derived_state_replay_telemetry.backend,
                        derived_state_replay_retained_sessions =
                            derived_state_replay_telemetry.retained_sessions,
                        derived_state_replay_retained_envelopes =
                            derived_state_replay_telemetry.retained_envelopes,
                        derived_state_replay_truncated_envelopes =
                            derived_state_replay_telemetry.truncated_envelopes,
                        derived_state_replay_append_failures =
                            derived_state_replay_telemetry.append_failures,
                        derived_state_replay_load_failures =
                            derived_state_replay_telemetry.load_failures,
                        derived_state_replay_compactions =
                            derived_state_replay_telemetry.compactions,
                        derived_state_pending_recovery_consumers =
                            ?derived_state_pending_recovery_names,
                        derived_state_rebuild_consumers = ?derived_state_rebuild_names,
                        derived_state_consumers = ?derived_state_consumer_telemetry,
                        dataset_decode_failures = dataset_decode_fail_count.load(Ordering::Relaxed),
                        dataset_tail_skips = dataset_tail_skip_count.load(Ordering::Relaxed),
                        dataset_duplicate_drops = dataset_duplicate_drop_count.load(Ordering::Relaxed),
                        dataset_queue_capacity_per_worker = dataset_queue_capacity,
                        dataset_queue_capacity_total = dataset_queue_capacity_total,
                        dataset_worker_count = dataset_worker_count,
                        dataset_queue_drops = dataset_queue_drops,
                        dataset_queue_depth = dataset_queue_depth,
                        dataset_jobs_enqueued,
                        dataset_jobs_started,
                        dataset_jobs_completed,
                        dataset_jobs_pending,
                        packet_worker_queue_depth = runtime_stage_metrics.packet_worker_queue_depth,
                        packet_worker_max_queue_depth =
                            runtime_stage_metrics.packet_worker_max_queue_depth,
                        packet_worker_queue_depth_local = packet_worker_pool.queue_depth(),
                        packet_worker_max_queue_depth_local = packet_worker_pool.max_queue_depth(),
                        packet_worker_queue_overflow_policy =
                            PACKET_WORKER_QUEUE_OVERFLOW_POLICY,
                        packet_worker_queue_depths = ?packet_worker_pool.worker_queue_depths(),
                        packet_worker_dropped_batches =
                            runtime_stage_metrics.packet_worker_dropped_batches_total,
                        packet_worker_dropped_packets =
                            runtime_stage_metrics.packet_worker_dropped_packets_total,
                        dataset_slots_tracked = dataset_reassembler.tracked_slots(),
                        dataset_max_tracked_slots = dataset_max_tracked_slots,
                        fec_sets_tracked = packet_worker_pool.tracked_fec_sets(),
                        fec_sets_tracked_by_worker = ?packet_worker_pool.tracked_fec_sets_by_worker(),
                        fec_max_tracked_sets = fec_max_tracked_sets,
                        vote_only = vote_only_count,
                        mixed = mixed_count,
                        non_vote = non_vote_count,
                        verify_verified = verify_verified_count,
                        verify_unknown_leader = verify_unknown_leader_count,
                        verify_invalid_merkle = verify_invalid_merkle_count,
                        verify_invalid_signature = verify_invalid_signature_count,
                        verify_malformed = verify_malformed_count,
                        verify_dropped = verify_dropped_count,
                        verify_recovered_enabled = verify_recovered_shreds,
                        repair_requests_total = repair_requests_total,
                        repair_requests_enqueued = repair_requests_enqueued,
                        repair_requests_sent = repair_requests_sent,
                        repair_requests_no_peer = repair_requests_no_peer,
                        repair_request_errors = repair_request_errors,
                        repair_request_queue_drops = repair_request_queue_drops,
                        repair_requests_port_8899 = repair_requests_port_8899,
                        repair_requests_port_8900 = repair_requests_port_8900,
                        repair_requests_port_other = repair_requests_port_other,
                        repair_requests_window_index = repair_requests_window_index,
                        repair_requests_highest_window_index = repair_requests_highest_window_index,
                        repair_requests_skipped_outstanding = repair_requests_skipped_outstanding,
                        repair_outstanding_entries = outstanding_repairs
                            .as_ref()
                            .map_or(0, OutstandingRepairRequests::len),
                        repair_outstanding_purged = repair_outstanding_purged,
                        repair_outstanding_cleared_on_receive = repair_outstanding_cleared_on_receive,
                        repair_outstanding_timeout_ms = repair_outstanding_timeout_ms,
                        repair_tip_stall_ms = repair_tip_stall_ms,
                        repair_dataset_stall_ms = repair_dataset_stall_ms,
                        repair_stall_sustain_ms = repair_stall_sustain_ms,
                        repair_tip_probe_ahead_slots = repair_tip_probe_ahead_slots,
                        repair_min_slot_lag = repair_min_slot_lag,
                        repair_min_slot_lag_stalled = repair_min_slot_lag_stalled,
                        repair_dynamic_stalled = repair_dynamic_stalled,
                        repair_dynamic_dataset_stalled = repair_dynamic_dataset_stalled,
                        repair_dynamic_stream_progress = repair_dynamic_stream_progress,
                        repair_dynamic_stream_healthy = repair_dynamic_stream_healthy,
                        repair_dynamic_min_slot_lag = repair_dynamic_min_slot_lag,
                        repair_per_slot_cap = repair_per_slot_cap,
                        repair_per_slot_cap_stalled = repair_per_slot_cap_stalled,
                        repair_dynamic_per_slot_cap = repair_dynamic_per_slot_cap,
                        repair_max_requests_per_tick = repair_max_requests_per_tick,
                        repair_max_requests_per_tick_stalled = repair_max_requests_per_tick_stalled,
                        repair_dynamic_max_requests_per_tick = repair_dynamic_max_requests_per_tick,
                        repair_max_highest_per_tick = repair_max_highest_per_tick,
                        repair_max_highest_per_tick_stalled = repair_max_highest_per_tick_stalled,
                        repair_dynamic_max_highest_per_tick = repair_dynamic_max_highest_per_tick,
                        repair_max_forward_probe_per_tick = repair_max_forward_probe_per_tick,
                        repair_max_forward_probe_per_tick_stalled = repair_max_forward_probe_per_tick_stalled,
                        repair_dynamic_max_forward_probe_per_tick = repair_dynamic_max_forward_probe_per_tick,
                        repair_seed_slot = 0,
                        repair_seed_slots = 0,
                        repair_seed_failures = 0,
                        repair_response_pings = repair_response_pings,
                        repair_response_ping_errors = repair_response_ping_errors,
                        repair_ping_queue_drops = repair_ping_queue_drops,
                        repair_serve_requests_enqueued = repair_serve_requests_enqueued,
                        repair_serve_requests_handled = repair_serve_requests_handled,
                        repair_serve_responses_sent = repair_serve_responses_sent,
                        repair_serve_cache_misses = repair_serve_cache_misses,
                        repair_serve_rate_limited = repair_serve_rate_limited,
                        repair_serve_rate_limited_peer = repair_serve_rate_limited_peer,
                        repair_serve_rate_limited_bytes = repair_serve_rate_limited_bytes,
                        repair_serve_errors = repair_serve_errors,
                        repair_serve_queue_drops = repair_serve_queue_drops,
                        repair_source_hint_enqueued = repair_source_hint_enqueued,
                        repair_source_hint_drops = repair_source_hint_drops,
                        repair_source_hint_buffer_drops = repair_source_hint_buffer_drops,
                        gossip_active_entrypoint = gossip_active_entrypoint,
                        gossip_runtime_switch_enabled = gossip_switch_enabled,
                        gossip_runtime_switch_stall_ms = gossip_switch_stall_ms,
                        gossip_runtime_switch_dataset_stall_ms = gossip_switch_dataset_stall_ms,
                        gossip_runtime_switch_warmup_ms = gossip_switch_warmup_ms,
                        gossip_runtime_switch_overlap_ms = gossip_switch_overlap_ms,
                        gossip_runtime_switch_sustain_ms = gossip_switch_sustain_ms,
                        gossip_runtime_switch_attempts = gossip_switch_attempts,
                        gossip_runtime_switch_successes = gossip_switch_successes,
                        gossip_runtime_switch_failures = gossip_switch_fails,
                        repair_peer_total = repair_peer_total,
                        repair_peer_active = repair_peer_active,
                        latest_shred_slot = latest_shred_slot.unwrap_or_default(),
                        fork_window_slots = fork_window_slots,
                        fork_slots_tracked = u64::try_from(fork_snapshot.tracked_slots).unwrap_or(u64::MAX),
                        fork_tip_slot = fork_snapshot.tip_slot.unwrap_or_default(),
                        fork_confirmed_slot = fork_snapshot.confirmed_slot.unwrap_or_default(),
                        fork_finalized_slot = fork_snapshot.finalized_slot.unwrap_or_default(),
                        fork_status_transitions = fork_status_transitions_total,
                        fork_reorgs = fork_reorg_count,
                        fork_orphaned_slots = fork_orphaned_slots_total,
                        tx_confirmed_slot =
                            tx_commitment_tracker.snapshot().confirmed_slot.unwrap_or_default(),
                        tx_finalized_slot =
                            tx_commitment_tracker.snapshot().finalized_slot.unwrap_or_default(),
                        latest_shred_age_ms = duration_to_ms_u64(
                            Instant::now().saturating_duration_since(latest_shred_updated_at)
                        ),
                        latest_dataset_age_ms = duration_to_ms_u64(
                            Instant::now().saturating_duration_since(last_dataset_reconstructed_at)
                        ),
                        gossip_runtime_age_ms = gossip_runtime_age_ms,
                        gossip_runtime_stall_age_ms = gossip_runtime_stall_age_ms,
                        window_slots = coverage.slots_tracked,
                        window_slots_with_tx = coverage.slots_with_tx,
                        window_tx_total = coverage.tx_total,
                        window_dataset_total = coverage.dataset_total,
                        window_data_shreds = coverage.data_shreds,
                        window_code_shreds = coverage.code_shreds,
                        window_recovered_data = coverage.recovered_data_shreds,
                        "ingest telemetry pressure detected"
                    );
                } else if telemetry_log_now {
                    tracing::info!(
                    packets = packet_count,
                    source_8899_packets = source_port_8899_packets,
                    source_8900_packets = source_port_8900_packets,
                    source_other_packets = source_port_other_packets,
                    data = data_count,
                    code = code_count,
                    source_8899_data = source_port_8899_data,
                    source_8900_data = source_port_8900_data,
                    source_other_data = source_port_other_data,
                    source_8899_code = source_port_8899_code,
                    source_8900_code = source_port_8900_code,
                    source_other_code = source_port_other_code,
                    ingest_packets_seen,
                    ingest_sent_packets,
                    ingest_sent_batches,
                    ingest_dropped_packets,
                    ingest_dropped_batches,
                    ingest_rxq_ovfl_drops,
                    ingest_last_packet_age_ms,
                    recovered_data = recovered_data_count,
                    data_complete = data_complete_count,
                    last_in_slot = last_in_slot_count,
                    dataset_ranges_emitted,
                    dataset_ranges_emitted_from_recovered,
                    parse_errors = parse_error_count,
                    parse_too_short = parse_too_short_count,
                    parse_invalid_variant = parse_invalid_variant_count,
                    parse_invalid_data_size = parse_invalid_data_size_count,
                    parse_invalid_coding = parse_invalid_coding_header_count,
                    parse_other = parse_other_count,
                    relay_cache_enabled = relay_cache.is_some(),
                    relay_cache_window_ms = relay_cache_window_ms,
                    relay_cache_max_shreds = relay_cache_max_shreds,
                    relay_cache_entries = relay_cache.as_ref().map_or(0, SharedRelayCache::len),
                    relay_cache_inserts = relay_cache_inserts,
                    relay_cache_replacements = relay_cache_replacements,
                    relay_cache_evictions = relay_cache_evictions,
                    udp_relay_enabled = udp_relay_enabled,
                    udp_relay_refresh_ms = udp_relay_refresh_ms_telemetry,
                    udp_relay_peer_candidates = udp_relay_peer_candidates_telemetry,
                    udp_relay_fanout = udp_relay_fanout_telemetry,
                    udp_relay_max_sends_per_sec =
                        udp_relay_max_sends_per_sec_telemetry,
                    udp_relay_max_peers_per_ip = udp_relay_max_peers_per_ip_telemetry,
                    udp_relay_require_turbine_source_ports =
                        udp_relay_require_turbine_source_ports_telemetry,
                    udp_relay_send_error_backoff_ms =
                        udp_relay_send_error_backoff_ms_telemetry,
                    udp_relay_send_error_backoff_threshold =
                        udp_relay_send_error_backoff_threshold_telemetry,
                    udp_relay_candidates = udp_relay_candidates,
                    udp_relay_peers = udp_relay_peers_telemetry,
                    udp_relay_refreshes = udp_relay_refreshes,
                    udp_relay_forwarded_packets = udp_relay_forwarded_packets,
                    udp_relay_send_attempts = udp_relay_send_attempts,
                    udp_relay_send_errors = udp_relay_send_errors,
                    udp_relay_rate_limited_packets =
                        udp_relay_rate_limited_packets,
                    udp_relay_source_filtered_packets = udp_relay_source_filtered_packets,
                    udp_relay_backoff_events = udp_relay_backoff_events,
                    udp_relay_backoff_drops = udp_relay_backoff_drops,
                    dedupe_enabled = shred_dedupe_cache.is_some(),
                    dedupe_capacity = dedupe_capacity,
                    dedupe_ttl_ms = dedupe_ttl_ms,
                    dedupe_entries = runtime_stage_metrics.shred_dedupe_entries,
                    dedupe_max_entries =
                        runtime_stage_metrics.shred_dedupe_max_entries,
                    dedupe_queue_depth =
                        runtime_stage_metrics.shred_dedupe_queue_depth,
                    dedupe_max_queue_depth =
                        runtime_stage_metrics.shred_dedupe_max_queue_depth,
                    dedupe_capacity_evictions =
                        runtime_stage_metrics.shred_dedupe_capacity_evictions_total,
                    dedupe_expired_evictions =
                        runtime_stage_metrics.shred_dedupe_expired_evictions_total,
                    dedupe_ingress_duplicate_drops =
                        dedupe_ingress_duplicate_drop_count,
                    dedupe_ingress_conflict_drops =
                        dedupe_ingress_conflict_drop_count,
                    dedupe_canonical_duplicate_drops =
                        dedupe_canonical_duplicate_drop_count,
                    dedupe_canonical_conflict_drops =
                        dedupe_canonical_conflict_drop_count,
                    dedupe_duplicate_drops = dedupe_ingress_duplicate_drop_count
                        .saturating_add(dedupe_canonical_duplicate_drop_count),
                    dedupe_conflict_drops = dedupe_ingress_conflict_drop_count
                        .saturating_add(dedupe_canonical_conflict_drop_count),
                    tx_event_drops = tx_event_drop_count.load(Ordering::Relaxed),
                    runtime_extension_active = extension_dispatch.active_extensions,
                    runtime_extension_dispatched = extension_dispatch.dispatched_events,
                    runtime_extension_dropped = extension_dispatch.dropped_events,
                    runtime_extension_queue_depth = extension_dispatch.queue_depth,
                    runtime_extension_max_queue_depth = extension_dispatch.max_queue_depth,
                    runtime_extension_max_avg_dispatch_lag_us =
                        extension_dispatch.max_avg_dispatch_lag_us,
                    runtime_extension_max_dispatch_lag_us =
                        extension_dispatch.max_dispatch_lag_us,
                    derived_state_enabled = derived_state_hooks_enabled,
                    derived_state_checkpoint_interval_ms = derived_state_checkpoint_interval_ms,
                    derived_state_recovery_interval_ms = derived_state_recovery_interval_ms,
                    derived_state_healthy_consumers = derived_state_healthy_consumers,
                    derived_state_unhealthy_consumers = derived_state_unhealthy_consumers,
                    derived_state_pending_recovery = derived_state_pending_recovery,
                    derived_state_rebuild_required = derived_state_rebuild_required,
                    derived_state_fault_total = derived_state_fault_total,
                    derived_state_last_sequence = derived_state_last_sequence,
                    derived_state_replay_enabled = derived_state_replay_telemetry.enabled,
                    derived_state_replay_backend = %derived_state_replay_telemetry.backend,
                    derived_state_replay_retained_sessions =
                        derived_state_replay_telemetry.retained_sessions,
                    derived_state_replay_retained_envelopes =
                        derived_state_replay_telemetry.retained_envelopes,
                    derived_state_replay_truncated_envelopes =
                        derived_state_replay_telemetry.truncated_envelopes,
                    derived_state_replay_append_failures =
                        derived_state_replay_telemetry.append_failures,
                    derived_state_replay_load_failures =
                        derived_state_replay_telemetry.load_failures,
                    derived_state_replay_compactions =
                        derived_state_replay_telemetry.compactions,
                    derived_state_pending_recovery_consumers =
                        ?derived_state_pending_recovery_names,
                    derived_state_rebuild_consumers = ?derived_state_rebuild_names,
                    derived_state_consumers = ?derived_state_consumer_telemetry,
                    dataset_decode_failures = dataset_decode_fail_count.load(Ordering::Relaxed),
                    dataset_tail_skips = dataset_tail_skip_count.load(Ordering::Relaxed),
                    dataset_duplicate_drops = dataset_duplicate_drop_count.load(Ordering::Relaxed),
                    dataset_queue_capacity_per_worker = dataset_queue_capacity,
                    dataset_queue_capacity_total = dataset_queue_capacity_total,
                    dataset_worker_count = dataset_worker_count,
                    dataset_queue_drops = dataset_queue_drops,
                    dataset_queue_depth = dataset_queue_depth,
                    dataset_jobs_enqueued,
                    dataset_jobs_started,
                    dataset_jobs_completed,
                    dataset_jobs_pending,
                    packet_worker_queue_depth = runtime_stage_metrics.packet_worker_queue_depth,
                    packet_worker_max_queue_depth =
                        runtime_stage_metrics.packet_worker_max_queue_depth,
                    packet_worker_queue_depth_local = packet_worker_pool.queue_depth(),
                    packet_worker_max_queue_depth_local = packet_worker_pool.max_queue_depth(),
                    packet_worker_queue_overflow_policy =
                        PACKET_WORKER_QUEUE_OVERFLOW_POLICY,
                    packet_worker_queue_depths = ?packet_worker_pool.worker_queue_depths(),
                    packet_worker_dropped_batches =
                        runtime_stage_metrics.packet_worker_dropped_batches_total,
                    packet_worker_dropped_packets =
                        runtime_stage_metrics.packet_worker_dropped_packets_total,
                    dataset_slots_tracked = dataset_reassembler.tracked_slots(),
                    dataset_max_tracked_slots = dataset_max_tracked_slots,
                    fec_sets_tracked = packet_worker_pool.tracked_fec_sets(),
                    fec_sets_tracked_by_worker = ?packet_worker_pool.tracked_fec_sets_by_worker(),
                    fec_max_tracked_sets = fec_max_tracked_sets,
                    vote_only = vote_only_count,
                    mixed = mixed_count,
                    non_vote = non_vote_count,
                    verify_verified = verify_verified_count,
                    verify_unknown_leader = verify_unknown_leader_count,
                    verify_invalid_merkle = verify_invalid_merkle_count,
                    verify_invalid_signature = verify_invalid_signature_count,
                    verify_malformed = verify_malformed_count,
                    verify_dropped = verify_dropped_count,
                    verify_recovered_enabled = verify_recovered_shreds,
                    repair_requests_total = repair_requests_total,
                    repair_requests_enqueued = repair_requests_enqueued,
                    repair_requests_sent = repair_requests_sent,
                    repair_requests_no_peer = repair_requests_no_peer,
                    repair_request_errors = repair_request_errors,
                    repair_request_queue_drops = repair_request_queue_drops,
                    repair_requests_port_8899 = repair_requests_port_8899,
                    repair_requests_port_8900 = repair_requests_port_8900,
                    repair_requests_port_other = repair_requests_port_other,
                    repair_requests_window_index = repair_requests_window_index,
                    repair_requests_highest_window_index = repair_requests_highest_window_index,
                    repair_requests_skipped_outstanding = repair_requests_skipped_outstanding,
                    repair_outstanding_entries = outstanding_repairs
                        .as_ref()
                        .map_or(0, OutstandingRepairRequests::len),
                    repair_outstanding_purged = repair_outstanding_purged,
                    repair_outstanding_cleared_on_receive = repair_outstanding_cleared_on_receive,
                    repair_outstanding_timeout_ms = repair_outstanding_timeout_ms,
                    repair_tip_stall_ms = repair_tip_stall_ms,
                    repair_dataset_stall_ms = repair_dataset_stall_ms,
                    repair_stall_sustain_ms = repair_stall_sustain_ms,
                    repair_tip_probe_ahead_slots = repair_tip_probe_ahead_slots,
                    repair_min_slot_lag = repair_min_slot_lag,
                    repair_min_slot_lag_stalled = repair_min_slot_lag_stalled,
                    repair_dynamic_stalled = repair_dynamic_stalled,
                    repair_dynamic_dataset_stalled = repair_dynamic_dataset_stalled,
                    repair_dynamic_stream_progress = repair_dynamic_stream_progress,
                    repair_dynamic_stream_healthy = repair_dynamic_stream_healthy,
                    repair_dynamic_min_slot_lag = repair_dynamic_min_slot_lag,
                    repair_per_slot_cap = repair_per_slot_cap,
                    repair_per_slot_cap_stalled = repair_per_slot_cap_stalled,
                    repair_dynamic_per_slot_cap = repair_dynamic_per_slot_cap,
                    repair_max_requests_per_tick = repair_max_requests_per_tick,
                    repair_max_requests_per_tick_stalled = repair_max_requests_per_tick_stalled,
                    repair_dynamic_max_requests_per_tick = repair_dynamic_max_requests_per_tick,
                    repair_max_highest_per_tick = repair_max_highest_per_tick,
                    repair_max_highest_per_tick_stalled = repair_max_highest_per_tick_stalled,
                    repair_dynamic_max_highest_per_tick = repair_dynamic_max_highest_per_tick,
                    repair_max_forward_probe_per_tick = repair_max_forward_probe_per_tick,
                    repair_max_forward_probe_per_tick_stalled = repair_max_forward_probe_per_tick_stalled,
                    repair_dynamic_max_forward_probe_per_tick = repair_dynamic_max_forward_probe_per_tick,
                    repair_seed_slot = 0,
                    repair_seed_slots = 0,
                    repair_seed_failures = 0,
                    repair_response_pings = repair_response_pings,
                    repair_response_ping_errors = repair_response_ping_errors,
                    repair_ping_queue_drops = repair_ping_queue_drops,
                    repair_serve_requests_enqueued = repair_serve_requests_enqueued,
                    repair_serve_requests_handled = repair_serve_requests_handled,
                    repair_serve_responses_sent = repair_serve_responses_sent,
                    repair_serve_cache_misses = repair_serve_cache_misses,
                    repair_serve_rate_limited = repair_serve_rate_limited,
                    repair_serve_rate_limited_peer = repair_serve_rate_limited_peer,
                    repair_serve_rate_limited_bytes = repair_serve_rate_limited_bytes,
                    repair_serve_errors = repair_serve_errors,
                    repair_serve_queue_drops = repair_serve_queue_drops,
                    repair_source_hint_enqueued = repair_source_hint_enqueued,
                    repair_source_hint_drops = repair_source_hint_drops,
                    repair_source_hint_buffer_drops = repair_source_hint_buffer_drops,
                    gossip_active_entrypoint = gossip_active_entrypoint,
                    gossip_runtime_switch_enabled = gossip_switch_enabled,
                    gossip_runtime_switch_stall_ms = gossip_switch_stall_ms,
                    gossip_runtime_switch_dataset_stall_ms = gossip_switch_dataset_stall_ms,
                    gossip_runtime_switch_warmup_ms = gossip_switch_warmup_ms,
                    gossip_runtime_switch_overlap_ms = gossip_switch_overlap_ms,
                    gossip_runtime_switch_sustain_ms = gossip_switch_sustain_ms,
                    gossip_runtime_switch_attempts = gossip_switch_attempts,
                    gossip_runtime_switch_successes = gossip_switch_successes,
                    gossip_runtime_switch_failures = gossip_switch_fails,
                    repair_peer_total = repair_peer_total,
                    repair_peer_active = repair_peer_active,
                    latest_shred_slot = latest_shred_slot.unwrap_or_default(),
                    fork_window_slots = fork_window_slots,
                    fork_slots_tracked = u64::try_from(fork_snapshot.tracked_slots).unwrap_or(u64::MAX),
                    fork_tip_slot = fork_snapshot.tip_slot.unwrap_or_default(),
                    fork_confirmed_slot = fork_snapshot.confirmed_slot.unwrap_or_default(),
                    fork_finalized_slot = fork_snapshot.finalized_slot.unwrap_or_default(),
                    fork_status_transitions = fork_status_transitions_total,
                    fork_reorgs = fork_reorg_count,
                    fork_orphaned_slots = fork_orphaned_slots_total,
                    tx_confirmed_slot =
                        tx_commitment_tracker.snapshot().confirmed_slot.unwrap_or_default(),
                    tx_finalized_slot =
                        tx_commitment_tracker.snapshot().finalized_slot.unwrap_or_default(),
                    latest_shred_age_ms = duration_to_ms_u64(
                        Instant::now().saturating_duration_since(latest_shred_updated_at)
                    ),
                    latest_dataset_age_ms = duration_to_ms_u64(
                        Instant::now().saturating_duration_since(last_dataset_reconstructed_at)
                    ),
                    gossip_runtime_age_ms = gossip_runtime_age_ms,
                    gossip_runtime_stall_age_ms = gossip_runtime_stall_age_ms,
                    window_slots = coverage.slots_tracked,
                    window_slots_with_tx = coverage.slots_with_tx,
                    window_tx_total = coverage.tx_total,
                    window_dataset_total = coverage.dataset_total,
                    window_data_shreds = coverage.data_shreds,
                    window_code_shreds = coverage.code_shreds,
                    window_recovered_data = coverage.recovered_data_shreds,
                        "ingest telemetry"
                    );
                }
                if derived_state_hooks_enabled && !derived_state_unhealthy_names.is_empty() {
                    tracing::warn!(
                        unhealthy_consumers = ?derived_state_unhealthy_names,
                        consumer_telemetry = ?derived_state_consumer_telemetry,
                        derived_state_fault_total = derived_state_fault_total,
                        derived_state_last_sequence = derived_state_last_sequence,
                        "derived-state consumers lost live continuity"
                    );
                }
                if extension_hooks_enabled {
                    let dropped_delta = extension_dispatch
                        .dropped_events
                        .saturating_sub(extension_last_dropped_events);
                    extension_last_dropped_events = extension_dispatch.dropped_events;
                    let queue_depth_over_limit = extension_queue_depth_warn > 0
                        && extension_dispatch.queue_depth >= extension_queue_depth_warn;
                    let lag_over_limit = extension_dispatch_lag_warn_us > 0
                        && extension_dispatch.max_dispatch_lag_us
                            >= extension_dispatch_lag_warn_us;
                    let dropped_delta_over_limit = extension_drop_warn_delta > 0
                        && dropped_delta >= extension_drop_warn_delta;
                    if queue_depth_over_limit || lag_over_limit || dropped_delta_over_limit {
                        tracing::warn!(
                            runtime_extension_active = extension_dispatch.active_extensions,
                            runtime_extension_dispatched = extension_dispatch.dispatched_events,
                            runtime_extension_dropped = extension_dispatch.dropped_events,
                            runtime_extension_dropped_delta = dropped_delta,
                            runtime_extension_queue_depth = extension_dispatch.queue_depth,
                            runtime_extension_max_queue_depth = extension_dispatch.max_queue_depth,
                            runtime_extension_max_avg_dispatch_lag_us =
                                extension_dispatch.max_avg_dispatch_lag_us,
                            runtime_extension_max_dispatch_lag_us =
                                extension_dispatch.max_dispatch_lag_us,
                            queue_depth_warn = extension_queue_depth_warn,
                            dispatch_lag_warn_us = extension_dispatch_lag_warn_us,
                            drop_warn_delta = extension_drop_warn_delta,
                            "runtime extension dispatch pressure detected"
                        );
                    }
                }
            }
            maybe_event = runtime.tx_event_rx.recv(), if !tx_events_closed => {
                let Some(event) = maybe_event else {
                    tx_events_closed = true;
                    if packet_workers_closed {
                        break 'event_loop;
                    }
                    continue;
                };
                match event {
                    TxObservedEvent::Detailed {
                        slot,
                        signature,
                        kind,
                        commitment_status,
                    } => {
                        match kind {
                            TxKind::VoteOnly => {
                                vote_only_count = vote_only_count.saturating_add(1);
                            }
                            TxKind::Mixed => {
                                mixed_count = mixed_count.saturating_add(1);
                            }
                            TxKind::NonVote => {
                                non_vote_count = non_vote_count.saturating_add(1);
                            }
                        }
                        coverage_window.on_tx(slot);
                        last_dataset_reconstructed_at = Instant::now();
                        if log_all_txs || (log_non_vote_txs && !matches!(kind, TxKind::VoteOnly)) {
                            tracing::info!(
                                slot,
                                signature = %signature,
                                kind = ?kind,
                                commitment_status = ?commitment_status,
                                "tx observed"
                            );
                        }
                    }
                    TxObservedEvent::Summary {
                        slot,
                        vote_only,
                        mixed,
                        non_vote,
                    } => {
                        vote_only_count = vote_only_count.saturating_add(vote_only);
                        mixed_count = mixed_count.saturating_add(mixed);
                        non_vote_count = non_vote_count.saturating_add(non_vote);
                        coverage_window.on_tx_count(
                            slot,
                            vote_only.saturating_add(mixed).saturating_add(non_vote),
                        );
                        last_dataset_reconstructed_at = Instant::now();
                    }
                }
            }
        }
    }
    packet_worker_pool.shutdown().await;
    dataset_worker_pool.shutdown().await;
    if derived_state_hooks_enabled {
        emit_shutdown_checkpoint_barrier(&derived_state_host, fork_tracker.snapshot());
    }
    if extension_hooks_enabled {
        extension_host.shutdown().await;
    }
    if plugin_hooks_enabled {
        plugin_host.shutdown().await;
    }
    #[cfg(feature = "gossip-bootstrap")]
    runtime.stop_gossip_runtime().await;
    if let Some(observability_handle) = observability_handle.as_ref() {
        observability_handle.mark_not_ready();
    }
    drop(runtime);
    #[cfg(feature = "kernel-bypass")]
    if let Some(drain_task) = kernel_bypass_internal_ingest_drain_task.take()
        && drain_task.await.is_err()
    {
        // Internal drain task was already cancelled by runtime teardown.
    }
    Ok(())
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
struct ExtensionDispatchTelemetrySnapshot {
    active_extensions: u64,
    dispatched_events: u64,
    dropped_events: u64,
    queue_depth: u64,
    max_queue_depth: u64,
    max_avg_dispatch_lag_us: u64,
    max_dispatch_lag_us: u64,
}

fn collect_extension_dispatch_telemetry(
    metrics: Vec<RuntimeExtensionDispatchMetrics>,
) -> ExtensionDispatchTelemetrySnapshot {
    let active_extensions = u64::try_from(metrics.len()).unwrap_or(u64::MAX);
    let mut snapshot = ExtensionDispatchTelemetrySnapshot {
        active_extensions,
        ..ExtensionDispatchTelemetrySnapshot::default()
    };
    for metric in metrics {
        snapshot.dispatched_events = snapshot
            .dispatched_events
            .saturating_add(metric.dispatched_events);
        snapshot.dropped_events = snapshot
            .dropped_events
            .saturating_add(metric.dropped_events);
        snapshot.queue_depth = snapshot.queue_depth.saturating_add(metric.queue_depth);
        snapshot.max_queue_depth = snapshot.max_queue_depth.max(metric.max_queue_depth);
        snapshot.max_avg_dispatch_lag_us = snapshot
            .max_avg_dispatch_lag_us
            .max(metric.avg_dispatch_lag_us);
        snapshot.max_dispatch_lag_us = snapshot.max_dispatch_lag_us.max(metric.max_dispatch_lag_us);
    }
    snapshot
}

#[derive(Default)]
struct PacketWorkerResultSummary {
    recovered_data_packets: u64,
    completed_dataset_count: u64,
    completed_datasets_from_recovered: u64,
}

struct PacketWorkerResultContext<'context> {
    tx_commitment_tracker: &'context CommitmentSlotTracker,
    plugin_host: &'context PluginHost,
    derived_state_host: &'context DerivedStateHost,
    plugin_hooks_enabled: bool,
    derived_state_hooks_enabled: bool,
    latest_shred_slot: &'context mut Option<u64>,
    latest_shred_updated_at: &'context mut Instant,
    fork_tracker: &'context mut ForkTracker,
    coverage_window: &'context mut SlotCoverageWindow,
    missing_tracker: &'context mut Option<MissingShredTracker>,
    outstanding_repairs: &'context mut Option<OutstandingRepairRequests>,
    repair_outstanding_cleared_on_receive: &'context mut u64,
    shred_dedupe_cache: &'context mut Option<ShredDedupeCache>,
    dedupe_canonical_duplicate_drop_count: &'context mut u64,
    dedupe_canonical_conflict_drop_count: &'context mut u64,
    dataset_reassembler: &'context mut DataSetReassembler,
    dataset_retained_slot_lag: u64,
    dataset_worker_queues: &'context [DatasetDispatchQueue],
    dataset_jobs_enqueued_count: &'context AtomicU64,
    dataset_queue_drop_count: &'context AtomicU64,
    last_dataset_reconstructed_at: &'context mut Instant,
    data_count: &'context mut u64,
    code_count: &'context mut u64,
    recovered_data_count: &'context mut u64,
    data_complete_count: &'context mut u64,
    last_in_slot_count: &'context mut u64,
    source_port_8899_data: &'context mut u64,
    source_port_8900_data: &'context mut u64,
    source_port_other_data: &'context mut u64,
    source_port_8899_code: &'context mut u64,
    source_port_8900_code: &'context mut u64,
    source_port_other_code: &'context mut u64,
    fork_status_transitions_total: &'context mut u64,
    fork_reorg_count: &'context mut u64,
    fork_orphaned_slots_total: &'context mut u64,
    #[cfg(feature = "gossip-bootstrap")]
    emitted_slot_leaders: &'context mut HashMap<u64, [u8; 32]>,
    #[cfg(feature = "gossip-bootstrap")]
    verify_slot_leader_window: u64,
    #[cfg(feature = "gossip-bootstrap")]
    repair_driver_enabled: bool,
    #[cfg(feature = "gossip-bootstrap")]
    repair_source_hints: &'context mut RepairSourceHintBuffer,
    #[cfg(feature = "gossip-bootstrap")]
    repair_command_tx: Option<&'context mpsc::Sender<RepairCommand>>,
    #[cfg(feature = "gossip-bootstrap")]
    repair_source_hint_batch_size: usize,
    #[cfg(feature = "gossip-bootstrap")]
    repair_source_hint_flush_interval: Duration,
    #[cfg(feature = "gossip-bootstrap")]
    repair_source_hint_drops: &'context mut u64,
    #[cfg(feature = "gossip-bootstrap")]
    repair_source_hint_enqueued: &'context mut u64,
    #[cfg(feature = "gossip-bootstrap")]
    repair_source_hint_buffer_drops: &'context mut u64,
    #[cfg(feature = "gossip-bootstrap")]
    repair_source_hint_last_flush: &'context mut Instant,
}

impl PacketWorkerResultContext<'_> {
    fn process(
        mut self,
        worker_result: PacketWorkerBatchResult,
        observed_at: Instant,
        packet_batch_dispatch_scratch: &mut PacketBatchDispatchScratch,
    ) -> PacketWorkerResultSummary {
        let PacketWorkerBatchResult {
            worker_index,
            reusable_packets,
            accepted_shreds,
            #[cfg(feature = "gossip-bootstrap")]
            leader_diff,
            #[cfg(feature = "gossip-bootstrap")]
            observed_slot_leaders,
            verify_verified_count: _,
            verify_unknown_leader_count: _,
            verify_invalid_merkle_count: _,
            verify_invalid_signature_count: _,
            verify_malformed_count: _,
            verify_dropped_count: _,
        } = worker_result;
        #[cfg(feature = "gossip-bootstrap")]
        self.emit_leader_events(leader_diff, observed_slot_leaders);

        let mut summary = PacketWorkerResultSummary::default();
        for shred in accepted_shreds {
            self.process_accepted_shred(shred, observed_at, &mut summary);
        }
        packet_batch_dispatch_scratch.recycle_worker_batch(worker_index, reusable_packets);
        summary
    }

    fn process_accepted_shred(
        &mut self,
        shred: WorkerAcceptedShred,
        observed_at: Instant,
        summary: &mut PacketWorkerResultSummary,
    ) {
        if let Some(cache) = self.shred_dedupe_cache.as_mut() {
            match cache.observe_signature(
                ShredDedupeIdentity::new(
                    shred.slot,
                    shred.index,
                    shred.fec_set_index,
                    shred.version,
                    shred.variant,
                ),
                shred.signature,
                observed_at,
                ShredDedupeStage::Canonical,
            ) {
                ShredDedupeObservation::Accepted => {}
                ShredDedupeObservation::Duplicate => {
                    *self.dedupe_canonical_duplicate_drop_count =
                        self.dedupe_canonical_duplicate_drop_count.saturating_add(1);
                    crate::runtime_metrics::observe_shred_dedupe_drops(
                        ShredDedupeStage::Canonical,
                        1,
                        0,
                    );
                    return;
                }
                ShredDedupeObservation::Conflict => {
                    *self.dedupe_canonical_conflict_drop_count =
                        self.dedupe_canonical_conflict_drop_count.saturating_add(1);
                    crate::runtime_metrics::observe_shred_dedupe_drops(
                        ShredDedupeStage::Canonical,
                        0,
                        1,
                    );
                    if *self.dedupe_canonical_conflict_drop_count <= INITIAL_DEBUG_SAMPLE_LOG_LIMIT
                    {
                        tracing::warn!(
                            slot = shred.slot,
                            index = shred.index,
                            fec_set_index = shred.fec_set_index,
                            "dropping conflicting duplicate accepted shred"
                        );
                    }
                    return;
                }
            }
        }
        match shred.kind {
            WorkerAcceptedShredKind::Data {
                parent_slot,
                data_complete,
                last_in_slot,
                reference_tick,
            } => {
                let source_addr = source_addr_or_unspecified(shred.source);
                self.process_data_like_shred(
                    shred.slot,
                    shred.index,
                    shred.fec_set_index,
                    parent_slot,
                    data_complete,
                    last_in_slot,
                    reference_tick,
                    shred.payload_fragment,
                    observed_at,
                    Some(source_addr),
                    false,
                    summary,
                );
            }
            WorkerAcceptedShredKind::Code { num_data_shreds } => {
                self.process_code_shred(
                    shred.slot,
                    shred.index,
                    shred.fec_set_index,
                    num_data_shreds,
                    source_addr_or_unspecified(shred.source),
                    observed_at,
                );
            }
            WorkerAcceptedShredKind::RecoveredData {
                parent_slot,
                data_complete,
                last_in_slot,
                reference_tick,
            } => {
                self.process_data_like_shred(
                    shred.slot,
                    shred.index,
                    shred.fec_set_index,
                    parent_slot,
                    data_complete,
                    last_in_slot,
                    reference_tick,
                    shred.payload_fragment,
                    observed_at,
                    None,
                    true,
                    summary,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn process_data_like_shred(
        &mut self,
        slot: u64,
        index: u32,
        fec_set_index: u32,
        parent_slot: Option<u64>,
        data_complete: bool,
        last_in_slot: bool,
        reference_tick: u8,
        payload_fragment: Option<crate::reassembly::dataset::SharedPayloadFragment>,
        observed_at: Instant,
        source_addr: Option<SocketAddr>,
        recovered: bool,
        summary: &mut PacketWorkerResultSummary,
    ) {
        if recovered {
            summary.recovered_data_packets = summary.recovered_data_packets.saturating_add(1);
            *self.recovered_data_count = self.recovered_data_count.saturating_add(1);
            self.coverage_window.on_recovered_data_shred(slot);
        } else if let Some(source_addr) = source_addr {
            *self.data_count = self.data_count.saturating_add(1);
            observe_source_port(
                source_addr,
                self.source_port_8899_data,
                self.source_port_8900_data,
                self.source_port_other_data,
            );
            self.coverage_window.on_data_shred(slot);
            #[cfg(feature = "gossip-bootstrap")]
            self.record_repair_source_hint(source_addr, observed_at);
        }

        if data_complete {
            *self.data_complete_count = self.data_complete_count.saturating_add(1);
        }
        if last_in_slot {
            *self.last_in_slot_count = self.last_in_slot_count.saturating_add(1);
        }

        self.note_slot(slot, observed_at);
        let fork_update = if recovered {
            self.fork_tracker
                .observe_recovered_data_shred(slot, parent_slot)
        } else {
            self.fork_tracker.observe_data_shred(slot, parent_slot)
        };
        self.apply_fork_update(&fork_update);
        self.clear_outstanding_repair(slot, index);

        if let Some(tracker) = self.missing_tracker.as_mut() {
            if recovered {
                tracker.on_recovered_data_shred(
                    slot,
                    index,
                    fec_set_index,
                    last_in_slot,
                    reference_tick,
                    observed_at,
                );
            } else {
                tracker.on_data_shred(
                    slot,
                    index,
                    fec_set_index,
                    last_in_slot,
                    reference_tick,
                    observed_at,
                );
            }
        }

        if let Some(payload_fragment) = payload_fragment {
            if let Some(slot_floor) = self.dataset_slot_floor() {
                self.dataset_reassembler.purge_older_than(slot_floor);
                if slot < slot_floor {
                    return;
                }
            }
            let completed_datasets = self.dataset_reassembler.ingest_data_shred_meta(
                slot,
                index,
                data_complete,
                last_in_slot,
                payload_fragment,
            );
            let completed_count = self.dispatch_completed_datasets(completed_datasets, observed_at);
            summary.completed_dataset_count = summary
                .completed_dataset_count
                .saturating_add(completed_count);
            if recovered {
                summary.completed_datasets_from_recovered = summary
                    .completed_datasets_from_recovered
                    .saturating_add(completed_count);
            }
        }
    }

    fn process_code_shred(
        &mut self,
        slot: u64,
        index: u32,
        fec_set_index: u32,
        num_data_shreds: u16,
        source_addr: SocketAddr,
        observed_at: Instant,
    ) {
        *self.code_count = self.code_count.saturating_add(1);
        observe_source_port(
            source_addr,
            self.source_port_8899_code,
            self.source_port_8900_code,
            self.source_port_other_code,
        );
        self.note_slot(slot, observed_at);
        let fork_update = self.fork_tracker.observe_code_shred(slot);
        self.apply_fork_update(&fork_update);
        self.coverage_window.on_code_shred(slot);
        self.clear_outstanding_repair(slot, index);
        if let Some(tracker) = self.missing_tracker.as_mut() {
            tracker.on_code_shred(slot, fec_set_index, num_data_shreds, observed_at);
        }
        #[cfg(feature = "gossip-bootstrap")]
        self.record_repair_source_hint(source_addr, observed_at);
    }

    fn dispatch_completed_datasets(
        &mut self,
        completed_datasets: Vec<CompletedDataSet>,
        observed_at: Instant,
    ) -> u64 {
        let slot_floor = self.dataset_slot_floor();
        let mut completed_count = 0_u64;
        let mut dispatchable = Vec::with_capacity(completed_datasets.len());
        for dataset in completed_datasets {
            if slot_floor.is_some_and(|floor| dataset.slot < floor) {
                continue;
            }
            completed_count = completed_count.saturating_add(1);
            self.coverage_window.on_dataset_completed(dataset.slot);
            let substantial_dataset =
                dataset.payload_fragments.len() >= SUBSTANTIAL_DATASET_MIN_SHREDS;
            dispatchable.push(dataset);
            if substantial_dataset {
                *self.last_dataset_reconstructed_at = observed_at;
            }
        }
        dispatch_completed_dataset(
            self.dataset_worker_queues,
            dispatchable,
            self.dataset_jobs_enqueued_count,
            self.dataset_queue_drop_count,
        );
        completed_count
    }

    fn dataset_slot_floor(&self) -> Option<u64> {
        (*self.latest_shred_slot).map(|slot| slot.saturating_sub(self.dataset_retained_slot_lag))
    }

    const fn note_slot(&mut self, slot: u64, observed_at: Instant) {
        note_latest_shred_slot(
            self.latest_shred_slot,
            self.latest_shred_updated_at,
            slot,
            observed_at,
        );
    }

    fn clear_outstanding_repair(&mut self, slot: u64, index: u32) {
        if let Some(outstanding_repairs) = self.outstanding_repairs.as_mut() {
            let cleared = outstanding_repairs.on_shred_received(slot, index);
            *self.repair_outstanding_cleared_on_receive = self
                .repair_outstanding_cleared_on_receive
                .saturating_add(u64::try_from(cleared).unwrap_or(u64::MAX));
        }
    }

    fn apply_fork_update(&mut self, update: &ForkTrackerUpdate) {
        apply_fork_update(
            update,
            &mut ForkUpdateDispatchContext {
                tx_commitment_tracker: self.tx_commitment_tracker,
                plugin_host: self.plugin_host,
                derived_state_host: self.derived_state_host,
                plugin_hooks_enabled: self.plugin_hooks_enabled,
                derived_state_hooks_enabled: self.derived_state_hooks_enabled,
                fork_status_transitions_total: self.fork_status_transitions_total,
                fork_reorg_count: self.fork_reorg_count,
                fork_orphaned_slots_total: self.fork_orphaned_slots_total,
            },
        );
    }

    #[cfg(feature = "gossip-bootstrap")]
    fn emit_leader_events(
        &mut self,
        leader_diff: crate::verify::SlotLeaderDiff,
        observed_slot_leaders: Vec<(u64, [u8; 32])>,
    ) {
        if !self.plugin_hooks_enabled {
            return;
        }
        emit_slot_leader_diff_event(
            self.plugin_host,
            self.derived_state_host,
            leader_diff,
            *self.latest_shred_slot,
            self.emitted_slot_leaders,
        );
        for (slot, leader_bytes) in observed_slot_leaders {
            emit_observed_slot_leader_bytes_event(
                self.plugin_host,
                self.derived_state_host,
                slot,
                leader_bytes,
                self.emitted_slot_leaders,
                self.verify_slot_leader_window,
            );
        }
    }

    #[cfg(feature = "gossip-bootstrap")]
    fn record_repair_source_hint(&mut self, source_addr: SocketAddr, observed_at: Instant) {
        if !self.repair_driver_enabled {
            return;
        }
        if self.repair_source_hints.record(source_addr).is_err() {
            *self.repair_source_hint_buffer_drops =
                self.repair_source_hint_buffer_drops.saturating_add(1);
        }
        let should_flush = self.repair_source_hints.len() >= self.repair_source_hint_batch_size
            || observed_at.saturating_duration_since(*self.repair_source_hint_last_flush)
                >= self.repair_source_hint_flush_interval;
        if should_flush {
            *self.repair_source_hint_last_flush = observed_at;
            flush_repair_source_hints(
                self.repair_source_hints,
                self.repair_command_tx,
                self.repair_source_hint_batch_size,
                self.repair_source_hint_drops,
                self.repair_source_hint_enqueued,
            );
        }
    }
}

const fn parsed_shred_slot(parsed_shred: &ParsedShredHeader) -> u64 {
    match parsed_shred {
        ParsedShredHeader::Data(data) => data.common.slot,
        ParsedShredHeader::Code(code) => code.common.slot,
    }
}

const fn parsed_shred_index(parsed_shred: &ParsedShredHeader) -> u32 {
    match parsed_shred {
        ParsedShredHeader::Data(data) => data.common.index,
        ParsedShredHeader::Code(code) => code.common.index,
    }
}

const fn parsed_shred_fec_set_index(parsed_shred: &ParsedShredHeader) -> u32 {
    match parsed_shred {
        ParsedShredHeader::Data(data) => data.common.fec_set_index,
        ParsedShredHeader::Code(code) => code.common.fec_set_index,
    }
}

struct PacketWorkerAssignments {
    owners: HashMap<(u64, u32), usize>,
    retained_slot_window: u64,
    last_pruned_floor: u64,
}

impl PacketWorkerAssignments {
    fn new(retained_slot_window: usize) -> Self {
        Self {
            owners: HashMap::new(),
            retained_slot_window: u64::try_from(retained_slot_window).unwrap_or(u64::MAX),
            last_pruned_floor: 0,
        }
    }

    fn worker_for(&mut self, slot: u64, fec_set_index: u32, worker_loads: &[u64]) -> usize {
        let worker_count = worker_loads.len().max(1);
        self.prune(slot);
        if let Some(&worker_index) = self.owners.get(&(slot, fec_set_index)) {
            return worker_index.min(worker_count.saturating_sub(1));
        }

        let preferred_worker = shard_fec_set_to_worker(slot, fec_set_index, worker_count)
            .min(worker_count.saturating_sub(1));
        let worker_index = select_least_loaded_worker(worker_loads, preferred_worker);
        self.owners.insert((slot, fec_set_index), worker_index);
        worker_index
    }

    fn prune(&mut self, latest_slot: u64) {
        let floor = latest_slot.saturating_sub(self.retained_slot_window);
        if floor <= self.last_pruned_floor {
            return;
        }
        self.owners.retain(|(slot, _), _| *slot >= floor);
        self.last_pruned_floor = floor;
    }
}

/// Reusable coordinator scratch buffers for routing one ingress packet batch to worker queues.
struct PacketBatchDispatchScratch {
    /// Snapshot of current worker queue depths.
    worker_queue_depths: Vec<u64>,
    /// Snapshot of currently tracked FEC sets per worker.
    tracked_fec_sets: Vec<u64>,
    /// Combined worker pressure scores used for worker selection.
    worker_loads: Vec<u64>,
    /// Outbound packet batches staged per worker.
    worker_batches: Vec<Vec<PacketWorkerInput>>,
}

impl PacketBatchDispatchScratch {
    /// Creates scratch storage sized for the current worker count.
    fn new(worker_count: usize) -> Self {
        let worker_count = worker_count.max(1);
        let mut worker_batches = Vec::with_capacity(worker_count);
        worker_batches.resize_with(worker_count, Vec::new);
        Self {
            worker_queue_depths: Vec::with_capacity(worker_count),
            tracked_fec_sets: Vec::with_capacity(worker_count),
            worker_loads: Vec::with_capacity(worker_count),
            worker_batches,
        }
    }

    /// Refreshes queue/fec pressure snapshots and clears staged worker batches.
    fn refresh(&mut self, packet_worker_pool: &PacketWorkerPool) {
        packet_worker_pool.fill_worker_queue_depths(&mut self.worker_queue_depths);
        packet_worker_pool.fill_tracked_fec_sets_by_worker(&mut self.tracked_fec_sets);
        combine_packet_worker_pressure_into(
            &self.worker_queue_depths,
            &self.tracked_fec_sets,
            &mut self.worker_loads,
        );
        let worker_count = packet_worker_pool.worker_count();
        if self.worker_batches.len() != worker_count {
            self.worker_batches.clear();
            self.worker_batches.resize_with(worker_count, Vec::new);
        }
        for batch in &mut self.worker_batches {
            batch.clear();
        }
    }

    /// Returns the current combined worker pressure view.
    fn worker_loads(&self) -> &[u64] {
        &self.worker_loads
    }

    /// Returns one mutable staged batch for `worker_index`.
    fn worker_batch_mut(&mut self, worker_index: usize) -> Option<&mut Vec<PacketWorkerInput>> {
        self.worker_batches.get_mut(worker_index)
    }

    /// Increments the pressure score for one worker after staging another packet there.
    fn bump_worker_load(&mut self, worker_index: usize) {
        if let Some(load) = self.worker_loads.get_mut(worker_index) {
            *load = load.saturating_add(1);
        }
    }

    /// Moves one staged worker batch out so the worker can process it.
    fn take_worker_batch(&mut self, worker_index: usize) -> Vec<PacketWorkerInput> {
        let Some(batch) = self.worker_batches.get_mut(worker_index) else {
            return Vec::new();
        };
        std::mem::take(batch)
    }

    /// Recycles one drained worker batch back into retained scratch storage.
    fn recycle_worker_batch(&mut self, worker_index: usize, mut batch: Vec<PacketWorkerInput>) {
        let Some(slot) = self.worker_batches.get_mut(worker_index) else {
            return;
        };
        batch.clear();
        *slot = batch;
    }

    /// Returns number of staged worker batches.
    const fn worker_count(&self) -> usize {
        self.worker_batches.len()
    }
}

fn shard_fec_set_to_worker(slot: u64, fec_set_index: u32, worker_count: usize) -> usize {
    let worker_count = worker_count.max(1);
    let slot_mix = slot.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let fec_mix = u64::from(fec_set_index).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    usize::try_from(slot_mix ^ fec_mix)
        .unwrap_or(usize::MAX)
        .checked_rem(worker_count)
        .unwrap_or(0)
}

fn select_least_loaded_worker(worker_loads: &[u64], preferred_worker: usize) -> usize {
    if worker_loads.is_empty() {
        return 0;
    }
    let preferred_worker = preferred_worker.min(worker_loads.len().saturating_sub(1));
    let preferred_load = worker_loads
        .get(preferred_worker)
        .copied()
        .unwrap_or(u64::MAX);
    let mut best_worker = preferred_worker;
    let mut best_load = preferred_load;
    for (worker_index, &load) in worker_loads.iter().enumerate() {
        if load < best_load {
            best_worker = worker_index;
            best_load = load;
        } else if load == best_load
            && worker_index == preferred_worker
            && best_worker != preferred_worker
        {
            best_worker = worker_index;
        }
    }
    best_worker
}

/// Recomputes combined worker pressure into caller-owned scratch storage.
fn combine_packet_worker_pressure_into(
    worker_queue_depths: &[u64],
    tracked_fec_sets: &[u64],
    out: &mut Vec<u64>,
) {
    let worker_count = worker_queue_depths.len().max(tracked_fec_sets.len());
    out.clear();
    out.reserve(worker_count.saturating_sub(out.capacity()));
    for worker_index in 0..worker_count {
        let queue_depth = worker_queue_depths.get(worker_index).copied().unwrap_or(0);
        let fec_pressure = tracked_fec_sets
            .get(worker_index)
            .copied()
            .unwrap_or(0)
            .checked_div(PACKET_WORKER_FEC_PRESSURE_DIVISOR)
            .unwrap_or(0);
        out.push(queue_depth.saturating_add(fec_pressure));
    }
}

fn sync_shred_dedupe_runtime_metrics(cache: Option<&ShredDedupeCache>) {
    let metrics = cache.map_or_else(
        crate::app::state::ShredDedupeCacheMetrics::default,
        ShredDedupeCache::metrics,
    );
    crate::runtime_metrics::set_shred_dedupe_metrics(
        metrics.entries,
        metrics.max_entries,
        metrics.queue_depth,
        metrics.max_queue_depth,
    );
    crate::runtime_metrics::set_shred_dedupe_evictions(
        metrics.capacity_evictions_total,
        metrics.expired_evictions_total,
    );
}

fn source_addr_or_unspecified(source_addr: Option<SocketAddr>) -> SocketAddr {
    source_addr.unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0))
}

const fn observe_source_port(
    source_addr: SocketAddr,
    port_8899_count: &mut u64,
    port_8900_count: &mut u64,
    port_other_count: &mut u64,
) {
    match source_addr.port() {
        TURBINE_PRIMARY_SOURCE_PORT => {
            *port_8899_count = port_8899_count.saturating_add(1);
        }
        TURBINE_SECONDARY_SOURCE_PORT => {
            *port_8900_count = port_8900_count.saturating_add(1);
        }
        _ => {
            *port_other_count = port_other_count.saturating_add(1);
        }
    }
}

/// Shared inputs and counters used while dispatching one fork-tracker update.
struct ForkUpdateDispatchContext<'context> {
    /// Commitment tracker updated from the latest fork snapshot.
    tx_commitment_tracker: &'context CommitmentSlotTracker,
    /// Plugin host that receives observational fork events.
    plugin_host: &'context PluginHost,
    /// Derived-state host that receives authoritative fork events.
    derived_state_host: &'context DerivedStateHost,
    /// Whether plugin fork hooks are enabled for this runtime.
    plugin_hooks_enabled: bool,
    /// Whether derived-state fork hooks are enabled for this runtime.
    derived_state_hooks_enabled: bool,
    /// Aggregate count of slot status transitions seen so far.
    fork_status_transitions_total: &'context mut u64,
    /// Aggregate count of reorgs seen so far.
    fork_reorg_count: &'context mut u64,
    /// Aggregate count of orphaned slots seen so far.
    fork_orphaned_slots_total: &'context mut u64,
}

fn apply_fork_update(update: &ForkTrackerUpdate, context: &mut ForkUpdateDispatchContext<'_>) {
    context.tx_commitment_tracker.update(
        update.snapshot.confirmed_slot,
        update.snapshot.finalized_slot,
    );
    *context.fork_status_transitions_total = context
        .fork_status_transitions_total
        .saturating_add(u64::try_from(update.status_transitions.len()).unwrap_or(u64::MAX));
    for transition in &update.status_transitions {
        if transition.status == crate::event::ForkSlotStatus::Orphaned {
            *context.fork_orphaned_slots_total =
                context.fork_orphaned_slots_total.saturating_add(1);
        }
        let event = SlotStatusEvent {
            slot: transition.slot,
            parent_slot: transition.parent_slot,
            previous_status: transition.previous_status,
            status: transition.status,
            tip_slot: update.snapshot.tip_slot,
            confirmed_slot: update.snapshot.confirmed_slot,
            finalized_slot: update.snapshot.finalized_slot,
        };
        if context.derived_state_hooks_enabled {
            context.derived_state_host.on_slot_status(event);
        }
        if context.plugin_hooks_enabled {
            context.plugin_host.on_slot_status(event);
        }
    }
    if let Some(reorg) = update.reorg.as_ref() {
        *context.fork_reorg_count = context.fork_reorg_count.saturating_add(1);
        let event = ReorgEvent {
            old_tip: reorg.old_tip,
            new_tip: reorg.new_tip,
            common_ancestor: reorg.common_ancestor,
            detached_slots: reorg.detached_slots.clone(),
            attached_slots: reorg.attached_slots.clone(),
            confirmed_slot: update.snapshot.confirmed_slot,
            finalized_slot: update.snapshot.finalized_slot,
        };
        if context.derived_state_hooks_enabled {
            context.derived_state_host.on_reorg(event.clone());
        }
        if context.plugin_hooks_enabled {
            context.plugin_host.on_reorg(event);
        }
    }
}

#[cfg(feature = "gossip-bootstrap")]
#[derive(Default)]
struct UdpRelayPeers {
    total_candidates: usize,
    selected_peers: Vec<SocketAddr>,
}

#[cfg(feature = "gossip-bootstrap")]
fn collect_udp_relay_peers(
    cluster_info: &ClusterInfo,
    local_pubkey: Pubkey,
    peer_candidates: usize,
    fanout: usize,
    max_peers_per_ip: usize,
) -> UdpRelayPeers {
    if peer_candidates == 0 || fanout == 0 || max_peers_per_ip == 0 {
        return UdpRelayPeers::default();
    }

    let local_shred_version = cluster_info.my_shred_version();
    let mut peers = cluster_info.all_peers();
    peers.sort_unstable_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.0.wallclock().cmp(&left.0.wallclock()))
            .then_with(|| right.0.pubkey().to_bytes().cmp(&left.0.pubkey().to_bytes()))
    });
    let mut selected_peers = Vec::with_capacity(fanout.min(peer_candidates));
    let mut seen = HashSet::new();
    let mut selected_per_ip: HashMap<IpAddr, usize> = HashMap::new();
    let mut total_candidates = 0_usize;
    for (contact_info, _) in peers {
        if contact_info.pubkey() == &local_pubkey {
            continue;
        }
        if local_shred_version != 0 && contact_info.shred_version() != local_shred_version {
            continue;
        }
        let Some(candidate) = contact_info.tvu(solana_gossip::contact_info::Protocol::UDP) else {
            continue;
        };
        let ip = candidate.ip();
        if ip.is_unspecified() || ip.is_multicast() || candidate.port() == 0 {
            continue;
        }
        if !seen.insert(candidate) {
            continue;
        }
        total_candidates = total_candidates.saturating_add(1);
        if selected_peers.len() < fanout {
            let selected_on_ip = selected_per_ip.entry(ip).or_default();
            if *selected_on_ip >= max_peers_per_ip {
                continue;
            }
            selected_peers.push(candidate);
            *selected_on_ip = selected_on_ip.saturating_add(1);
        }
        if total_candidates >= peer_candidates && selected_peers.len() >= fanout {
            break;
        }
    }
    UdpRelayPeers {
        total_candidates,
        selected_peers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framework::{
        CheckpointBarrierEvent, DerivedStateCheckpoint, DerivedStateConsumer,
        DerivedStateConsumerFault, DerivedStateConsumerFaultKind, DerivedStateFeedEnvelope,
        DerivedStateFeedEvent, FeedSequence,
    };
    use std::sync::{Arc, Mutex};

    #[test]
    fn extension_dispatch_telemetry_aggregates_totals_and_maxima() {
        let metrics = vec![
            RuntimeExtensionDispatchMetrics {
                extension: "ext-a",
                dropped_events: 2,
                queue_depth: 5,
                max_queue_depth: 7,
                dispatched_events: 10,
                avg_dispatch_lag_us: 200,
                max_dispatch_lag_us: 900,
            },
            RuntimeExtensionDispatchMetrics {
                extension: "ext-b",
                dropped_events: 1,
                queue_depth: 4,
                max_queue_depth: 8,
                dispatched_events: 11,
                avg_dispatch_lag_us: 300,
                max_dispatch_lag_us: 700,
            },
        ];

        let snapshot = collect_extension_dispatch_telemetry(metrics);

        assert_eq!(snapshot.active_extensions, 2);
        assert_eq!(snapshot.dispatched_events, 21);
        assert_eq!(snapshot.dropped_events, 3);
        assert_eq!(snapshot.queue_depth, 9);
        assert_eq!(snapshot.max_queue_depth, 8);
        assert_eq!(snapshot.max_avg_dispatch_lag_us, 300);
        assert_eq!(snapshot.max_dispatch_lag_us, 900);
    }

    #[test]
    fn extension_dispatch_telemetry_handles_empty_metrics() {
        let snapshot = collect_extension_dispatch_telemetry(Vec::new());
        assert_eq!(snapshot, ExtensionDispatchTelemetrySnapshot::default());
    }

    #[test]
    fn shutdown_barrier_uses_fork_snapshot_watermarks() {
        let state = Arc::new(Mutex::new(Vec::<DerivedStateFeedEnvelope>::new()));
        let host = DerivedStateHost::builder()
            .add_consumer(DriverRecordingConsumer {
                state: Arc::clone(&state),
            })
            .build();

        emit_shutdown_checkpoint_barrier(
            &host,
            crate::app::state::ForkTrackerSnapshot {
                tracked_slots: 4,
                tip_slot: Some(88),
                confirmed_slot: Some(80),
                finalized_slot: Some(70),
            },
        );

        let state = state
            .lock()
            .expect("driver recording state mutex should not be poisoned");
        assert_eq!(state.len(), 1);
        assert!(matches!(
            state[0].event,
            DerivedStateFeedEvent::CheckpointBarrier(CheckpointBarrierEvent {
                barrier_sequence: FeedSequence(0),
                reason: CheckpointBarrierReason::ShutdownRequested,
            })
        ));
        assert_eq!(
            state[0].watermarks,
            FeedWatermarks {
                canonical_tip_slot: Some(88),
                processed_slot: Some(88),
                confirmed_slot: Some(80),
                finalized_slot: Some(70),
            }
        );
    }

    struct DriverRecordingConsumer {
        state: Arc<Mutex<Vec<DerivedStateFeedEnvelope>>>,
    }

    impl DerivedStateConsumer for DriverRecordingConsumer {
        fn name(&self) -> &'static str {
            "driver-recording-consumer"
        }

        fn state_version(&self) -> u32 {
            1
        }

        fn extension_version(&self) -> &'static str {
            "driver-test"
        }

        fn load_checkpoint(
            &mut self,
        ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
            Ok(None)
        }

        fn config(&self) -> crate::framework::DerivedStateConsumerConfig {
            crate::framework::DerivedStateConsumerConfig::new()
                .with_transaction_applied()
                .with_account_touch_key_partitions()
                .with_control_plane_observed()
        }

        fn apply(
            &mut self,
            envelope: &DerivedStateFeedEnvelope,
        ) -> Result<(), DerivedStateConsumerFault> {
            self.state
                .lock()
                .map_err(|_poison| {
                    DerivedStateConsumerFault::new(
                        DerivedStateConsumerFaultKind::ConsumerApplyFailed,
                        Some(envelope.sequence),
                        "driver recording state mutex poisoned during apply",
                    )
                })?
                .push(envelope.clone());
            Ok(())
        }

        fn flush_checkpoint(
            &mut self,
            _checkpoint: DerivedStateCheckpoint,
        ) -> Result<(), DerivedStateConsumerFault> {
            Ok(())
        }
    }
}
