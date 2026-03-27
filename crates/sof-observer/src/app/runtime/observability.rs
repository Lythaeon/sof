use std::{
    collections::HashMap,
    io,
    net::SocketAddr,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::JoinHandle,
};

use crate::{
    framework::{
        DerivedStateConsumerRecoveryState, DerivedStateHost, PluginHost, RuntimeExtensionHost,
    },
    provider_stream::{
        ProviderSourceHealthEvent, ProviderSourceHealthStatus, ProviderSourceId,
        ProviderSourceIdentity,
    },
    runtime_metrics,
};

const METRICS_PATH: &str = "/metrics";
const HEALTH_PATH: &str = "/healthz";
const READY_PATH: &str = "/readyz";
const REQUEST_BUFFER_BYTES: usize = 8 * 1024;
const CONTENT_TYPE_TEXT: &str = "text/plain; charset=utf-8";
const CONTENT_TYPE_PROMETHEUS: &str = "text/plain; version=0.0.4; charset=utf-8";

#[derive(Clone, Debug, Default)]
pub(crate) struct RuntimeObservabilityHandle {
    inner: Arc<RuntimeObservabilityState>,
}

#[derive(Debug, Default)]
struct RuntimeObservabilityState {
    live: AtomicBool,
    ready: AtomicBool,
    provider_sources: RwLock<HashMap<ProviderSourceIdentity, ProviderSourceHealthEvent>>,
    provider_capability_mode: RwLock<Option<&'static str>>,
    provider_capability_unsupported_hooks: RwLock<Vec<String>>,
}

impl RuntimeObservabilityHandle {
    fn mark_live(&self) {
        self.inner.live.store(true, Ordering::Relaxed);
    }

    pub(crate) fn mark_ready(&self) {
        self.inner.ready.store(true, Ordering::Relaxed);
    }

    pub(crate) fn mark_not_ready(&self) {
        self.inner.ready.store(false, Ordering::Relaxed);
    }

    pub(crate) fn observe_provider_source_health(&self, event: &ProviderSourceHealthEvent) {
        let Ok(mut sources) = self.inner.provider_sources.write() else {
            return;
        };
        sources.insert(event.source.clone(), event.clone());
        let mut health_by_kind = HashMap::<ProviderSourceId, bool>::new();
        for source in sources.values() {
            let has_healthy = health_by_kind
                .entry(source.source.kind.clone())
                .or_insert(false);
            *has_healthy |= matches!(source.status, ProviderSourceHealthStatus::Healthy);
        }
        let ready = !health_by_kind.is_empty() && health_by_kind.values().all(|healthy| *healthy);
        self.inner.ready.store(ready, Ordering::Relaxed);
    }

    pub(crate) fn observe_provider_capability_warning(
        &self,
        mode: &'static str,
        unsupported_hooks: &[String],
    ) {
        if let Ok(mut capability_mode) = self.inner.provider_capability_mode.write() {
            *capability_mode = Some(mode);
        }
        if let Ok(mut hooks) = self.inner.provider_capability_unsupported_hooks.write() {
            hooks.clear();
            hooks.extend(unsupported_hooks.iter().cloned());
            hooks.sort();
            hooks.dedup();
        }
    }

    fn mark_stopped(&self) {
        self.inner.ready.store(false, Ordering::Relaxed);
        self.inner.live.store(false, Ordering::Relaxed);
    }

    fn snapshot(&self) -> RuntimeObservabilitySnapshot {
        let mut provider_sources = self.inner.provider_sources.read().map_or_else(
            |_error| Vec::new(),
            |sources| sources.values().cloned().collect(),
        );
        provider_sources.sort_by_key(|event| {
            (
                event.source.kind_str().to_owned(),
                event.source.instance_str().to_owned(),
                event.reason.as_str().to_owned(),
            )
        });
        let provider_capability_mode = self
            .inner
            .provider_capability_mode
            .read()
            .ok()
            .and_then(|mode| *mode);
        let provider_capability_unsupported_hooks = self
            .inner
            .provider_capability_unsupported_hooks
            .read()
            .map_or_else(|_error| Vec::new(), |hooks| hooks.clone());
        RuntimeObservabilitySnapshot {
            live: self.inner.live.load(Ordering::Relaxed),
            ready: self.inner.ready.load(Ordering::Relaxed),
            provider_sources,
            provider_capability_mode,
            provider_capability_unsupported_hooks,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct RuntimeObservabilitySnapshot {
    live: bool,
    ready: bool,
    provider_sources: Vec<ProviderSourceHealthEvent>,
    provider_capability_mode: Option<&'static str>,
    provider_capability_unsupported_hooks: Vec<String>,
}

pub(crate) struct RuntimeObservabilityService {
    handle: RuntimeObservabilityHandle,
    local_addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl RuntimeObservabilityService {
    pub(crate) async fn start(
        bind_addr: SocketAddr,
        plugin_host: PluginHost,
        extension_host: RuntimeExtensionHost,
        derived_state_host: DerivedStateHost,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(bind_addr).await?;
        let local_addr = listener.local_addr()?;
        let handle = RuntimeObservabilityHandle::default();
        handle.mark_live();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let service_handle = handle.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break,
                    accept_result = listener.accept() => {
                        let Ok((stream, _peer_addr)) = accept_result else {
                            break;
                        };
                        let request_handle = service_handle.clone();
                        let request_plugin_host = plugin_host.clone();
                        let request_extension_host = extension_host.clone();
                        let request_derived_state_host = derived_state_host.clone();
                        tokio::spawn(async move {
                            if let Err(error) = handle_connection(
                                stream,
                                request_handle,
                                request_plugin_host,
                                request_extension_host,
                                request_derived_state_host,
                            )
                            .await
                            {
                                tracing::debug!(%error, "observability endpoint request failed");
                            }
                        });
                    }
                }
            }
        });
        Ok(Self {
            handle,
            local_addr,
            shutdown_tx: Some(shutdown_tx),
            task,
        })
    }

    pub(crate) const fn handle(&self) -> &RuntimeObservabilityHandle {
        &self.handle
    }

    pub(crate) const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub(crate) async fn shutdown(mut self) {
        self.handle.mark_stopped();
        if let Some(shutdown_tx) = self.shutdown_tx.take()
            && shutdown_tx.send(()).is_err()
        {}
        if self.task.await.is_err() {
            tracing::warn!("observability endpoint task panicked during shutdown");
        }
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    handle: RuntimeObservabilityHandle,
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
) -> io::Result<()> {
    let response = match read_request_path(&mut stream).await? {
        Some(METRICS_PATH) => HttpResponse::ok(
            CONTENT_TYPE_PROMETHEUS,
            render_metrics(&handle, &plugin_host, &extension_host, &derived_state_host),
        ),
        Some(HEALTH_PATH) => {
            let snapshot = handle.snapshot();
            if snapshot.live {
                HttpResponse::ok(CONTENT_TYPE_TEXT, "ok\n".to_owned())
            } else {
                HttpResponse::service_unavailable(CONTENT_TYPE_TEXT, "stopped\n".to_owned())
            }
        }
        Some(READY_PATH) => {
            let snapshot = handle.snapshot();
            if snapshot.ready {
                HttpResponse::ok(CONTENT_TYPE_TEXT, "ready\n".to_owned())
            } else {
                HttpResponse::service_unavailable(CONTENT_TYPE_TEXT, "starting\n".to_owned())
            }
        }
        Some(_) => HttpResponse::not_found(),
        None => HttpResponse::bad_request(),
    };
    stream.write_all(response.serialize().as_bytes()).await?;
    stream.shutdown().await
}

async fn read_request_path(stream: &mut TcpStream) -> io::Result<Option<&'static str>> {
    let mut buffer = [0_u8; REQUEST_BUFFER_BYTES];
    let read = stream.read(&mut buffer).await?;
    if read == 0 {
        return Ok(None);
    }
    let Some(bytes) = buffer.get(..read) else {
        return Ok(None);
    };
    let request = match std::str::from_utf8(bytes) {
        Ok(request) => request,
        Err(_) => return Ok(None),
    };
    let mut parts = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    if method != "GET" {
        return Ok(None);
    }
    Ok(match path {
        METRICS_PATH => Some(METRICS_PATH),
        HEALTH_PATH => Some(HEALTH_PATH),
        READY_PATH => Some(READY_PATH),
        _ => Some(""),
    })
}

