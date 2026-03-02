#[cfg(feature = "gossip-bootstrap")]
use super::control_plane::{
    ClusterTopologyTracker, emit_leader_schedule_diff_event, emit_observed_slot_leader_event,
};
use super::*;
use thiserror::Error;

#[derive(Debug, Error)]
pub(in crate::app::runtime) enum RuntimeRunloopError {
    #[error("receiver runtime bootstrap failed: {source}")]
    ReceiverBootstrap {
        source: bootstrap::gossip::ReceiverBootstrapError,
    },
}

// Runtime coordination defaults kept local to the runloop for operational clarity.
const RAW_PACKET_CHANNEL_CAPACITY: usize = 16_384;
const TX_EVENT_CHANNEL_CAPACITY: usize = 65_536;
const TELEMETRY_INTERVAL_SECS: u64 = 5;
const TURBINE_PRIMARY_SOURCE_PORT: u16 = 8_899;
const TURBINE_SECONDARY_SOURCE_PORT: u16 = 8_900;
const INITIAL_DEBUG_SAMPLE_LOG_LIMIT: u64 = 5;
#[cfg(feature = "gossip-bootstrap")]
const INITIAL_REPAIR_TRAFFIC_LOG_LIMIT: u64 = 8;
const SUBSTANTIAL_DATASET_MIN_SHREDS: usize = 2;
const CONTROL_PLANE_EVENT_TICK_MS: u64 = 250;
#[cfg(feature = "gossip-bootstrap")]
const CONTROL_PLANE_EVENT_SNAPSHOT_SECS: u64 = 30;