fn render_metrics(
    handle: &RuntimeObservabilityHandle,
    plugin_host: &PluginHost,
    extension_host: &RuntimeExtensionHost,
    derived_state_host: &DerivedStateHost,
) -> String {
    let mut buffer = String::with_capacity(16 * 1024);
    let state = handle.snapshot();
    append_metric_family(
        &mut buffer,
        "sof_runtime_live",
        "Runtime process liveness.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(&mut buffer, "sof_runtime_live", u8::from(state.live), None);
    append_metric_family(
        &mut buffer,
        "sof_runtime_ready",
        "Runtime readiness after receiver bootstrap completes.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_runtime_ready",
        u8::from(state.ready),
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_provider_sources_reconnecting",
        "Provider sources currently reconnecting.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_provider_sources_reconnecting",
        state
            .provider_sources
            .iter()
            .filter(|event| matches!(event.status, ProviderSourceHealthStatus::Reconnecting))
            .count(),
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_provider_sources_unhealthy",
        "Provider sources currently unhealthy.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_provider_sources_unhealthy",
        state
            .provider_sources
            .iter()
            .filter(|event| matches!(event.status, ProviderSourceHealthStatus::Unhealthy))
            .count(),
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_provider_source_status",
        "Typed provider source health state by source.",
        PrometheusMetricType::Gauge,
    );
    for event in &state.provider_sources {
        let labels = [
            ("source_kind", event.source.kind_str()),
            ("source_instance", event.source.instance_str()),
            (
                "status",
                match event.status {
                    ProviderSourceHealthStatus::Healthy => "healthy",
                    ProviderSourceHealthStatus::Reconnecting => "reconnecting",
                    ProviderSourceHealthStatus::Unhealthy => "unhealthy",
                },
            ),
            ("reason", event.reason.as_str()),
        ];
        append_metric_value(
            &mut buffer,
            "sof_provider_source_status",
            1_u8,
            Some(&labels),
        );
    }
    append_metric_family(
        &mut buffer,
        "sof_provider_capability_degraded",
        "Provider runtime started with unsupported requested hooks under warn policy.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_provider_capability_degraded",
        u8::from(!state.provider_capability_unsupported_hooks.is_empty()),
        None,
    );
    if let Some(mode) = state.provider_capability_mode {
        append_metric_family(
            &mut buffer,
            "sof_provider_capability_unsupported_hook",
            "Unsupported requested hook or derived-state feed tolerated under provider warn policy.",
            PrometheusMetricType::Gauge,
        );
        for hook in &state.provider_capability_unsupported_hooks {
            append_metric_value(
                &mut buffer,
                "sof_provider_capability_unsupported_hook",
                1_u8,
                Some(&[("mode", mode), ("hook", hook.as_str())]),
            );
        }
    }

    let snapshot = runtime_metrics::snapshot();
    append_metric_family(
        &mut buffer,
        "sof_ingest_packets_seen_total",
        "Packets observed by the active ingest source.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_ingest_packets_seen_total",
        snapshot.ingest_packets_seen_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_ingest_sent_packets_total",
        "Packets forwarded from ingest into runtime processing.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_ingest_sent_packets_total",
        snapshot.ingest_sent_packets_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_ingest_sent_batches_total",
        "Packet batches forwarded from ingest into runtime processing.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_ingest_sent_batches_total",
        snapshot.ingest_sent_batches_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_ingest_dropped_packets_total",
        "Packets dropped by ingest due to downstream backpressure.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_ingest_dropped_packets_total",
        snapshot.ingest_dropped_packets_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_ingest_dropped_batches_total",
        "Packet batches dropped by ingest due to downstream backpressure.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_ingest_dropped_batches_total",
        snapshot.ingest_dropped_batches_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_ingest_rxq_ovfl_drops_total",
        "Kernel receive queue overflow drops observed by ingest.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_ingest_rxq_ovfl_drops_total",
        snapshot.ingest_rxq_ovfl_drops_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_ingest_last_packet_age_ms",
        "Age in milliseconds of the latest packet observed by ingest.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_ingest_last_packet_age_ms",
        snapshot.ingest_last_packet_age_ms,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_recovered_data_packets_total",
        "Recovered data shreds accepted after FEC repair.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_recovered_data_packets_total",
        snapshot.recovered_data_packets_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_dataset_queue_depth",
        "Current aggregate dataset dispatch queue depth across dataset workers.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_dataset_queue_depth",
        snapshot.dataset_queue_depth,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_dataset_jobs_pending",
        "Current number of dataset jobs pending across dataset workers.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_dataset_jobs_pending",
        snapshot.dataset_jobs_pending,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_packet_worker_queue_depth",
        "Current aggregate packet-worker queue depth.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_packet_worker_queue_depth",
        snapshot.packet_worker_queue_depth,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_packet_worker_max_queue_depth",
        "Maximum aggregate packet-worker queue depth observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_packet_worker_max_queue_depth",
        snapshot.packet_worker_max_queue_depth,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_packet_worker_dropped_batches_total",
        "Packet-worker batches dropped due to queue pressure.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_packet_worker_dropped_batches_total",
        snapshot.packet_worker_dropped_batches_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_packet_worker_dropped_packets_total",
        "Packets dropped due to packet-worker queue pressure.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_packet_worker_dropped_packets_total",
        snapshot.packet_worker_dropped_packets_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_shred_dedupe_entries",
        "Current semantic shred dedupe cache entry count.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_shred_dedupe_entries",
        snapshot.shred_dedupe_entries,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_shred_dedupe_max_entries",
        "Maximum semantic shred dedupe cache entry count observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_shred_dedupe_max_entries",
        snapshot.shred_dedupe_max_entries,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_shred_dedupe_queue_depth",
        "Current semantic shred dedupe eviction queue depth.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_shred_dedupe_queue_depth",
        snapshot.shred_dedupe_queue_depth,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_shred_dedupe_max_queue_depth",
        "Maximum semantic shred dedupe eviction queue depth observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_shred_dedupe_max_queue_depth",
        snapshot.shred_dedupe_max_queue_depth,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_shred_dedupe_capacity_evictions_total",
        "Semantic shred dedupe evictions caused by capacity pressure.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_shred_dedupe_capacity_evictions_total",
        snapshot.shred_dedupe_capacity_evictions_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_shred_dedupe_expired_evictions_total",
        "Semantic shred dedupe evictions caused by expiry.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_shred_dedupe_expired_evictions_total",
        snapshot.shred_dedupe_expired_evictions_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_shred_dedupe_ingress_duplicate_drops_total",
        "Duplicate semantic shreds dropped at ingress.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_shred_dedupe_ingress_duplicate_drops_total",
        snapshot.shred_dedupe_ingress_duplicate_drops_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_shred_dedupe_ingress_conflict_drops_total",
        "Conflicting semantic shreds dropped at ingress.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_shred_dedupe_ingress_conflict_drops_total",
        snapshot.shred_dedupe_ingress_conflict_drops_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_shred_dedupe_canonical_duplicate_drops_total",
        "Duplicate semantic shreds dropped at canonical emission.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_shred_dedupe_canonical_duplicate_drops_total",
        snapshot.shred_dedupe_canonical_duplicate_drops_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_shred_dedupe_canonical_conflict_drops_total",
        "Conflicting semantic shreds dropped at canonical emission.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_shred_dedupe_canonical_conflict_drops_total",
        snapshot.shred_dedupe_canonical_conflict_drops_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_completed_datasets_total",
        "Completed datasets emitted from reassembly.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_completed_datasets_total",
        snapshot.completed_datasets_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_inline_reassembler_data_shreds_total",
        "Data shreds observed by the dedicated inline reassembler path.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_reassembler_data_shreds_total",
        snapshot.inline_reassembler_data_shreds_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_inline_reassembler_recovered_data_shreds_total",
        "Recovered data shreds observed by the dedicated inline reassembler path.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_reassembler_recovered_data_shreds_total",
        snapshot.inline_reassembler_recovered_data_shreds_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_inline_reassembler_code_shreds_total",
        "Code shreds observed by the dedicated inline reassembler path.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_reassembler_code_shreds_total",
        snapshot.inline_reassembler_code_shreds_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_inline_reassembler_fec_sets_ready_total",
        "FEC sets that became fully ready in the dedicated inline reassembler path.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_reassembler_fec_sets_ready_total",
        snapshot.inline_reassembler_fec_sets_ready_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_inline_reassembler_ranges_retired_total",
        "Completed ranges retired from the dedicated inline reassembler path.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_reassembler_ranges_retired_total",
        snapshot.inline_reassembler_ranges_retired_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_decoded_datasets_total",
        "Completed datasets successfully decoded into entries.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_decoded_datasets_total",
        snapshot.decoded_datasets_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_decode_failed_datasets_total",
        "Completed datasets that failed decode.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_decode_failed_datasets_total",
        snapshot.decode_failed_datasets_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_decoded_transactions_total",
        "Transactions decoded from completed datasets.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_decoded_transactions_total",
        snapshot.decoded_transactions_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_dataset_jobs_enqueued_total",
        "Dataset jobs enqueued for dataset workers.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_dataset_jobs_enqueued_total",
        snapshot.dataset_jobs_enqueued_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_dataset_queue_dropped_jobs_total",
        "Dataset jobs dropped due to queue pressure.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_dataset_queue_dropped_jobs_total",
        snapshot.dataset_queue_dropped_jobs_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_dataset_jobs_started_total",
        "Dataset jobs started by dataset workers.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_dataset_jobs_started_total",
        snapshot.dataset_jobs_started_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_dataset_jobs_completed_total",
        "Dataset jobs completed by dataset workers.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_dataset_jobs_completed_total",
        snapshot.dataset_jobs_completed_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_tx_event_dropped_total",
        "Transaction events dropped before downstream delivery.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_tx_event_dropped_total",
        snapshot.tx_event_dropped_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_transaction_plugin_visibility_samples_total",
        "Transaction plugin callbacks observed for visibility-lag tracking.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_transaction_plugin_visibility_samples_total",
        snapshot.transaction_plugin_visibility_samples_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_transaction_plugin_visibility_lag_us_total",
        "Total accumulated lag in microseconds from completed-dataset emission to transaction plugin callback start.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_transaction_plugin_visibility_lag_us_total",
        snapshot.transaction_plugin_visibility_lag_us_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_transaction_plugin_visibility_lag_us",
        "Lag in microseconds from completed-dataset emission to the most recent transaction plugin callback start.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_transaction_plugin_visibility_lag_us",
        snapshot.latest_transaction_plugin_visibility_lag_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_transaction_plugin_visibility_lag_us",
        "Maximum lag in microseconds from completed-dataset emission to transaction plugin callback start observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_transaction_plugin_visibility_lag_us",
        snapshot.max_transaction_plugin_visibility_lag_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_transaction_plugin_queue_wait_us_total",
        "Total accumulated queue wait in microseconds from completed-dataset emission to transaction worker dequeue.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_transaction_plugin_queue_wait_us_total",
        snapshot.transaction_plugin_queue_wait_us_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_transaction_plugin_queue_wait_us",
        "Queue wait in microseconds from completed-dataset emission to the most recent transaction worker dequeue.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_transaction_plugin_queue_wait_us",
        snapshot.latest_transaction_plugin_queue_wait_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_transaction_plugin_queue_wait_us",
        "Maximum queue wait in microseconds from completed-dataset emission to transaction worker dequeue observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_transaction_plugin_queue_wait_us",
        snapshot.max_transaction_plugin_queue_wait_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_transaction_plugin_callback_duration_us_total",
        "Total accumulated transaction plugin callback execution time in microseconds after dequeue.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_transaction_plugin_callback_duration_us_total",
        snapshot.transaction_plugin_callback_duration_us_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_transaction_plugin_callback_duration_us",
        "Callback execution time in microseconds for the most recent transaction plugin fanout after dequeue.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_transaction_plugin_callback_duration_us",
        snapshot.latest_transaction_plugin_callback_duration_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_transaction_plugin_callback_duration_us",
        "Maximum transaction plugin callback execution time in microseconds observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_transaction_plugin_callback_duration_us",
        snapshot.max_transaction_plugin_callback_duration_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_inline_transaction_plugin_latency_samples_total",
        "Inline transaction plugin callbacks observed for exact ingress-to-callback latency tracking.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_transaction_plugin_latency_samples_total",
        snapshot.inline_transaction_plugin_latency_samples_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_inline_transaction_plugin_first_shred_lag_us_total",
        "Total accumulated lag in microseconds from first shred observation to inline transaction plugin callback start.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_transaction_plugin_first_shred_lag_us_total",
        snapshot.inline_transaction_plugin_first_shred_lag_us_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_inline_transaction_plugin_first_shred_lag_us",
        "Lag in microseconds from first shred observation to the most recent inline transaction plugin callback start.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_inline_transaction_plugin_first_shred_lag_us",
        snapshot.latest_inline_transaction_plugin_first_shred_lag_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_inline_transaction_plugin_first_shred_lag_us",
        "Maximum lag in microseconds from first shred observation to inline transaction plugin callback start observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_inline_transaction_plugin_first_shred_lag_us",
        snapshot.max_inline_transaction_plugin_first_shred_lag_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_inline_transaction_plugin_last_shred_lag_us_total",
        "Total accumulated lag in microseconds from last shred observation to inline transaction plugin callback start.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_transaction_plugin_last_shred_lag_us_total",
        snapshot.inline_transaction_plugin_last_shred_lag_us_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_inline_transaction_plugin_last_shred_lag_us",
        "Lag in microseconds from last shred observation to the most recent inline transaction plugin callback start.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_inline_transaction_plugin_last_shred_lag_us",
        snapshot.latest_inline_transaction_plugin_last_shred_lag_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_inline_transaction_plugin_last_shred_lag_us",
        "Maximum lag in microseconds from last shred observation to inline transaction plugin callback start observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_inline_transaction_plugin_last_shred_lag_us",
        snapshot.max_inline_transaction_plugin_last_shred_lag_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_inline_transaction_plugin_completed_dataset_lag_us_total",
        "Total accumulated lag in microseconds from completed-dataset emission to inline transaction plugin callback start.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_transaction_plugin_completed_dataset_lag_us_total",
        snapshot.inline_transaction_plugin_completed_dataset_lag_us_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_inline_transaction_plugin_completed_dataset_lag_us",
        "Lag in microseconds from completed-dataset emission to the most recent inline transaction plugin callback start.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_inline_transaction_plugin_completed_dataset_lag_us",
        snapshot.latest_inline_transaction_plugin_completed_dataset_lag_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_inline_transaction_plugin_completed_dataset_lag_us",
        "Maximum lag in microseconds from completed-dataset emission to inline transaction plugin callback start observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_inline_transaction_plugin_completed_dataset_lag_us",
        snapshot.max_inline_transaction_plugin_completed_dataset_lag_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_inline_transaction_plugin_source_latency_samples_total",
        "Inline transaction plugin callbacks observed by inline source path.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_transaction_plugin_source_latency_samples_total",
        snapshot.inline_transaction_plugin_early_prefix_latency_samples_total,
        Some(&[(
            "source",
            crate::framework::host::InlineTransactionDispatchSource::EarlyPrefix.as_str(),
        )]),
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_transaction_plugin_source_latency_samples_total",
        snapshot.inline_transaction_plugin_completed_dataset_fallback_latency_samples_total,
        Some(&[(
            "source",
            crate::framework::host::InlineTransactionDispatchSource::CompletedDatasetFallback
                .as_str(),
        )]),
    );
    append_metric_family(
        &mut buffer,
        "sof_inline_transaction_plugin_source_first_shred_lag_us_total",
        "Total accumulated inline transaction lag in microseconds from first shred observation to callback start, split by inline source path.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_transaction_plugin_source_first_shred_lag_us_total",
        snapshot.inline_transaction_plugin_early_prefix_first_shred_lag_us_total,
        Some(&[(
            "source",
            crate::framework::host::InlineTransactionDispatchSource::EarlyPrefix.as_str(),
        )]),
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_transaction_plugin_source_first_shred_lag_us_total",
        snapshot.inline_transaction_plugin_completed_dataset_fallback_first_shred_lag_us_total,
        Some(&[(
            "source",
            crate::framework::host::InlineTransactionDispatchSource::CompletedDatasetFallback
                .as_str(),
        )]),
    );
    append_metric_family(
        &mut buffer,
        "sof_inline_transaction_plugin_source_last_shred_lag_us_total",
        "Total accumulated inline transaction lag in microseconds from last shred observation to callback start, split by inline source path.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_transaction_plugin_source_last_shred_lag_us_total",
        snapshot.inline_transaction_plugin_early_prefix_last_shred_lag_us_total,
        Some(&[(
            "source",
            crate::framework::host::InlineTransactionDispatchSource::EarlyPrefix.as_str(),
        )]),
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_transaction_plugin_source_last_shred_lag_us_total",
        snapshot.inline_transaction_plugin_completed_dataset_fallback_last_shred_lag_us_total,
        Some(&[(
            "source",
            crate::framework::host::InlineTransactionDispatchSource::CompletedDatasetFallback
                .as_str(),
        )]),
    );
    append_metric_family(
        &mut buffer,
        "sof_inline_transaction_plugin_source_completed_dataset_lag_us_total",
        "Total accumulated inline transaction lag in microseconds from reconstructability to callback start, split by inline source path.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_transaction_plugin_source_completed_dataset_lag_us_total",
        snapshot.inline_transaction_plugin_early_prefix_completed_dataset_lag_us_total,
        Some(&[(
            "source",
            crate::framework::host::InlineTransactionDispatchSource::EarlyPrefix.as_str(),
        )]),
    );
    append_metric_value(
        &mut buffer,
        "sof_inline_transaction_plugin_source_completed_dataset_lag_us_total",
        snapshot
            .inline_transaction_plugin_completed_dataset_fallback_completed_dataset_lag_us_total,
        Some(&[(
            "source",
            crate::framework::host::InlineTransactionDispatchSource::CompletedDatasetFallback
                .as_str(),
        )]),
    );
    append_metric_family(
        &mut buffer,
        "sof_transaction_batch_plugin_visibility_samples_total",
        "Total completed-dataset transaction-batch callbacks observed for visibility-lag tracking.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_transaction_batch_plugin_visibility_samples_total",
        snapshot.transaction_batch_plugin_visibility_samples_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_transaction_batch_plugin_visibility_lag_us_total",
        "Total accumulated lag in microseconds from completed-dataset emission to transaction-batch callback start.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_transaction_batch_plugin_visibility_lag_us_total",
        snapshot.transaction_batch_plugin_visibility_lag_us_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_transaction_batch_plugin_visibility_lag_us",
        "Lag in microseconds from completed-dataset emission to the most recent transaction-batch callback start.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_transaction_batch_plugin_visibility_lag_us",
        snapshot.latest_transaction_batch_plugin_visibility_lag_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_transaction_batch_plugin_visibility_lag_us",
        "Maximum lag in microseconds from completed-dataset emission to transaction-batch callback start observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_transaction_batch_plugin_visibility_lag_us",
        snapshot.max_transaction_batch_plugin_visibility_lag_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_transaction_batch_plugin_callback_duration_us_total",
        "Total accumulated transaction-batch callback execution time in microseconds after dequeue.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_transaction_batch_plugin_callback_duration_us_total",
        snapshot.transaction_batch_plugin_callback_duration_us_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_transaction_batch_plugin_callback_duration_us",
        "Callback execution time in microseconds for the most recent transaction-batch callback after dequeue.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_transaction_batch_plugin_callback_duration_us",
        snapshot.latest_transaction_batch_plugin_callback_duration_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_transaction_batch_plugin_callback_duration_us",
        "Maximum transaction-batch callback execution time in microseconds observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_transaction_batch_plugin_callback_duration_us",
        snapshot.max_transaction_batch_plugin_callback_duration_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_transaction_view_batch_plugin_visibility_samples_total",
        "Total completed-dataset transaction-view-batch callbacks observed for visibility-lag tracking.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_transaction_view_batch_plugin_visibility_samples_total",
        snapshot.transaction_view_batch_plugin_visibility_samples_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_transaction_view_batch_plugin_visibility_lag_us_total",
        "Total accumulated lag in microseconds from completed-dataset emission to transaction-view-batch callback start.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_transaction_view_batch_plugin_visibility_lag_us_total",
        snapshot.transaction_view_batch_plugin_visibility_lag_us_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_transaction_view_batch_plugin_visibility_lag_us",
        "Lag in microseconds from completed-dataset emission to the most recent transaction-view-batch callback start.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_transaction_view_batch_plugin_visibility_lag_us",
        snapshot.latest_transaction_view_batch_plugin_visibility_lag_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_transaction_view_batch_plugin_visibility_lag_us",
        "Maximum lag in microseconds from completed-dataset emission to transaction-view-batch callback start observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_transaction_view_batch_plugin_visibility_lag_us",
        snapshot.max_transaction_view_batch_plugin_visibility_lag_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_transaction_view_batch_plugin_callback_duration_us_total",
        "Total accumulated transaction-view-batch callback execution time in microseconds after dequeue.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_transaction_view_batch_plugin_callback_duration_us_total",
        snapshot.transaction_view_batch_plugin_callback_duration_us_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_transaction_view_batch_plugin_callback_duration_us",
        "Callback execution time in microseconds for the most recent transaction-view-batch callback after dequeue.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_transaction_view_batch_plugin_callback_duration_us",
        snapshot.latest_transaction_view_batch_plugin_callback_duration_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_transaction_view_batch_plugin_callback_duration_us",
        "Maximum transaction-view-batch callback execution time in microseconds observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_transaction_view_batch_plugin_callback_duration_us",
        snapshot.max_transaction_view_batch_plugin_callback_duration_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_transaction_plugin_first_in_dataset_samples_total",
        "Total transaction plugin callbacks observed for the first decoded transaction in a completed dataset.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_transaction_plugin_first_in_dataset_samples_total",
        snapshot.transaction_plugin_first_in_dataset_samples_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_transaction_plugin_first_in_dataset_queue_wait_us_total",
        "Total accumulated queue wait in microseconds for first-in-dataset transaction callbacks.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_transaction_plugin_first_in_dataset_queue_wait_us_total",
        snapshot.transaction_plugin_first_in_dataset_queue_wait_us_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_transaction_plugin_first_in_dataset_queue_wait_us",
        "Maximum queue wait in microseconds observed for first-in-dataset transaction callbacks.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_transaction_plugin_first_in_dataset_queue_wait_us",
        snapshot.max_transaction_plugin_first_in_dataset_queue_wait_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_transaction_plugin_nonfirst_in_dataset_samples_total",
        "Total transaction plugin callbacks observed for non-first decoded transactions in a completed dataset.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_transaction_plugin_nonfirst_in_dataset_samples_total",
        snapshot.transaction_plugin_nonfirst_in_dataset_samples_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_transaction_plugin_nonfirst_in_dataset_queue_wait_us_total",
        "Total accumulated queue wait in microseconds for non-first transaction callbacks in a completed dataset.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_transaction_plugin_nonfirst_in_dataset_queue_wait_us_total",
        snapshot.transaction_plugin_nonfirst_in_dataset_queue_wait_us_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_transaction_plugin_nonfirst_in_dataset_queue_wait_us",
        "Maximum queue wait in microseconds observed for non-first transaction callbacks in a completed dataset.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_transaction_plugin_nonfirst_in_dataset_queue_wait_us",
        snapshot.max_transaction_plugin_nonfirst_in_dataset_queue_wait_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_transaction_dataset_tx_position",
        "Zero-based dataset-local position of the most recent transaction callback sample.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_transaction_dataset_tx_position",
        snapshot.latest_transaction_dataset_tx_position,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_transaction_dataset_tx_position",
        "Maximum zero-based dataset-local transaction position observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_transaction_dataset_tx_position",
        snapshot.max_transaction_dataset_tx_position,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_transaction_dataset_tx_count",
        "Decoded transaction count in the completed dataset that produced the most recent transaction callback sample.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_transaction_dataset_tx_count",
        snapshot.latest_transaction_dataset_tx_count,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_transaction_dataset_tx_count",
        "Maximum decoded transaction count observed for a completed dataset carrying transaction callbacks since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_transaction_dataset_tx_count",
        snapshot.max_transaction_dataset_tx_count,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_dataset_worker_start_lag_ms",
        "Lag in milliseconds from completed-dataset emission to dataset processing start for the most recent dataset job. Inline transaction dispatch reports the same timing for immediate inline processing.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_dataset_worker_start_lag_ms",
        snapshot.latest_dataset_worker_start_lag_ms,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_dataset_worker_start_lag_ms",
        "Maximum lag in milliseconds from completed-dataset emission to dataset processing start observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_dataset_worker_start_lag_ms",
        snapshot.max_dataset_worker_start_lag_ms,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_dataset_processing_duration_us",
        "Processing duration in microseconds for the most recent completed dataset job.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_dataset_processing_duration_us",
        snapshot.latest_dataset_processing_duration_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_max_dataset_processing_duration_us",
        "Maximum dataset processing duration in microseconds observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_max_dataset_processing_duration_us",
        snapshot.max_dataset_processing_duration_us,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_shred_age_ms",
        "Age in milliseconds of the most recent canonical shred observed by the runtime.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_shred_age_ms",
        snapshot.latest_shred_age_ms,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_dataset_age_ms",
        "Age in milliseconds since the runtime last observed a reconstructed dataset emitted by reassembly.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_dataset_age_ms",
        snapshot.latest_dataset_age_ms,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_latest_substantial_dataset_age_ms",
        "Age in milliseconds since the runtime last observed a substantial reconstructed dataset emitted by reassembly.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_latest_substantial_dataset_age_ms",
        snapshot.latest_substantial_dataset_age_ms,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_gossip_runtime_stall_age_ms",
        "Age in milliseconds since the gossip runtime last made progress.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_gossip_runtime_stall_age_ms",
        snapshot.gossip_runtime_stall_age_ms,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_dynamic_stream_healthy",
        "Whether the dynamic repair stream is currently healthy.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_dynamic_stream_healthy",
        u8::from(snapshot.repair_dynamic_stream_healthy),
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_relay_cache_entries",
        "Current relay cache entry count.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_relay_cache_entries",
        snapshot.relay_cache_entries,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_relay_cache_inserts_total",
        "Relay cache inserts since startup.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_relay_cache_inserts_total",
        snapshot.relay_cache_inserts_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_relay_cache_replacements_total",
        "Relay cache replacements since startup.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_relay_cache_replacements_total",
        snapshot.relay_cache_replacements_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_relay_cache_evictions_total",
        "Relay cache evictions since startup.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_relay_cache_evictions_total",
        snapshot.relay_cache_evictions_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_udp_relay_candidates",
        "Current UDP relay candidate peer count after filtering.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_udp_relay_candidates",
        snapshot.udp_relay_candidates,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_udp_relay_peers",
        "Current UDP relay peer count selected for forwarding.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_udp_relay_peers",
        snapshot.udp_relay_peers,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_udp_relay_refreshes_total",
        "UDP relay peer refresh cycles since startup.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_udp_relay_refreshes_total",
        snapshot.udp_relay_refreshes_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_udp_relay_forwarded_packets_total",
        "Packets forwarded by the UDP relay.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_udp_relay_forwarded_packets_total",
        snapshot.udp_relay_forwarded_packets_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_udp_relay_send_attempts_total",
        "UDP relay send attempts.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_udp_relay_send_attempts_total",
        snapshot.udp_relay_send_attempts_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_udp_relay_send_errors_total",
        "UDP relay send errors.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_udp_relay_send_errors_total",
        snapshot.udp_relay_send_errors_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_udp_relay_rate_limited_packets_total",
        "UDP relay packets dropped by rate limiting.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_udp_relay_rate_limited_packets_total",
        snapshot.udp_relay_rate_limited_packets_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_udp_relay_source_filtered_packets_total",
        "Packets filtered out before UDP relay forwarding.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_udp_relay_source_filtered_packets_total",
        snapshot.udp_relay_source_filtered_packets_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_udp_relay_backoff_events_total",
        "UDP relay backoff activations.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_udp_relay_backoff_events_total",
        snapshot.udp_relay_backoff_events_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_udp_relay_backoff_drops_total",
        "Packets dropped due to UDP relay backoff.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_udp_relay_backoff_drops_total",
        snapshot.udp_relay_backoff_drops_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_requests_total",
        "Repair requests considered by the runtime.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_requests_total",
        snapshot.repair_requests_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_requests_enqueued_total",
        "Repair requests enqueued for the repair driver.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_requests_enqueued_total",
        snapshot.repair_requests_enqueued_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_requests_sent_total",
        "Repair requests successfully sent.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_requests_sent_total",
        snapshot.repair_requests_sent_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_requests_no_peer_total",
        "Repair requests skipped because no peer was available.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_requests_no_peer_total",
        snapshot.repair_requests_no_peer_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_request_errors_total",
        "Repair request send errors.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_request_errors_total",
        snapshot.repair_request_errors_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_request_queue_drops_total",
        "Repair requests dropped before enqueue due to queue pressure.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_request_queue_drops_total",
        snapshot.repair_request_queue_drops_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_requests_skipped_outstanding_total",
        "Repair requests skipped because one was already outstanding.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_requests_skipped_outstanding_total",
        snapshot.repair_requests_skipped_outstanding_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_outstanding_entries",
        "Current outstanding repair request count.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_outstanding_entries",
        snapshot.repair_outstanding_entries,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_outstanding_purged_total",
        "Outstanding repair requests purged by timeout.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_outstanding_purged_total",
        snapshot.repair_outstanding_purged_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_outstanding_cleared_on_receive_total",
        "Outstanding repair requests cleared when data arrived.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_outstanding_cleared_on_receive_total",
        snapshot.repair_outstanding_cleared_on_receive_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_response_pings_total",
        "Repair response pings sent.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_response_pings_total",
        snapshot.repair_response_pings_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_response_ping_errors_total",
        "Repair response ping send errors.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_response_ping_errors_total",
        snapshot.repair_response_ping_errors_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_ping_queue_drops_total",
        "Repair pings dropped due to queue pressure.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_ping_queue_drops_total",
        snapshot.repair_ping_queue_drops_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_serve_requests_enqueued_total",
        "Incoming repair serve requests enqueued.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_serve_requests_enqueued_total",
        snapshot.repair_serve_requests_enqueued_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_serve_requests_handled_total",
        "Incoming repair serve requests handled.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_serve_requests_handled_total",
        snapshot.repair_serve_requests_handled_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_serve_responses_sent_total",
        "Repair serve responses sent.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_serve_responses_sent_total",
        snapshot.repair_serve_responses_sent_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_serve_cache_misses_total",
        "Repair serve cache misses.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_serve_cache_misses_total",
        snapshot.repair_serve_cache_misses_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_serve_rate_limited_total",
        "Repair serve drops due to aggregate rate limiting.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_serve_rate_limited_total",
        snapshot.repair_serve_rate_limited_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_serve_rate_limited_peer_total",
        "Repair serve drops due to per-peer rate limiting.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_serve_rate_limited_peer_total",
        snapshot.repair_serve_rate_limited_peer_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_serve_rate_limited_bytes_total",
        "Repair serve bytes dropped by byte budgeting.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_serve_rate_limited_bytes_total",
        snapshot.repair_serve_rate_limited_bytes_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_serve_errors_total",
        "Repair serve response errors.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_serve_errors_total",
        snapshot.repair_serve_errors_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_serve_queue_drops_total",
        "Repair serve queue drops.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_serve_queue_drops_total",
        snapshot.repair_serve_queue_drops_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_source_hint_enqueued_total",
        "Repair source hints enqueued.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_source_hint_enqueued_total",
        snapshot.repair_source_hint_enqueued_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_source_hint_drops_total",
        "Repair source hints dropped during enqueue.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_source_hint_drops_total",
        snapshot.repair_source_hint_drops_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_source_hint_buffer_drops_total",
        "Repair source hints dropped by the hint buffer.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_source_hint_buffer_drops_total",
        snapshot.repair_source_hint_buffer_drops_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_peer_total",
        "Current repair peer count known to the runtime.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_peer_total",
        snapshot.repair_peer_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_repair_peer_active",
        "Current active repair peer count after runtime filtering.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_repair_peer_active",
        snapshot.repair_peer_active,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_gossip_runtime_switch_attempts_total",
        "Gossip-runtime switch attempts.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_gossip_runtime_switch_attempts_total",
        snapshot.gossip_runtime_switch_attempts_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_gossip_runtime_switch_successes_total",
        "Successful gossip-runtime switches.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_gossip_runtime_switch_successes_total",
        snapshot.gossip_runtime_switch_successes_total,
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_gossip_runtime_switch_failures_total",
        "Failed gossip-runtime switches.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_gossip_runtime_switch_failures_total",
        snapshot.gossip_runtime_switch_failures_total,
        None,
    );

    append_metric_family(
        &mut buffer,
        "sof_plugin_general_dropped_events_total",
        "Non-transaction plugin events dropped by the bounded plugin dispatcher.",
        PrometheusMetricType::Counter,
    );
    append_metric_family(
        &mut buffer,
        "sof_plugin_general_queue_depth",
        "Current non-transaction plugin dispatch queue depth.",
        PrometheusMetricType::Gauge,
    );
    append_metric_family(
        &mut buffer,
        "sof_plugin_general_max_queue_depth",
        "Maximum non-transaction plugin dispatch queue depth observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_family(
        &mut buffer,
        "sof_plugin_transaction_critical_dropped_events_total",
        "Critical accepted-transaction plugin events dropped by bounded dispatch lanes.",
        PrometheusMetricType::Counter,
    );
    append_metric_family(
        &mut buffer,
        "sof_plugin_transaction_background_dropped_events_total",
        "Background accepted-transaction plugin events dropped by bounded dispatch lanes.",
        PrometheusMetricType::Counter,
    );
    append_metric_family(
        &mut buffer,
        "sof_plugin_transaction_inline_critical_queue_depth",
        "Current inline-critical accepted-transaction dispatch queue depth.",
        PrometheusMetricType::Gauge,
    );
    append_metric_family(
        &mut buffer,
        "sof_plugin_transaction_inline_critical_max_queue_depth",
        "Maximum inline-critical accepted-transaction dispatch queue depth observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_family(
        &mut buffer,
        "sof_plugin_transaction_critical_queue_depth",
        "Current aggregate critical accepted-transaction dispatch queue depth.",
        PrometheusMetricType::Gauge,
    );
    append_metric_family(
        &mut buffer,
        "sof_plugin_transaction_critical_max_queue_depth",
        "Maximum aggregate critical accepted-transaction dispatch queue depth observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_family(
        &mut buffer,
        "sof_plugin_transaction_background_queue_depth",
        "Current aggregate background accepted-transaction dispatch queue depth.",
        PrometheusMetricType::Gauge,
    );
    append_metric_family(
        &mut buffer,
        "sof_plugin_transaction_background_max_queue_depth",
        "Maximum aggregate background accepted-transaction dispatch queue depth observed since startup.",
        PrometheusMetricType::Gauge,
    );
    let tx_queue_metrics = plugin_host.transaction_queue_metrics();
    append_metric_value(
        &mut buffer,
        "sof_plugin_general_dropped_events_total",
        plugin_host.general_dropped_event_count(),
        None,
    );
    append_metric_value(
        &mut buffer,
        "sof_plugin_general_queue_depth",
        plugin_host.general_queue_depth(),
        None,
    );
    append_metric_value(
        &mut buffer,
        "sof_plugin_general_max_queue_depth",
        plugin_host.general_max_queue_depth(),
        None,
    );
    append_metric_value(
        &mut buffer,
        "sof_plugin_transaction_critical_dropped_events_total",
        plugin_host.transaction_dropped_event_count(),
        None,
    );
    append_metric_value(
        &mut buffer,
        "sof_plugin_transaction_background_dropped_events_total",
        plugin_host.background_transaction_dropped_event_count(),
        None,
    );
    append_metric_value(
        &mut buffer,
        "sof_plugin_transaction_inline_critical_queue_depth",
        tx_queue_metrics.inline_critical_queue_depth,
        None,
    );
    append_metric_value(
        &mut buffer,
        "sof_plugin_transaction_inline_critical_max_queue_depth",
        tx_queue_metrics.inline_critical_max_queue_depth,
        None,
    );
    append_metric_value(
        &mut buffer,
        "sof_plugin_transaction_critical_queue_depth",
        tx_queue_metrics.critical_queue_depth,
        None,
    );
    append_metric_value(
        &mut buffer,
        "sof_plugin_transaction_critical_max_queue_depth",
        tx_queue_metrics.critical_max_queue_depth,
        None,
    );
    append_metric_value(
        &mut buffer,
        "sof_plugin_transaction_background_queue_depth",
        tx_queue_metrics.background_queue_depth,
        None,
    );
    append_metric_value(
        &mut buffer,
        "sof_plugin_transaction_background_max_queue_depth",
        tx_queue_metrics.background_max_queue_depth,
        None,
    );

    append_metric_family(
        &mut buffer,
        "sof_runtime_extension_dropped_events_total",
        "Runtime extension events dropped by dispatcher.",
        PrometheusMetricType::Counter,
    );
    append_metric_family(
        &mut buffer,
        "sof_runtime_extension_queue_depth",
        "Current runtime extension dispatch queue depth.",
        PrometheusMetricType::Gauge,
    );
    append_metric_family(
        &mut buffer,
        "sof_runtime_extension_max_queue_depth",
        "Maximum runtime extension dispatch queue depth observed since startup.",
        PrometheusMetricType::Gauge,
    );
    append_metric_family(
        &mut buffer,
        "sof_runtime_extension_dispatched_events_total",
        "Runtime extension events delivered to `on_packet_received`.",
        PrometheusMetricType::Counter,
    );
    append_metric_family(
        &mut buffer,
        "sof_runtime_extension_avg_dispatch_lag_us",
        "Mean runtime extension queue wait time before callback dispatch.",
        PrometheusMetricType::Gauge,
    );
    append_metric_family(
        &mut buffer,
        "sof_runtime_extension_max_dispatch_lag_us",
        "Maximum runtime extension queue wait time before callback dispatch.",
        PrometheusMetricType::Gauge,
    );
    for metric in extension_host.dispatch_metrics_by_extension() {
        let labels = [("extension", metric.extension)];
        append_metric_value(
            &mut buffer,
            "sof_runtime_extension_dropped_events_total",
            metric.dropped_events,
            Some(&labels),
        );
        append_metric_value(
            &mut buffer,
            "sof_runtime_extension_queue_depth",
            metric.queue_depth,
            Some(&labels),
        );
        append_metric_value(
            &mut buffer,
            "sof_runtime_extension_max_queue_depth",
            metric.max_queue_depth,
            Some(&labels),
        );
        append_metric_value(
            &mut buffer,
            "sof_runtime_extension_dispatched_events_total",
            metric.dispatched_events,
            Some(&labels),
        );
        append_metric_value(
            &mut buffer,
            "sof_runtime_extension_avg_dispatch_lag_us",
            metric.avg_dispatch_lag_us,
            Some(&labels),
        );
        append_metric_value(
            &mut buffer,
            "sof_runtime_extension_max_dispatch_lag_us",
            metric.max_dispatch_lag_us,
            Some(&labels),
        );
    }

    append_metric_family(
        &mut buffer,
        "sof_derived_state_healthy_consumers",
        "Healthy derived-state consumer count.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_derived_state_healthy_consumers",
        derived_state_host.healthy_consumer_count(),
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_derived_state_unhealthy_consumers",
        "Unhealthy derived-state consumer count.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_derived_state_unhealthy_consumers",
        derived_state_host.unhealthy_consumer_names().len(),
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_derived_state_pending_recovery_consumers",
        "Derived-state consumers waiting for replay-based recovery.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_derived_state_pending_recovery_consumers",
        derived_state_host.consumers_pending_recovery().len(),
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_derived_state_rebuild_required_consumers",
        "Derived-state consumers requiring a rebuild.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_derived_state_rebuild_required_consumers",
        derived_state_host.consumers_requiring_rebuild().len(),
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_derived_state_fault_total",
        "Structured derived-state consumer faults recorded by the host.",
        PrometheusMetricType::Counter,
    );
    append_metric_value(
        &mut buffer,
        "sof_derived_state_fault_total",
        derived_state_host.fault_count(),
        None,
    );
    append_metric_family(
        &mut buffer,
        "sof_derived_state_last_emitted_sequence",
        "Highest derived-state feed sequence emitted by the host.",
        PrometheusMetricType::Gauge,
    );
    append_metric_value(
        &mut buffer,
        "sof_derived_state_last_emitted_sequence",
        derived_state_host
            .last_emitted_sequence()
            .map_or(0_u64, |sequence| sequence.0),
        None,
    );

    append_metric_family(
        &mut buffer,
        "sof_derived_state_consumer_unhealthy",
        "Whether one derived-state consumer is unhealthy.",
        PrometheusMetricType::Gauge,
    );
    append_metric_family(
        &mut buffer,
        "sof_derived_state_consumer_applied_events_total",
        "Derived-state envelopes successfully applied by one consumer.",
        PrometheusMetricType::Counter,
    );
    append_metric_family(
        &mut buffer,
        "sof_derived_state_consumer_checkpoint_flushes_total",
        "Derived-state checkpoints flushed by one consumer.",
        PrometheusMetricType::Counter,
    );
    append_metric_family(
        &mut buffer,
        "sof_derived_state_consumer_fault_total",
        "Structured faults recorded for one derived-state consumer.",
        PrometheusMetricType::Counter,
    );
    append_metric_family(
        &mut buffer,
        "sof_derived_state_consumer_last_applied_sequence",
        "Highest derived-state sequence applied by one consumer.",
        PrometheusMetricType::Gauge,
    );
    for telemetry in derived_state_host.consumer_telemetry() {
        let labels = [
            ("consumer", telemetry.name),
            (
                "recovery_state",
                match telemetry.recovery_state {
                    DerivedStateConsumerRecoveryState::Live => "live",
                    DerivedStateConsumerRecoveryState::ReplayRecoveryPending => {
                        "replay_recovery_pending"
                    }
                    DerivedStateConsumerRecoveryState::RebuildRequired => "rebuild_required",
                },
            ),
        ];
        append_metric_value(
            &mut buffer,
            "sof_derived_state_consumer_unhealthy",
            u8::from(telemetry.unhealthy),
            Some(&labels),
        );
        append_metric_value(
            &mut buffer,
            "sof_derived_state_consumer_applied_events_total",
            telemetry.applied_events,
            Some(&labels),
        );
        append_metric_value(
            &mut buffer,
            "sof_derived_state_consumer_checkpoint_flushes_total",
            telemetry.checkpoint_flushes,
            Some(&labels),
        );
        append_metric_value(
            &mut buffer,
            "sof_derived_state_consumer_fault_total",
            telemetry.fault_count,
            Some(&labels),
        );
        append_metric_value(
            &mut buffer,
            "sof_derived_state_consumer_last_applied_sequence",
            telemetry
                .last_applied_sequence
                .map_or(0_u64, |sequence| sequence.0),
            Some(&labels),
        );
    }

    if let Some(replay) = derived_state_host.replay_telemetry() {
        let labels = [("backend", replay.backend.as_str())];
        append_metric_family(
            &mut buffer,
            "sof_derived_state_replay_enabled",
            "Whether the runtime installed a derived-state replay backend.",
            PrometheusMetricType::Gauge,
        );
        append_metric_value(
            &mut buffer,
            "sof_derived_state_replay_enabled",
            u8::from(replay.enabled),
            Some(&labels),
        );
        append_metric_family(
            &mut buffer,
            "sof_derived_state_replay_retained_sessions",
            "Derived-state replay sessions retained by the backend.",
            PrometheusMetricType::Gauge,
        );
        append_metric_value(
            &mut buffer,
            "sof_derived_state_replay_retained_sessions",
            replay.retained_sessions,
            Some(&labels),
        );
        append_metric_family(
            &mut buffer,
            "sof_derived_state_replay_retained_envelopes",
            "Derived-state replay envelopes retained by the backend.",
            PrometheusMetricType::Gauge,
        );
        append_metric_value(
            &mut buffer,
            "sof_derived_state_replay_retained_envelopes",
            replay.retained_envelopes,
            Some(&labels),
        );
        append_metric_family(
            &mut buffer,
            "sof_derived_state_replay_truncated_envelopes_total",
            "Derived-state replay envelopes truncated by retention policy.",
            PrometheusMetricType::Counter,
        );
        append_metric_value(
            &mut buffer,
            "sof_derived_state_replay_truncated_envelopes_total",
            replay.truncated_envelopes,
            Some(&labels),
        );
        append_metric_family(
            &mut buffer,
            "sof_derived_state_replay_append_failures_total",
            "Derived-state replay backend append failures.",
            PrometheusMetricType::Counter,
        );
        append_metric_value(
            &mut buffer,
            "sof_derived_state_replay_append_failures_total",
            replay.append_failures,
            Some(&labels),
        );
        append_metric_family(
            &mut buffer,
            "sof_derived_state_replay_load_failures_total",
            "Derived-state replay backend load failures.",
            PrometheusMetricType::Counter,
        );
        append_metric_value(
            &mut buffer,
            "sof_derived_state_replay_load_failures_total",
            replay.load_failures,
            Some(&labels),
        );
        append_metric_family(
            &mut buffer,
            "sof_derived_state_replay_compactions_total",
            "Derived-state replay backend compaction runs.",
            PrometheusMetricType::Counter,
        );
        append_metric_value(
            &mut buffer,
            "sof_derived_state_replay_compactions_total",
            replay.compactions,
            Some(&labels),
        );
    }

    buffer
}

#[derive(Debug, Clone, Copy)]
enum PrometheusMetricType {
    Counter,
    Gauge,
}

impl PrometheusMetricType {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
        }
    }
}

fn append_metric_family(
    buffer: &mut String,
    name: &str,
    help: &str,
    metric_type: PrometheusMetricType,
) {
    buffer.push_str("# HELP ");
    buffer.push_str(name);
    buffer.push(' ');
    buffer.push_str(help);
    buffer.push('\n');
    buffer.push_str("# TYPE ");
    buffer.push_str(name);
    buffer.push(' ');
    buffer.push_str(metric_type.as_str());
    buffer.push('\n');
}

fn append_metric_value<T>(
    buffer: &mut String,
    name: &str,
    value: T,
    labels: Option<&[(&str, &str)]>,
) where
    T: std::fmt::Display,
{
    buffer.push_str(name);
    if let Some(labels) = labels
        && !labels.is_empty()
    {
        buffer.push('{');
        for (index, (key, label_value)) in labels.iter().enumerate() {
            if index > 0 {
                buffer.push(',');
            }
            buffer.push_str(key);
            buffer.push_str("=\"");
            append_escaped_label_value(buffer, label_value);
            buffer.push('"');
        }
        buffer.push('}');
    }
    buffer.push(' ');
    buffer.push_str(&value.to_string());
    buffer.push('\n');
}

fn append_escaped_label_value(buffer: &mut String, value: &str) {
    for byte in value.bytes() {
        match byte {
            b'\\' => buffer.push_str("\\\\"),
            b'"' => buffer.push_str("\\\""),
            b'\n' => buffer.push_str("\\n"),
            _ => buffer.push(byte as char),
        }
    }
}