pub(in crate::app::runtime) async fn run_async_with_hosts(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
) -> Result<(), RuntimeRunloopError> {
    init_tracing();
    let log_startup_steps = read_log_startup_steps();
    if log_startup_steps {
        tracing::info!(step = "runtime_init", "SOF runtime starting");
    }

    let (tx, mut rx) = mpsc::channel::<RawPacketBatch>(RAW_PACKET_CHANNEL_CAPACITY);
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
    let dataset_worker_pool = spawn_dataset_workers(
        DatasetWorkerConfig {
            workers: dataset_workers,
            queue_capacity: dataset_queue_capacity,
            attempt_cache_capacity: dataset_attempt_cache_capacity,
            attempt_success_ttl: dataset_attempt_success_ttl,
            attempt_failure_ttl: dataset_attempt_failure_ttl,
            log_dataset_reconstruction,
        },
        &dataset_worker_shared,
    );
    let plugin_hooks_enabled = !plugin_host.is_empty();
    if plugin_hooks_enabled {
        tracing::info!(plugins = ?plugin_host.plugin_names(), "observer plugins enabled");
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
    let relay_cache_window_ms = read_relay_cache_window_ms();
    let relay_cache_max_shreds = read_relay_cache_max_shreds();
    let relay_cache = (relay_cache_window_ms > 0 && relay_cache_max_shreds > 0).then(|| {
        SharedRelayCache::new(RecentShredRingBuffer::new(
            relay_cache_max_shreds,
            Duration::from_millis(relay_cache_window_ms),
        ))
    });
    #[cfg(feature = "gossip-bootstrap")]
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
    let verify_enabled = read_verify_shreds();
    let live_shreds_enabled = read_live_shreds_enabled();
    if !live_shreds_enabled && verify_enabled {
        tracing::warn!("SOF_VERIFY_SHREDS=true ignored because SOF_LIVE_SHREDS_ENABLED=false");
    }
    let verify_enabled = live_shreds_enabled && verify_enabled;
    let verify_strict_unknown = read_verify_strict_unknown();
    let verify_recovered_shreds = read_verify_recovered_shreds();
    let dedupe_capacity = read_shred_dedupe_capacity();
    let dedupe_ttl_ms = read_shred_dedupe_ttl_ms();
    let mut dedupe_cache = (dedupe_capacity > 0 && dedupe_ttl_ms > 0)
        .then(|| RecentShredCache::new(dedupe_capacity, Duration::from_millis(dedupe_ttl_ms)));
    let verify_slot_leader_window = read_verify_slot_leader_window();
    let mut shred_verifier = verify_enabled.then(|| {
        ShredVerifier::new(
            read_verify_signature_cache_entries(),
            verify_slot_leader_window,
            Duration::from_millis(read_verify_unknown_retry_ms()),
        )
    });
    tracing::info!(
        verify_shreds = verify_enabled,
        verify_recovered_shreds,
        verify_strict_unknown,
        "shred verification configuration"
    );

    #[cfg(feature = "gossip-bootstrap")]
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
    let gossip_runtime_switch_enabled = repair_enabled && read_gossip_runtime_switch_enabled();
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
    let fec_max_tracked_sets = read_fec_max_tracked_sets();
    let mut dataset_reassembler = DataSetReassembler::new(dataset_max_tracked_slots)
        .with_tail_min_shreds_without_anchor(read_dataset_tail_min_shreds_without_anchor());
    let mut fec_recoverer = FecRecoverer::new(fec_max_tracked_sets);
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
    let mut dedupe_drop_count: u64 = 0;
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
    let mut repair_tick = interval(Duration::from_millis(read_repair_tick_ms()));
    let mut control_plane_tick = interval(Duration::from_millis(CONTROL_PLANE_EVENT_TICK_MS));
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

    loop {
        tokio::select! {
            maybe_packet_batch = rx.recv() => {
                let Some(packet_batch) = maybe_packet_batch else {
                    break;
                };
                for packet in packet_batch {
                let observed_at = Instant::now();
                let source_addr = packet.source;
                let packet_bytes = packet.bytes;
                if plugin_hooks_enabled {
                    plugin_host.on_raw_packet(RawPacketEvent {
                        source: source_addr,
                        bytes: Arc::from(packet_bytes.as_slice()),
                    });
                }
                if extension_hooks_enabled {
                    extension_host.on_observer_packet(source_addr, &packet_bytes);
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
                    && crate::repair::is_repair_response_ping_packet(&packet_bytes)
                    && let Some(command_tx) = repair_command_tx.as_ref() {
                        match command_tx.try_send(RepairCommand::HandleResponsePing {
                            packet: packet_bytes.clone(),
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
                    && crate::repair::is_supported_repair_request_packet(&packet_bytes)
                    && let Some(command_tx) = repair_command_tx.as_ref()
                {
                    match command_tx.try_send(RepairCommand::HandleServeRequest {
                        packet: packet_bytes.clone(),
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
                let parsed_shred = match parse_shred_header(&packet_bytes) {
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
                #[cfg(feature = "gossip-bootstrap")]
                let parsed_slot = match &parsed_shred {
                    ParsedShredHeader::Data(data) => data.common.slot,
                    ParsedShredHeader::Code(code) => code.common.slot,
                };
                if let Some(cache) = dedupe_cache.as_mut()
                    && cache.is_recent_duplicate(&packet_bytes, &parsed_shred, observed_at)
                {
                    dedupe_drop_count = dedupe_drop_count.saturating_add(1);
                    continue;
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
                        packet: Arc::from(packet_bytes.as_slice()),
                        parsed: Arc::new(parsed_shred.clone()),
                    });
                }
                if let Some(verifier) = shred_verifier.as_mut() {
                    let verify_status = verifier.verify_packet(&packet_bytes, observed_at);
                    match verify_status {
                        VerifyStatus::Verified => {
                            verify_verified_count = verify_verified_count.saturating_add(1);
                        }
                        VerifyStatus::UnknownLeader => {
                            verify_unknown_leader_count =
                                verify_unknown_leader_count.saturating_add(1);
                        }
                        VerifyStatus::InvalidMerkle => {
                            verify_invalid_merkle_count =
                                verify_invalid_merkle_count.saturating_add(1);
                        }
                        VerifyStatus::InvalidSignature => {
                            verify_invalid_signature_count =
                                verify_invalid_signature_count.saturating_add(1);
                        }
                        VerifyStatus::Malformed => {
                            verify_malformed_count = verify_malformed_count.saturating_add(1);
                        }
                    }
                    if !verify_status.is_accepted(verify_strict_unknown) {
                        verify_dropped_count = verify_dropped_count.saturating_add(1);
                        continue;
                    }
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
                                    match socket.send_to(packet_bytes.as_slice(), peer) {
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
                #[cfg(feature = "gossip-bootstrap")]
                if plugin_hooks_enabled
                    && let Some(verifier) = shred_verifier.as_mut()
                {
                    emit_leader_schedule_diff_event(
                        &plugin_host,
                        verifier,
                        latest_shred_slot,
                        &mut emitted_slot_leaders,
                    );
                    emit_observed_slot_leader_event(
                        &plugin_host,
                        verifier,
                        parsed_slot,
                        &mut emitted_slot_leaders,
                        verify_slot_leader_window,
                    );
                }
                #[cfg(feature = "gossip-bootstrap")]
                if repair_driver_enabled {
                    if repair_source_hints.record(source_addr).is_err() {
                        repair_source_hint_buffer_drops =
                            repair_source_hint_buffer_drops.saturating_add(1);
                    }
                    let should_flush = repair_source_hints.len() >= repair_source_hint_batch_size
                        || observed_at.saturating_duration_since(repair_source_hint_last_flush)
                            >= repair_source_hint_flush_interval;
                    if should_flush {
                        repair_source_hint_last_flush = observed_at;
                        flush_repair_source_hints(
                            &mut repair_source_hints,
                            repair_command_tx.as_ref(),
                            repair_source_hint_batch_size,
                            &mut repair_source_hint_drops,
                            &mut repair_source_hint_enqueued,
                        );
                    }
                }
                let recovered_packets = fec_recoverer.ingest_packet(&packet_bytes);

                match parsed_shred {
                    ParsedShredHeader::Data(data) => {
                        data_count = data_count.saturating_add(1);
                        match source_addr.port() {
                            TURBINE_PRIMARY_SOURCE_PORT => {
                                source_port_8899_data = source_port_8899_data.saturating_add(1);
                            }
                            TURBINE_SECONDARY_SOURCE_PORT => {
                                source_port_8900_data = source_port_8900_data.saturating_add(1);
                            }
                            _ => {
                                source_port_other_data = source_port_other_data.saturating_add(1);
                            }
                        }
                        if data.data_header.data_complete() {
                            data_complete_count = data_complete_count.saturating_add(1);
                        }
                        if data.data_header.last_in_slot() {
                            last_in_slot_count = last_in_slot_count.saturating_add(1);
                        }
                        note_latest_shred_slot(
                            &mut latest_shred_slot,
                            &mut latest_shred_updated_at,
                            data.common.slot,
                            observed_at,
                        );
                        let fork_update = fork_tracker.observe_data_shred(
                            data.common.slot,
                            derive_parent_slot(data.common.slot, data.data_header.parent_offset),
                        );
                        apply_fork_update(
                            &fork_update,
                            tx_commitment_tracker.as_ref(),
                            &plugin_host,
                            plugin_hooks_enabled,
                            &mut fork_status_transitions_total,
                            &mut fork_reorg_count,
                            &mut fork_orphaned_slots_total,
                        );
                        coverage_window.on_data_shred(data.common.slot);
                        if let Some(outstanding_repairs) = outstanding_repairs.as_mut()
                        {
                            let cleared = outstanding_repairs
                                .on_shred_received(data.common.slot, data.common.index);
                            repair_outstanding_cleared_on_receive =
                                repair_outstanding_cleared_on_receive
                                    .saturating_add(u64::try_from(cleared).unwrap_or(u64::MAX));
                        }
                        if let Some(tracker) = missing_tracker.as_mut() {
                            tracker.on_data_shred(
                                data.common.slot,
                                data.common.index,
                                data.common.fec_set_index,
                                data.data_header.last_in_slot(),
                                data.data_header.reference_tick(),
                                observed_at,
                            );
                        }
                        let datasets = dataset_reassembler.ingest_data_shred_meta(
                            data.common.slot,
                            data.common.index,
                            data.data_header.data_complete(),
                            data.data_header.last_in_slot(),
                            packet_bytes,
                        );
                        dataset_ranges_emitted = dataset_ranges_emitted
                            .saturating_add(u64::try_from(datasets.len()).unwrap_or(u64::MAX));
                        for dataset in datasets {
                            coverage_window.on_dataset_completed(dataset.slot);
                            let substantial_dataset =
                                dataset.serialized_shreds.len() >= SUBSTANTIAL_DATASET_MIN_SHREDS;
                            dispatch_completed_dataset(
                                dataset_worker_pool.queues(),
                                dataset,
                                dataset_jobs_enqueued_count.as_ref(),
                                dataset_queue_drop_count.as_ref(),
                            );
                            if substantial_dataset {
                                last_dataset_reconstructed_at = observed_at;
                            }
                        }
                        if data_count <= INITIAL_DEBUG_SAMPLE_LOG_LIMIT {
                            tracing::info!(
                                slot = data.common.slot,
                                index = data.common.index,
                                fec_set_index = data.common.fec_set_index,
                                flags = format_args!("0x{:02x}", data.data_header.flags),
                                declared_size = data.data_header.size,
                                payload_len = data.payload_len,
                                "received data shred"
                            );
                        }
                    }
                    ParsedShredHeader::Code(code) => {
                        code_count = code_count.saturating_add(1);
                        match source_addr.port() {
                            TURBINE_PRIMARY_SOURCE_PORT => {
                                source_port_8899_code = source_port_8899_code.saturating_add(1);
                            }
                            TURBINE_SECONDARY_SOURCE_PORT => {
                                source_port_8900_code = source_port_8900_code.saturating_add(1);
                            }
                            _ => {
                                source_port_other_code = source_port_other_code.saturating_add(1);
                            }
                        }
                        note_latest_shred_slot(
                            &mut latest_shred_slot,
                            &mut latest_shred_updated_at,
                            code.common.slot,
                            observed_at,
                        );
                        let fork_update = fork_tracker.observe_code_shred(code.common.slot);
                        apply_fork_update(
                            &fork_update,
                            tx_commitment_tracker.as_ref(),
                            &plugin_host,
                            plugin_hooks_enabled,
                            &mut fork_status_transitions_total,
                            &mut fork_reorg_count,
                            &mut fork_orphaned_slots_total,
                        );
                        coverage_window.on_code_shred(code.common.slot);
                        if let Some(outstanding_repairs) = outstanding_repairs.as_mut() {
                            let cleared = outstanding_repairs
                                .on_shred_received(code.common.slot, code.common.index);
                            repair_outstanding_cleared_on_receive = repair_outstanding_cleared_on_receive
                                .saturating_add(u64::try_from(cleared).unwrap_or(u64::MAX));
                        }
                        if let Some(tracker) = missing_tracker.as_mut() {
                            tracker.on_code_shred(
                                code.common.slot,
                                code.common.fec_set_index,
                                code.coding_header.num_data_shreds,
                                observed_at,
                            );
                        }
                        if code_count <= INITIAL_DEBUG_SAMPLE_LOG_LIMIT {
                            tracing::info!(
                                slot = code.common.slot,
                                index = code.common.index,
                                fec_set_index = code.common.fec_set_index,
                                num_data_shreds = code.coding_header.num_data_shreds,
                                num_coding_shreds = code.coding_header.num_coding_shreds,
                                "received coding shred"
                            );
                        }
                    }
                }

                for recovered in recovered_packets {
                    let parsed_recovered = match parse_shred(&recovered) {
                        Ok(parsed) => parsed,
                        Err(_) => continue,
                    };
                    if verify_recovered_shreds
                        && let Some(verifier) = shred_verifier.as_mut()
                    {
                        let verify_status = verifier.verify_packet(&recovered, observed_at);
                        match verify_status {
                            VerifyStatus::Verified => {
                                verify_verified_count = verify_verified_count.saturating_add(1);
                            }
                            VerifyStatus::UnknownLeader => {
                                verify_unknown_leader_count =
                                    verify_unknown_leader_count.saturating_add(1);
                            }
                            VerifyStatus::InvalidMerkle => {
                                verify_invalid_merkle_count =
                                    verify_invalid_merkle_count.saturating_add(1);
                            }
                            VerifyStatus::InvalidSignature => {
                                verify_invalid_signature_count =
                                    verify_invalid_signature_count.saturating_add(1);
                            }
                            VerifyStatus::Malformed => {
                                verify_malformed_count = verify_malformed_count.saturating_add(1);
                            }
                        }
                        if !verify_status.is_accepted(verify_strict_unknown) {
                            verify_dropped_count = verify_dropped_count.saturating_add(1);
                            continue;
                        }
                    }
                    match parsed_recovered {
                        ParsedShred::Data(data) => {
                            recovered_data_count = recovered_data_count.saturating_add(1);
                            if data.data_header.data_complete() {
                                data_complete_count = data_complete_count.saturating_add(1);
                            }
                            if data.data_header.last_in_slot() {
                                last_in_slot_count = last_in_slot_count.saturating_add(1);
                            }
                            note_latest_shred_slot(
                                &mut latest_shred_slot,
                                &mut latest_shred_updated_at,
                                data.common.slot,
                                observed_at,
                            );
                            let fork_update = fork_tracker.observe_recovered_data_shred(
                                data.common.slot,
                                derive_parent_slot(data.common.slot, data.data_header.parent_offset),
                            );
                            apply_fork_update(
                                &fork_update,
                                tx_commitment_tracker.as_ref(),
                                &plugin_host,
                                plugin_hooks_enabled,
                                &mut fork_status_transitions_total,
                                &mut fork_reorg_count,
                                &mut fork_orphaned_slots_total,
                            );
                            coverage_window.on_recovered_data_shred(data.common.slot);
                            if let Some(outstanding_repairs) = outstanding_repairs.as_mut()
                            {
                                let cleared = outstanding_repairs
                                    .on_shred_received(data.common.slot, data.common.index);
                                repair_outstanding_cleared_on_receive =
                                    repair_outstanding_cleared_on_receive
                                        .saturating_add(u64::try_from(cleared).unwrap_or(u64::MAX));
                            }
                            if let Some(tracker) = missing_tracker.as_mut() {
                                tracker.on_recovered_data_shred(
                                    data.common.slot,
                                    data.common.index,
                                    data.common.fec_set_index,
                                    data.data_header.last_in_slot(),
                                    data.data_header.reference_tick(),
                                    observed_at,
                                );
                            }
                            let datasets =
                                dataset_reassembler.ingest_data_shred_meta(
                                    data.common.slot,
                                    data.common.index,
                                    data.data_header.data_complete(),
                                    data.data_header.last_in_slot(),
                                    recovered,
                                );
                            let emitted = u64::try_from(datasets.len()).unwrap_or(u64::MAX);
                            dataset_ranges_emitted =
                                dataset_ranges_emitted.saturating_add(emitted);
                            dataset_ranges_emitted_from_recovered =
                                dataset_ranges_emitted_from_recovered.saturating_add(emitted);
                            for dataset in datasets {
                                coverage_window.on_dataset_completed(dataset.slot);
                                let substantial_dataset =
                                    dataset.serialized_shreds.len()
                                        >= SUBSTANTIAL_DATASET_MIN_SHREDS;
                                dispatch_completed_dataset(
                                    dataset_worker_pool.queues(),
                                    dataset,
                                    dataset_jobs_enqueued_count.as_ref(),
                                    dataset_queue_drop_count.as_ref(),
                                );
                                if substantial_dataset {
                                    last_dataset_reconstructed_at = observed_at;
                                }
                            }
                        }
                        ParsedShred::Code(_) => {}
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
            _ = repair_tick.tick(), if repair_enabled => {
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
                            .gossip_runtime
                            .as_ref()
                            .map(|runtime| runtime.ingest_telemetry.snapshot().0)
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
                                    if let Some(cache) = dedupe_cache.as_mut() {
                                        cache.clear();
                                    }
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
            _ = control_plane_tick.tick() => {
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
                        plugin_host.on_cluster_topology(topology_event);
                    }
                }
            }
            _ = telemetry_tick.tick() => {
                if packet_count == 0 && !logged_waiting_for_packets {
                    tracing::info!(
                        "waiting for ingress packets; check SOF_BIND / SOF_GOSSIP_ENTRYPOINT configuration"
                    );
                    logged_waiting_for_packets = true;
                }
                #[cfg(feature = "gossip-bootstrap")]
                {
                    if let (Some(verifier), Some(peer_snapshot)) =
                        (shred_verifier.as_mut(), repair_peer_snapshot.as_ref())
                    {
                        verifier.set_known_pubkeys(peer_snapshot.shared_get().known_pubkeys.clone());
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
                    .gossip_runtime
                    .as_ref()
                    .map(|runtime| {
                        let (packets_seen, last_packet_unix_ms) =
                            runtime.ingest_telemetry.snapshot();
                        (
                            packets_seen,
                            last_packet_unix_ms,
                            runtime.ingest_telemetry.sent_packets(),
                            runtime.ingest_telemetry.sent_batches(),
                            runtime.ingest_telemetry.dropped_packets(),
                            runtime.ingest_telemetry.dropped_batches(),
                            runtime.ingest_telemetry.rxq_ovfl_drops(),
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
                    dedupe_enabled = dedupe_cache.is_some(),
                    dedupe_capacity = dedupe_capacity,
                    dedupe_ttl_ms = dedupe_ttl_ms,
                    dedupe_entries = dedupe_cache.as_ref().map_or(0, RecentShredCache::len),
                    dedupe_drops = dedupe_drop_count,
                    tx_event_drops = tx_event_drop_count.load(Ordering::Relaxed),
                    dataset_decode_failures = dataset_decode_fail_count.load(Ordering::Relaxed),
                    dataset_tail_skips = dataset_tail_skip_count.load(Ordering::Relaxed),
                    dataset_duplicate_drops = dataset_duplicate_drop_count.load(Ordering::Relaxed),
                    dataset_queue_capacity = dataset_queue_capacity,
                    dataset_queue_drops = dataset_queue_drops,
                    dataset_queue_depth = dataset_queue_depth,
                    dataset_jobs_enqueued,
                    dataset_jobs_started,
                    dataset_jobs_completed,
                    dataset_jobs_pending,
                    dataset_slots_tracked = dataset_reassembler.tracked_slots(),
                    dataset_max_tracked_slots = dataset_max_tracked_slots,
                    fec_sets_tracked = fec_recoverer.tracked_sets(),
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
            maybe_event = runtime.tx_event_rx.recv() => {
                let Some(event) = maybe_event else {
                    break;
                };
                match event.kind {
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
                coverage_window.on_tx(event.slot);
                last_dataset_reconstructed_at = Instant::now();
                if log_all_txs || (log_non_vote_txs && !matches!(event.kind, TxKind::VoteOnly)) {
                    tracing::info!(
                        slot = event.slot,
                        signature = %event.signature,
                        kind = ?event.kind,
                        commitment_status = ?event.commitment_status,
                        "tx observed"
                    );
                }
            }
        }
    }
    dataset_worker_pool.shutdown().await;
    if extension_hooks_enabled {
        extension_host.shutdown().await;
    }
    drop(runtime);
    Ok(())
}

const fn derive_parent_slot(slot: u64, parent_offset: u16) -> Option<u64> {
    if parent_offset == 0 {
        return None;
    }
    slot.checked_sub(parent_offset as u64)
}

fn apply_fork_update(
    update: &ForkTrackerUpdate,
    tx_commitment_tracker: &CommitmentSlotTracker,
    plugin_host: &PluginHost,
    plugin_hooks_enabled: bool,
    fork_status_transitions_total: &mut u64,
    fork_reorg_count: &mut u64,
    fork_orphaned_slots_total: &mut u64,
) {
    tx_commitment_tracker.update(
        update.snapshot.confirmed_slot,
        update.snapshot.finalized_slot,
    );
    *fork_status_transitions_total = fork_status_transitions_total
        .saturating_add(u64::try_from(update.status_transitions.len()).unwrap_or(u64::MAX));
    for transition in &update.status_transitions {
        if transition.status == crate::event::ForkSlotStatus::Orphaned {
            *fork_orphaned_slots_total = fork_orphaned_slots_total.saturating_add(1);
        }
        if plugin_hooks_enabled {
            plugin_host.on_slot_status(SlotStatusEvent {
                slot: transition.slot,
                parent_slot: transition.parent_slot,
                previous_status: transition.previous_status,
                status: transition.status,
                tip_slot: update.snapshot.tip_slot,
                confirmed_slot: update.snapshot.confirmed_slot,
                finalized_slot: update.snapshot.finalized_slot,
            });
        }
    }
    if let Some(reorg) = update.reorg.as_ref() {
        *fork_reorg_count = fork_reorg_count.saturating_add(1);
        if plugin_hooks_enabled {
            plugin_host.on_reorg(ReorgEvent {
                old_tip: reorg.old_tip,
                new_tip: reorg.new_tip,
                common_ancestor: reorg.common_ancestor,
                detached_slots: reorg.detached_slots.clone(),
                attached_slots: reorg.attached_slots.clone(),
                confirmed_slot: update.snapshot.confirmed_slot,
                finalized_slot: update.snapshot.finalized_slot,
            });
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