#[derive(Debug, Clone)]
struct HttpResponse {
    status_line: &'static str,
    content_type: &'static str,
    body: String,
}

impl HttpResponse {
    const fn ok(content_type: &'static str, body: String) -> Self {
        Self {
            status_line: "HTTP/1.1 200 OK",
            content_type,
            body,
        }
    }

    const fn service_unavailable(content_type: &'static str, body: String) -> Self {
        Self {
            status_line: "HTTP/1.1 503 Service Unavailable",
            content_type,
            body,
        }
    }

    fn bad_request() -> Self {
        Self::service_unavailable(CONTENT_TYPE_TEXT, "bad request\n".to_owned())
    }

    fn not_found() -> Self {
        Self {
            status_line: "HTTP/1.1 404 Not Found",
            content_type: CONTENT_TYPE_TEXT,
            body: "not found\n".to_owned(),
        }
    }

    fn serialize(&self) -> String {
        format!(
            "{}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            self.status_line,
            self.content_type,
            self.body.len(),
            self.body
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };

    #[test]
    fn label_values_are_escaped_for_prometheus_output() {
        let mut buffer = String::new();
        append_metric_value(
            &mut buffer,
            "sof_test_metric",
            1,
            Some(&[("extension", "quote\"slash\\newline\n")]),
        );

        assert_eq!(
            buffer,
            "sof_test_metric{extension=\"quote\\\"slash\\\\newline\\n\"} 1\n"
        );
    }

    #[test]
    fn metrics_include_runtime_lifecycle_state() {
        let handle = RuntimeObservabilityHandle::default();
        handle.mark_live();
        handle.mark_ready();
        let metrics = render_metrics(
            &handle,
            &PluginHost::builder().build(),
            &RuntimeExtensionHost::builder().build(),
            &DerivedStateHost::builder().build(),
        );

        assert!(metrics.contains("sof_runtime_live 1"));
        assert!(metrics.contains("sof_runtime_ready 1"));
        assert!(metrics.contains("sof_ingest_packets_seen_total "));
        assert!(metrics.contains("sof_packet_worker_queue_depth "));
        assert!(metrics.contains("sof_plugin_general_queue_depth "));
        assert!(metrics.contains("sof_plugin_transaction_critical_queue_depth "));
        assert!(metrics.contains("sof_latest_shred_age_ms "));
        assert!(metrics.contains("sof_udp_relay_forwarded_packets_total "));
        assert!(metrics.contains("sof_repair_requests_total "));
        assert!(metrics.contains("sof_inline_transaction_plugin_source_latency_samples_total"));
    }

    #[test]
    fn metrics_include_provider_source_health() {
        let handle = RuntimeObservabilityHandle::default();
        handle.mark_live();
        handle.observe_provider_source_health(&ProviderSourceHealthEvent {
            source: crate::provider_stream::ProviderSourceIdentity::new(
                crate::provider_stream::ProviderSourceId::YellowstoneGrpc,
                "yellowstone_grpc-1",
            ),
            status: ProviderSourceHealthStatus::Reconnecting,
            reason: crate::provider_stream::ProviderSourceHealthReason::UpstreamProtocolFailure,
            message: "upstream stalled".to_owned(),
        });
        let metrics = render_metrics(
            &handle,
            &PluginHost::builder().build(),
            &RuntimeExtensionHost::builder().build(),
            &DerivedStateHost::builder().build(),
        );

        assert!(metrics.contains("sof_runtime_ready 0"));
        assert!(metrics.contains("sof_provider_sources_reconnecting 1"));
        assert!(metrics.contains("sof_provider_sources_unhealthy 0"));
        assert!(metrics.contains(
            "sof_provider_source_status{source_kind=\"yellowstone_grpc\",source_instance=\"yellowstone_grpc-1\",status=\"reconnecting\",reason=\"upstream_protocol_failure\"} 1"
        ));
    }

    #[test]
    fn metrics_keep_distinct_sources_with_same_instance_label() {
        let handle = RuntimeObservabilityHandle::default();
        handle.mark_live();
        let shared_instance = "shared-source";
        handle.observe_provider_source_health(&ProviderSourceHealthEvent {
            source: crate::provider_stream::ProviderSourceIdentity::new(
                crate::provider_stream::ProviderSourceId::YellowstoneGrpc,
                shared_instance,
            ),
            status: ProviderSourceHealthStatus::Reconnecting,
            reason: crate::provider_stream::ProviderSourceHealthReason::UpstreamProtocolFailure,
            message: "yellowstone stalled".to_owned(),
        });
        handle.observe_provider_source_health(&ProviderSourceHealthEvent {
            source: crate::provider_stream::ProviderSourceIdentity::new(
                crate::provider_stream::ProviderSourceId::LaserStream,
                shared_instance,
            ),
            status: ProviderSourceHealthStatus::Unhealthy,
            reason:
                crate::provider_stream::ProviderSourceHealthReason::UpstreamStreamClosedUnexpectedly,
            message: "laserstream closed".to_owned(),
        });

        let metrics = render_metrics(
            &handle,
            &PluginHost::builder().build(),
            &RuntimeExtensionHost::builder().build(),
            &DerivedStateHost::builder().build(),
        );

        assert!(metrics.contains("sof_provider_sources_reconnecting 1"));
        assert!(metrics.contains("sof_provider_sources_unhealthy 1"));
        assert!(metrics.contains(
            "sof_provider_source_status{source_kind=\"yellowstone_grpc\",source_instance=\"shared-source\",status=\"reconnecting\",reason=\"upstream_protocol_failure\"} 1"
        ));
        assert!(metrics.contains(
            "sof_provider_source_status{source_kind=\"laserstream\",source_instance=\"shared-source\",status=\"unhealthy\",reason=\"upstream_stream_closed_unexpectedly\"} 1"
        ));
    }

    #[test]
    fn readiness_groups_redundant_sources_by_kind() {
        let handle = RuntimeObservabilityHandle::default();
        handle.mark_live();
        handle.observe_provider_source_health(&ProviderSourceHealthEvent {
            source: crate::provider_stream::ProviderSourceIdentity::new(
                crate::provider_stream::ProviderSourceId::YellowstoneGrpc,
                "yellowstone-a",
            ),
            status: ProviderSourceHealthStatus::Reconnecting,
            reason: crate::provider_stream::ProviderSourceHealthReason::UpstreamProtocolFailure,
            message: "yellowstone reconnecting".to_owned(),
        });
        handle.observe_provider_source_health(&ProviderSourceHealthEvent {
            source: crate::provider_stream::ProviderSourceIdentity::new(
                crate::provider_stream::ProviderSourceId::YellowstoneGrpc,
                "yellowstone-b",
            ),
            status: ProviderSourceHealthStatus::Healthy,
            reason: crate::provider_stream::ProviderSourceHealthReason::SubscriptionAckReceived,
            message: "yellowstone healthy".to_owned(),
        });

        let metrics = render_metrics(
            &handle,
            &PluginHost::builder().build(),
            &RuntimeExtensionHost::builder().build(),
            &DerivedStateHost::builder().build(),
        );

        assert!(metrics.contains("sof_runtime_ready 1"));
        assert!(metrics.contains("sof_provider_sources_reconnecting 1"));
    }

    #[test]
    fn metrics_include_provider_capability_warning_state() {
        let handle = RuntimeObservabilityHandle::default();
        handle.mark_live();
        handle.observe_provider_capability_warning(
            "generic_provider",
            &[String::from("on_dataset"), String::from("on_raw_packet")],
        );
        let metrics = render_metrics(
            &handle,
            &PluginHost::builder().build(),
            &RuntimeExtensionHost::builder().build(),
            &DerivedStateHost::builder().build(),
        );

        assert!(metrics.contains("sof_provider_capability_degraded 1"));
        assert!(metrics.contains(
            "sof_provider_capability_unsupported_hook{mode=\"generic_provider\",hook=\"on_dataset\"} 1"
        ));
        assert!(metrics.contains(
            "sof_provider_capability_unsupported_hook{mode=\"generic_provider\",hook=\"on_raw_packet\"} 1"
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn service_serves_health_and_metrics_endpoints() {
        let service = RuntimeObservabilityService::start(
            "127.0.0.1:0".parse().expect("valid bind addr"),
            PluginHost::builder().build(),
            RuntimeExtensionHost::builder().build(),
            DerivedStateHost::builder().build(),
        )
        .await
        .expect("service should start");
        service.handle().mark_ready();

        let ready_response = request(service.local_addr(), READY_PATH).await;
        assert!(ready_response.starts_with("HTTP/1.1 200 OK"));
        assert!(ready_response.ends_with("ready\n"));

        let metrics_response = request(service.local_addr(), METRICS_PATH).await;
        assert!(metrics_response.starts_with("HTTP/1.1 200 OK"));
        assert!(metrics_response.contains("sof_runtime_live 1"));
        assert!(metrics_response.contains("sof_runtime_ready 1"));

        service.shutdown().await;
    }

    async fn request(addr: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(addr)
            .await
            .expect("request stream should connect");
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .expect("request should write");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .expect("response should read");
        response
    }
}
