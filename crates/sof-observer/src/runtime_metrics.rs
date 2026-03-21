use std::sync::atomic::{AtomicU64, Ordering};

/// One cache-line-aligned runtime counter to reduce false sharing across workers.
#[derive(Debug, Default)]
#[repr(align(64))]
struct CacheAlignedAtomicU64(AtomicU64);

impl CacheAlignedAtomicU64 {
    /// Loads the current counter value.
    fn load(&self, ordering: Ordering) -> u64 {
        self.0.load(ordering)
    }

    /// Stores a new counter value.
    fn store(&self, value: u64, ordering: Ordering) {
        self.0.store(value, ordering);
    }

    /// Increments the counter and returns the previous value.
    fn fetch_add(&self, value: u64, ordering: Ordering) -> u64 {
        self.0.fetch_add(value, ordering)
    }

    /// Performs a weak compare-exchange operation.
    fn compare_exchange_weak(
        &self,
        current: u64,
        new: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64> {
        self.0.compare_exchange_weak(current, new, success, failure)
    }
}

/// Snapshot of SOF runtime-stage counters intended for external observability.
#[derive(Debug, Clone, Copy, Default)]
pub struct ObserverRuntimeMetricsSnapshot {
    /// Total packets observed by the active ingest source.
    pub ingest_packets_seen_total: u64,
    /// Total packets forwarded from ingest into runtime processing.
    pub ingest_sent_packets_total: u64,
    /// Total packet batches forwarded from ingest into runtime processing.
    pub ingest_sent_batches_total: u64,
    /// Total packets dropped by ingest due to downstream backpressure.
    pub ingest_dropped_packets_total: u64,
    /// Total packet batches dropped by ingest due to downstream backpressure.
    pub ingest_dropped_batches_total: u64,
    /// Total kernel receive-queue overflow drops observed by ingest.
    pub ingest_rxq_ovfl_drops_total: u64,
    /// Age in milliseconds of the latest packet observed by ingest.
    pub ingest_last_packet_age_ms: u64,
    /// Total recovered data shreds accepted after FEC repair.
    pub recovered_data_packets_total: u64,
    /// Current aggregate dataset dispatch queue depth across dataset workers.
    pub dataset_queue_depth: u64,
    /// Current number of dataset jobs pending across dataset workers.
    pub dataset_jobs_pending: u64,
    /// Current number of packets pending in packet-worker queues.
    pub packet_worker_queue_depth: u64,
    /// Maximum packet-worker queue depth observed since startup.
    pub packet_worker_max_queue_depth: u64,
    /// Total packet-worker batches dropped due to full worker queues.
    pub packet_worker_dropped_batches_total: u64,
    /// Total packets dropped due to full packet-worker queues.
    pub packet_worker_dropped_packets_total: u64,
    /// Current shared semantic shred dedupe cache entry count.
    pub shred_dedupe_entries: u64,
    /// Maximum shared semantic shred dedupe cache entry count observed since startup.
    pub shred_dedupe_max_entries: u64,
    /// Current shared semantic shred dedupe eviction-queue depth.
    pub shred_dedupe_queue_depth: u64,
    /// Maximum shared semantic shred dedupe eviction-queue depth observed since startup.
    pub shred_dedupe_max_queue_depth: u64,
    /// Total shared semantic shred dedupe evictions caused by capacity pressure.
    pub shred_dedupe_capacity_evictions_total: u64,
    /// Total shared semantic shred dedupe evictions caused by expiry.
    pub shred_dedupe_expired_evictions_total: u64,
    /// Total duplicate semantic shreds dropped at the ingress boundary.
    pub shred_dedupe_ingress_duplicate_drops_total: u64,
    /// Total conflicting semantic shreds dropped at the ingress boundary.
    pub shred_dedupe_ingress_conflict_drops_total: u64,
    /// Total duplicate semantic shreds dropped at the canonical emission boundary.
    pub shred_dedupe_canonical_duplicate_drops_total: u64,
    /// Total conflicting semantic shreds dropped at the canonical emission boundary.
    pub shred_dedupe_canonical_conflict_drops_total: u64,
    /// Total completed datasets emitted from the reassembly stage.
    pub completed_datasets_total: u64,
    /// Total completed datasets successfully decoded into entries.
    pub decoded_datasets_total: u64,
    /// Total completed datasets that failed decode.
    pub decode_failed_datasets_total: u64,
    /// Total transactions decoded from completed datasets before downstream filtering.
    pub decoded_transactions_total: u64,
    /// Total dataset jobs enqueued for dataset workers.
    pub dataset_jobs_enqueued_total: u64,
    /// Total dataset jobs evicted from worker queues due to backpressure.
    pub dataset_queue_dropped_jobs_total: u64,
    /// Total dataset jobs started by dataset workers.
    pub dataset_jobs_started_total: u64,
    /// Total dataset jobs finished by dataset workers.
    pub dataset_jobs_completed_total: u64,
    /// Total decoded transaction events dropped before delivery to downstream consumers.
    pub tx_event_dropped_total: u64,
    /// Age in milliseconds of the most recent canonical shred observed by the runtime.
    pub latest_shred_age_ms: u64,
    /// Age in milliseconds of the most recent reconstructed dataset observed by the runtime.
    pub latest_dataset_age_ms: u64,
    /// Age in milliseconds since the gossip runtime last made progress.
    pub gossip_runtime_stall_age_ms: u64,
    /// Whether the dynamic repair stream is currently healthy.
    pub repair_dynamic_stream_healthy: bool,
    /// Current relay cache entry count.
    pub relay_cache_entries: u64,
    /// Total relay cache inserts since startup.
    pub relay_cache_inserts_total: u64,
    /// Total relay cache replacements since startup.
    pub relay_cache_replacements_total: u64,
    /// Total relay cache evictions since startup.
    pub relay_cache_evictions_total: u64,
    /// Current UDP relay peer candidate count after filtering.
    pub udp_relay_candidates: u64,
    /// Current UDP relay peer count selected for forwarding.
    pub udp_relay_peers: u64,
    /// Total UDP relay refresh cycles since startup.
    pub udp_relay_refreshes_total: u64,
    /// Total packets forwarded by the UDP relay.
    pub udp_relay_forwarded_packets_total: u64,
    /// Total UDP relay send attempts.
    pub udp_relay_send_attempts_total: u64,
    /// Total UDP relay send errors.
    pub udp_relay_send_errors_total: u64,
    /// Total UDP relay packets dropped by rate limiting.
    pub udp_relay_rate_limited_packets_total: u64,
    /// Total packets filtered out before UDP relay forwarding.
    pub udp_relay_source_filtered_packets_total: u64,
    /// Total UDP relay backoff activations.
    pub udp_relay_backoff_events_total: u64,
    /// Total packets dropped due to UDP relay backoff.
    pub udp_relay_backoff_drops_total: u64,
    /// Total repair requests considered by the runtime.
    pub repair_requests_total: u64,
    /// Total repair requests enqueued for the repair driver.
    pub repair_requests_enqueued_total: u64,
    /// Total repair requests successfully sent.
    pub repair_requests_sent_total: u64,
    /// Total repair requests skipped because no peer was available.
    pub repair_requests_no_peer_total: u64,
    /// Total repair request send errors.
    pub repair_request_errors_total: u64,
    /// Total repair requests dropped before enqueue due to queue pressure.
    pub repair_request_queue_drops_total: u64,
    /// Total repair requests skipped because an outstanding request already covered the need.
    pub repair_requests_skipped_outstanding_total: u64,
    /// Current outstanding repair request count.
    pub repair_outstanding_entries: u64,
    /// Total outstanding repair requests purged by timeout.
    pub repair_outstanding_purged_total: u64,
    /// Total outstanding repair requests cleared when data arrived.
    pub repair_outstanding_cleared_on_receive_total: u64,
    /// Total repair response ping messages sent.
    pub repair_response_pings_total: u64,
    /// Total repair response ping send errors.
    pub repair_response_ping_errors_total: u64,
    /// Total repair pings dropped due to queue pressure.
    pub repair_ping_queue_drops_total: u64,
    /// Total incoming repair serve requests enqueued.
    pub repair_serve_requests_enqueued_total: u64,
    /// Total incoming repair serve requests handled.
    pub repair_serve_requests_handled_total: u64,
    /// Total repair serve responses sent.
    pub repair_serve_responses_sent_total: u64,
    /// Total repair serve cache misses.
    pub repair_serve_cache_misses_total: u64,
    /// Total repair serve drops due to aggregate rate limiting.
    pub repair_serve_rate_limited_total: u64,
    /// Total repair serve drops due to per-peer rate limiting.
    pub repair_serve_rate_limited_peer_total: u64,
    /// Total repair serve bytes dropped due to byte budgeting.
    pub repair_serve_rate_limited_bytes_total: u64,
    /// Total repair serve response errors.
    pub repair_serve_errors_total: u64,
    /// Total repair serve queue drops.
    pub repair_serve_queue_drops_total: u64,
    /// Total repair source hints enqueued.
    pub repair_source_hint_enqueued_total: u64,
    /// Total repair source hints dropped during enqueue.
    pub repair_source_hint_drops_total: u64,
    /// Total repair source hints dropped by the hint buffer.
    pub repair_source_hint_buffer_drops_total: u64,
    /// Current repair peer count known to the runtime.
    pub repair_peer_total: u64,
    /// Current active repair peer count after runtime filtering.
    pub repair_peer_active: u64,
    /// Total gossip-runtime switch attempts.
    pub gossip_runtime_switch_attempts_total: u64,
    /// Total successful gossip-runtime switches.
    pub gossip_runtime_switch_successes_total: u64,
    /// Total failed gossip-runtime switches.
    pub gossip_runtime_switch_failures_total: u64,
}

/// Total packets observed by the active ingest source.
static INGEST_PACKETS_SEEN_TOTAL: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total packets forwarded from ingest into runtime processing.
static INGEST_SENT_PACKETS_TOTAL: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total packet batches forwarded from ingest into runtime processing.
static INGEST_SENT_BATCHES_TOTAL: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total packets dropped by ingest due to downstream backpressure.
static INGEST_DROPPED_PACKETS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total packet batches dropped by ingest due to downstream backpressure.
static INGEST_DROPPED_BATCHES_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total kernel receive-queue overflow drops observed by ingest.
static INGEST_RXQ_OVFL_DROPS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Age in milliseconds of the latest packet observed by ingest.
static INGEST_LAST_PACKET_AGE_MS: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total recovered data shreds accepted after FEC repair.
static RECOVERED_DATA_PACKETS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Current aggregate dataset dispatch queue depth across dataset workers.
static DATASET_QUEUE_DEPTH: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Current number of dataset jobs pending across dataset workers.
static DATASET_JOBS_PENDING: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Current aggregate queue depth across packet workers.
static PACKET_WORKER_QUEUE_DEPTH: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Maximum aggregate packet-worker queue depth observed since startup.
static PACKET_WORKER_MAX_QUEUE_DEPTH: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total packet-worker batches dropped due to queue pressure.
static PACKET_WORKER_DROPPED_BATCHES_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total packets dropped due to packet-worker queue pressure.
static PACKET_WORKER_DROPPED_PACKETS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Current shared semantic shred dedupe cache entry count.
static SHRED_DEDUPE_ENTRIES: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Maximum shared semantic shred dedupe cache entry count observed since startup.
static SHRED_DEDUPE_MAX_ENTRIES: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Current shared semantic shred dedupe eviction-queue depth.
static SHRED_DEDUPE_QUEUE_DEPTH: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Maximum shared semantic shred dedupe eviction-queue depth observed since startup.
static SHRED_DEDUPE_MAX_QUEUE_DEPTH: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total shared semantic shred dedupe evictions caused by capacity pressure.
static SHRED_DEDUPE_CAPACITY_EVICTIONS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total shared semantic shred dedupe evictions caused by expiry.
static SHRED_DEDUPE_EXPIRED_EVICTIONS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total duplicate semantic shreds dropped at the ingress boundary.
static SHRED_DEDUPE_INGRESS_DUPLICATE_DROPS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total conflicting semantic shreds dropped at the ingress boundary.
static SHRED_DEDUPE_INGRESS_CONFLICT_DROPS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total duplicate semantic shreds dropped at the canonical emission boundary.
static SHRED_DEDUPE_CANONICAL_DUPLICATE_DROPS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total conflicting semantic shreds dropped at the canonical emission boundary.
static SHRED_DEDUPE_CANONICAL_CONFLICT_DROPS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total completed datasets emitted from reassembly.
static COMPLETED_DATASETS_TOTAL: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total datasets successfully decoded into entries.
static DECODED_DATASETS_TOTAL: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total datasets that failed decode.
static DECODE_FAILED_DATASETS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total decoded transactions observed from dataset payloads.
static DECODED_TRANSACTIONS_TOTAL: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total dataset jobs enqueued for dataset workers.
static DATASET_JOBS_ENQUEUED_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total dataset jobs evicted from queues due to backpressure.
static DATASET_QUEUE_DROPPED_JOBS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total dataset jobs started by dataset workers.
static DATASET_JOBS_STARTED_TOTAL: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total dataset jobs completed by dataset workers.
static DATASET_JOBS_COMPLETED_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total transaction events dropped before downstream delivery.
static TX_EVENT_DROPPED_TOTAL: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Age in milliseconds of the most recent canonical shred observed by the runtime.
static LATEST_SHRED_AGE_MS: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Age in milliseconds of the most recent reconstructed dataset observed by the runtime.
static LATEST_DATASET_AGE_MS: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Age in milliseconds since the gossip runtime last made progress.
static GOSSIP_RUNTIME_STALL_AGE_MS: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Whether the dynamic repair stream is currently healthy.
static REPAIR_DYNAMIC_STREAM_HEALTHY: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Current relay cache entry count.
static RELAY_CACHE_ENTRIES: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total relay cache inserts since startup.
static RELAY_CACHE_INSERTS_TOTAL: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total relay cache replacements since startup.
static RELAY_CACHE_REPLACEMENTS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total relay cache evictions since startup.
static RELAY_CACHE_EVICTIONS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Current UDP relay peer candidate count after filtering.
static UDP_RELAY_CANDIDATES: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Current UDP relay peer count selected for forwarding.
static UDP_RELAY_PEERS: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total UDP relay refresh cycles since startup.
static UDP_RELAY_REFRESHES_TOTAL: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total packets forwarded by the UDP relay.
static UDP_RELAY_FORWARDED_PACKETS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total UDP relay send attempts.
static UDP_RELAY_SEND_ATTEMPTS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total UDP relay send errors.
static UDP_RELAY_SEND_ERRORS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total UDP relay packets dropped by rate limiting.
static UDP_RELAY_RATE_LIMITED_PACKETS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total packets filtered out before UDP relay forwarding.
static UDP_RELAY_SOURCE_FILTERED_PACKETS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total UDP relay backoff activations.
static UDP_RELAY_BACKOFF_EVENTS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total packets dropped due to UDP relay backoff.
static UDP_RELAY_BACKOFF_DROPS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair requests considered by the runtime.
static REPAIR_REQUESTS_TOTAL: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair requests enqueued for the repair driver.
static REPAIR_REQUESTS_ENQUEUED_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair requests successfully sent.
static REPAIR_REQUESTS_SENT_TOTAL: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair requests skipped because no peer was available.
static REPAIR_REQUESTS_NO_PEER_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair request send errors.
static REPAIR_REQUEST_ERRORS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair requests dropped before enqueue due to queue pressure.
static REPAIR_REQUEST_QUEUE_DROPS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair requests skipped because an outstanding request already covered the need.
static REPAIR_REQUESTS_SKIPPED_OUTSTANDING_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Current outstanding repair request count.
static REPAIR_OUTSTANDING_ENTRIES: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total outstanding repair requests purged by timeout.
static REPAIR_OUTSTANDING_PURGED_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total outstanding repair requests cleared when data arrived.
static REPAIR_OUTSTANDING_CLEARED_ON_RECEIVE_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair response ping messages sent.
static REPAIR_RESPONSE_PINGS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair response ping send errors.
static REPAIR_RESPONSE_PING_ERRORS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair pings dropped due to queue pressure.
static REPAIR_PING_QUEUE_DROPS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total incoming repair serve requests enqueued.
static REPAIR_SERVE_REQUESTS_ENQUEUED_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total incoming repair serve requests handled.
static REPAIR_SERVE_REQUESTS_HANDLED_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair serve responses sent.
static REPAIR_SERVE_RESPONSES_SENT_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair serve cache misses.
static REPAIR_SERVE_CACHE_MISSES_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair serve drops due to aggregate rate limiting.
static REPAIR_SERVE_RATE_LIMITED_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair serve drops due to per-peer rate limiting.
static REPAIR_SERVE_RATE_LIMITED_PEER_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair serve bytes dropped due to byte budgeting.
static REPAIR_SERVE_RATE_LIMITED_BYTES_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair serve response errors.
static REPAIR_SERVE_ERRORS_TOTAL: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair serve queue drops.
static REPAIR_SERVE_QUEUE_DROPS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair source hints enqueued.
static REPAIR_SOURCE_HINT_ENQUEUED_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair source hints dropped during enqueue.
static REPAIR_SOURCE_HINT_DROPS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total repair source hints dropped by the hint buffer.
static REPAIR_SOURCE_HINT_BUFFER_DROPS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Current repair peer count known to the runtime.
static REPAIR_PEER_TOTAL: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Current active repair peer count after runtime filtering.
static REPAIR_PEER_ACTIVE: CacheAlignedAtomicU64 = CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total gossip-runtime switch attempts.
static GOSSIP_RUNTIME_SWITCH_ATTEMPTS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total successful gossip-runtime switches.
static GOSSIP_RUNTIME_SWITCH_SUCCESSES_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
/// Total failed gossip-runtime switches.
static GOSSIP_RUNTIME_SWITCH_FAILURES_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));

/// Returns the latest SOF runtime-stage counter snapshot.
#[must_use]
pub fn snapshot() -> ObserverRuntimeMetricsSnapshot {
    ObserverRuntimeMetricsSnapshot {
        ingest_packets_seen_total: INGEST_PACKETS_SEEN_TOTAL.load(Ordering::Relaxed),
        ingest_sent_packets_total: INGEST_SENT_PACKETS_TOTAL.load(Ordering::Relaxed),
        ingest_sent_batches_total: INGEST_SENT_BATCHES_TOTAL.load(Ordering::Relaxed),
        ingest_dropped_packets_total: INGEST_DROPPED_PACKETS_TOTAL.load(Ordering::Relaxed),
        ingest_dropped_batches_total: INGEST_DROPPED_BATCHES_TOTAL.load(Ordering::Relaxed),
        ingest_rxq_ovfl_drops_total: INGEST_RXQ_OVFL_DROPS_TOTAL.load(Ordering::Relaxed),
        ingest_last_packet_age_ms: INGEST_LAST_PACKET_AGE_MS.load(Ordering::Relaxed),
        recovered_data_packets_total: RECOVERED_DATA_PACKETS_TOTAL.load(Ordering::Relaxed),
        dataset_queue_depth: DATASET_QUEUE_DEPTH.load(Ordering::Relaxed),
        dataset_jobs_pending: DATASET_JOBS_PENDING.load(Ordering::Relaxed),
        packet_worker_queue_depth: PACKET_WORKER_QUEUE_DEPTH.load(Ordering::Relaxed),
        packet_worker_max_queue_depth: PACKET_WORKER_MAX_QUEUE_DEPTH.load(Ordering::Relaxed),
        packet_worker_dropped_batches_total: PACKET_WORKER_DROPPED_BATCHES_TOTAL
            .load(Ordering::Relaxed),
        packet_worker_dropped_packets_total: PACKET_WORKER_DROPPED_PACKETS_TOTAL
            .load(Ordering::Relaxed),
        shred_dedupe_entries: SHRED_DEDUPE_ENTRIES.load(Ordering::Relaxed),
        shred_dedupe_max_entries: SHRED_DEDUPE_MAX_ENTRIES.load(Ordering::Relaxed),
        shred_dedupe_queue_depth: SHRED_DEDUPE_QUEUE_DEPTH.load(Ordering::Relaxed),
        shred_dedupe_max_queue_depth: SHRED_DEDUPE_MAX_QUEUE_DEPTH.load(Ordering::Relaxed),
        shred_dedupe_capacity_evictions_total: SHRED_DEDUPE_CAPACITY_EVICTIONS_TOTAL
            .load(Ordering::Relaxed),
        shred_dedupe_expired_evictions_total: SHRED_DEDUPE_EXPIRED_EVICTIONS_TOTAL
            .load(Ordering::Relaxed),
        shred_dedupe_ingress_duplicate_drops_total: SHRED_DEDUPE_INGRESS_DUPLICATE_DROPS_TOTAL
            .load(Ordering::Relaxed),
        shred_dedupe_ingress_conflict_drops_total: SHRED_DEDUPE_INGRESS_CONFLICT_DROPS_TOTAL
            .load(Ordering::Relaxed),
        shred_dedupe_canonical_duplicate_drops_total: SHRED_DEDUPE_CANONICAL_DUPLICATE_DROPS_TOTAL
            .load(Ordering::Relaxed),
        shred_dedupe_canonical_conflict_drops_total: SHRED_DEDUPE_CANONICAL_CONFLICT_DROPS_TOTAL
            .load(Ordering::Relaxed),
        completed_datasets_total: COMPLETED_DATASETS_TOTAL.load(Ordering::Relaxed),
        decoded_datasets_total: DECODED_DATASETS_TOTAL.load(Ordering::Relaxed),
        decode_failed_datasets_total: DECODE_FAILED_DATASETS_TOTAL.load(Ordering::Relaxed),
        decoded_transactions_total: DECODED_TRANSACTIONS_TOTAL.load(Ordering::Relaxed),
        dataset_jobs_enqueued_total: DATASET_JOBS_ENQUEUED_TOTAL.load(Ordering::Relaxed),
        dataset_queue_dropped_jobs_total: DATASET_QUEUE_DROPPED_JOBS_TOTAL.load(Ordering::Relaxed),
        dataset_jobs_started_total: DATASET_JOBS_STARTED_TOTAL.load(Ordering::Relaxed),
        dataset_jobs_completed_total: DATASET_JOBS_COMPLETED_TOTAL.load(Ordering::Relaxed),
        tx_event_dropped_total: TX_EVENT_DROPPED_TOTAL.load(Ordering::Relaxed),
        latest_shred_age_ms: LATEST_SHRED_AGE_MS.load(Ordering::Relaxed),
        latest_dataset_age_ms: LATEST_DATASET_AGE_MS.load(Ordering::Relaxed),
        gossip_runtime_stall_age_ms: GOSSIP_RUNTIME_STALL_AGE_MS.load(Ordering::Relaxed),
        repair_dynamic_stream_healthy: REPAIR_DYNAMIC_STREAM_HEALTHY.load(Ordering::Relaxed) > 0,
        relay_cache_entries: RELAY_CACHE_ENTRIES.load(Ordering::Relaxed),
        relay_cache_inserts_total: RELAY_CACHE_INSERTS_TOTAL.load(Ordering::Relaxed),
        relay_cache_replacements_total: RELAY_CACHE_REPLACEMENTS_TOTAL.load(Ordering::Relaxed),
        relay_cache_evictions_total: RELAY_CACHE_EVICTIONS_TOTAL.load(Ordering::Relaxed),
        udp_relay_candidates: UDP_RELAY_CANDIDATES.load(Ordering::Relaxed),
        udp_relay_peers: UDP_RELAY_PEERS.load(Ordering::Relaxed),
        udp_relay_refreshes_total: UDP_RELAY_REFRESHES_TOTAL.load(Ordering::Relaxed),
        udp_relay_forwarded_packets_total: UDP_RELAY_FORWARDED_PACKETS_TOTAL
            .load(Ordering::Relaxed),
        udp_relay_send_attempts_total: UDP_RELAY_SEND_ATTEMPTS_TOTAL.load(Ordering::Relaxed),
        udp_relay_send_errors_total: UDP_RELAY_SEND_ERRORS_TOTAL.load(Ordering::Relaxed),
        udp_relay_rate_limited_packets_total: UDP_RELAY_RATE_LIMITED_PACKETS_TOTAL
            .load(Ordering::Relaxed),
        udp_relay_source_filtered_packets_total: UDP_RELAY_SOURCE_FILTERED_PACKETS_TOTAL
            .load(Ordering::Relaxed),
        udp_relay_backoff_events_total: UDP_RELAY_BACKOFF_EVENTS_TOTAL.load(Ordering::Relaxed),
        udp_relay_backoff_drops_total: UDP_RELAY_BACKOFF_DROPS_TOTAL.load(Ordering::Relaxed),
        repair_requests_total: REPAIR_REQUESTS_TOTAL.load(Ordering::Relaxed),
        repair_requests_enqueued_total: REPAIR_REQUESTS_ENQUEUED_TOTAL.load(Ordering::Relaxed),
        repair_requests_sent_total: REPAIR_REQUESTS_SENT_TOTAL.load(Ordering::Relaxed),
        repair_requests_no_peer_total: REPAIR_REQUESTS_NO_PEER_TOTAL.load(Ordering::Relaxed),
        repair_request_errors_total: REPAIR_REQUEST_ERRORS_TOTAL.load(Ordering::Relaxed),
        repair_request_queue_drops_total: REPAIR_REQUEST_QUEUE_DROPS_TOTAL.load(Ordering::Relaxed),
        repair_requests_skipped_outstanding_total: REPAIR_REQUESTS_SKIPPED_OUTSTANDING_TOTAL
            .load(Ordering::Relaxed),
        repair_outstanding_entries: REPAIR_OUTSTANDING_ENTRIES.load(Ordering::Relaxed),
        repair_outstanding_purged_total: REPAIR_OUTSTANDING_PURGED_TOTAL.load(Ordering::Relaxed),
        repair_outstanding_cleared_on_receive_total: REPAIR_OUTSTANDING_CLEARED_ON_RECEIVE_TOTAL
            .load(Ordering::Relaxed),
        repair_response_pings_total: REPAIR_RESPONSE_PINGS_TOTAL.load(Ordering::Relaxed),
        repair_response_ping_errors_total: REPAIR_RESPONSE_PING_ERRORS_TOTAL
            .load(Ordering::Relaxed),
        repair_ping_queue_drops_total: REPAIR_PING_QUEUE_DROPS_TOTAL.load(Ordering::Relaxed),
        repair_serve_requests_enqueued_total: REPAIR_SERVE_REQUESTS_ENQUEUED_TOTAL
            .load(Ordering::Relaxed),
        repair_serve_requests_handled_total: REPAIR_SERVE_REQUESTS_HANDLED_TOTAL
            .load(Ordering::Relaxed),
        repair_serve_responses_sent_total: REPAIR_SERVE_RESPONSES_SENT_TOTAL
            .load(Ordering::Relaxed),
        repair_serve_cache_misses_total: REPAIR_SERVE_CACHE_MISSES_TOTAL.load(Ordering::Relaxed),
        repair_serve_rate_limited_total: REPAIR_SERVE_RATE_LIMITED_TOTAL.load(Ordering::Relaxed),
        repair_serve_rate_limited_peer_total: REPAIR_SERVE_RATE_LIMITED_PEER_TOTAL
            .load(Ordering::Relaxed),
        repair_serve_rate_limited_bytes_total: REPAIR_SERVE_RATE_LIMITED_BYTES_TOTAL
            .load(Ordering::Relaxed),
        repair_serve_errors_total: REPAIR_SERVE_ERRORS_TOTAL.load(Ordering::Relaxed),
        repair_serve_queue_drops_total: REPAIR_SERVE_QUEUE_DROPS_TOTAL.load(Ordering::Relaxed),
        repair_source_hint_enqueued_total: REPAIR_SOURCE_HINT_ENQUEUED_TOTAL
            .load(Ordering::Relaxed),
        repair_source_hint_drops_total: REPAIR_SOURCE_HINT_DROPS_TOTAL.load(Ordering::Relaxed),
        repair_source_hint_buffer_drops_total: REPAIR_SOURCE_HINT_BUFFER_DROPS_TOTAL
            .load(Ordering::Relaxed),
        repair_peer_total: REPAIR_PEER_TOTAL.load(Ordering::Relaxed),
        repair_peer_active: REPAIR_PEER_ACTIVE.load(Ordering::Relaxed),
        gossip_runtime_switch_attempts_total: GOSSIP_RUNTIME_SWITCH_ATTEMPTS_TOTAL
            .load(Ordering::Relaxed),
        gossip_runtime_switch_successes_total: GOSSIP_RUNTIME_SWITCH_SUCCESSES_TOTAL
            .load(Ordering::Relaxed),
        gossip_runtime_switch_failures_total: GOSSIP_RUNTIME_SWITCH_FAILURES_TOTAL
            .load(Ordering::Relaxed),
    }
}

/// Publishes the latest ingest-side totals and freshness gauge.
pub(crate) fn set_ingest_metrics(
    packets_seen_total: u64,
    sent_packets_total: u64,
    sent_batches_total: u64,
    dropped_packets_total: u64,
    dropped_batches_total: u64,
    rxq_ovfl_drops_total: u64,
    last_packet_age_ms: u64,
) {
    INGEST_PACKETS_SEEN_TOTAL.store(packets_seen_total, Ordering::Relaxed);
    INGEST_SENT_PACKETS_TOTAL.store(sent_packets_total, Ordering::Relaxed);
    INGEST_SENT_BATCHES_TOTAL.store(sent_batches_total, Ordering::Relaxed);
    INGEST_DROPPED_PACKETS_TOTAL.store(dropped_packets_total, Ordering::Relaxed);
    INGEST_DROPPED_BATCHES_TOTAL.store(dropped_batches_total, Ordering::Relaxed);
    INGEST_RXQ_OVFL_DROPS_TOTAL.store(rxq_ovfl_drops_total, Ordering::Relaxed);
    INGEST_LAST_PACKET_AGE_MS.store(last_packet_age_ms, Ordering::Relaxed);
}

/// Adds recovered-data packets accepted after FEC repair.
pub(crate) fn observe_recovered_data_packets(count: u64) {
    RECOVERED_DATA_PACKETS_TOTAL.fetch_add(count, Ordering::Relaxed);
}

/// Publishes the latest dataset dispatch queue depth and pending-job gauge.
pub(crate) fn set_dataset_dispatch_metrics(queue_depth: u64, jobs_pending: u64) {
    DATASET_QUEUE_DEPTH.store(queue_depth, Ordering::Relaxed);
    DATASET_JOBS_PENDING.store(jobs_pending, Ordering::Relaxed);
}

/// Sets the current aggregate packet-worker queue depth.
pub(crate) fn set_packet_worker_queue_depth(depth: u64) {
    PACKET_WORKER_QUEUE_DEPTH.store(depth, Ordering::Relaxed);
}

/// Raises the maximum observed packet-worker queue depth when `depth` exceeds it.
pub(crate) fn observe_packet_worker_max_queue_depth(depth: u64) {
    let mut current = PACKET_WORKER_MAX_QUEUE_DEPTH.load(Ordering::Relaxed);
    while depth > current {
        match PACKET_WORKER_MAX_QUEUE_DEPTH.compare_exchange_weak(
            current,
            depth,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

/// Adds packet-worker queue drops aggregated by dropped batches and packets.
pub(crate) fn observe_packet_worker_queue_drops(batches: u64, packets: u64) {
    PACKET_WORKER_DROPPED_BATCHES_TOTAL.fetch_add(batches, Ordering::Relaxed);
    PACKET_WORKER_DROPPED_PACKETS_TOTAL.fetch_add(packets, Ordering::Relaxed);
}

/// Publishes the latest shared semantic shred dedupe cache occupancy.
pub(crate) fn set_shred_dedupe_metrics(
    entries: u64,
    max_entries: u64,
    queue_depth: u64,
    max_queue_depth: u64,
) {
    SHRED_DEDUPE_ENTRIES.store(entries, Ordering::Relaxed);
    SHRED_DEDUPE_QUEUE_DEPTH.store(queue_depth, Ordering::Relaxed);
    observe_max_counter(&SHRED_DEDUPE_MAX_ENTRIES, max_entries);
    observe_max_counter(&SHRED_DEDUPE_MAX_QUEUE_DEPTH, max_queue_depth);
}

/// Adds semantic shred dedupe drops for one pipeline stage.
pub(crate) fn observe_shred_dedupe_drops(
    stage: crate::app::state::ShredDedupeStage,
    duplicates: u64,
    conflicts: u64,
) {
    match stage {
        crate::app::state::ShredDedupeStage::Ingress => {
            SHRED_DEDUPE_INGRESS_DUPLICATE_DROPS_TOTAL.fetch_add(duplicates, Ordering::Relaxed);
            SHRED_DEDUPE_INGRESS_CONFLICT_DROPS_TOTAL.fetch_add(conflicts, Ordering::Relaxed);
        }
        crate::app::state::ShredDedupeStage::Canonical => {
            SHRED_DEDUPE_CANONICAL_DUPLICATE_DROPS_TOTAL.fetch_add(duplicates, Ordering::Relaxed);
            SHRED_DEDUPE_CANONICAL_CONFLICT_DROPS_TOTAL.fetch_add(conflicts, Ordering::Relaxed);
        }
    }
}

/// Publishes the latest shared semantic shred dedupe eviction totals.
pub(crate) fn set_shred_dedupe_evictions(
    capacity_evictions_total: u64,
    expired_evictions_total: u64,
) {
    SHRED_DEDUPE_CAPACITY_EVICTIONS_TOTAL.store(capacity_evictions_total, Ordering::Relaxed);
    SHRED_DEDUPE_EXPIRED_EVICTIONS_TOTAL.store(expired_evictions_total, Ordering::Relaxed);
}

/// Adds completed datasets emitted from the reassembly stage.
pub(crate) fn observe_completed_datasets(count: u64) {
    COMPLETED_DATASETS_TOTAL.fetch_add(count, Ordering::Relaxed);
}

/// Records one successfully decoded dataset and its decoded transaction count.
pub(crate) fn observe_decoded_dataset(tx_count: u64) {
    DECODED_DATASETS_TOTAL.fetch_add(1, Ordering::Relaxed);
    DECODED_TRANSACTIONS_TOTAL.fetch_add(tx_count, Ordering::Relaxed);
}

/// Records one dataset decode failure.
pub(crate) fn observe_decode_failed_dataset() {
    DECODE_FAILED_DATASETS_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Adds enqueued dataset jobs.
pub(crate) fn observe_dataset_jobs_enqueued(count: u64) {
    DATASET_JOBS_ENQUEUED_TOTAL.fetch_add(count, Ordering::Relaxed);
}

/// Adds dataset jobs dropped from worker queues.
pub(crate) fn observe_dataset_queue_dropped_jobs(count: u64) {
    DATASET_QUEUE_DROPPED_JOBS_TOTAL.fetch_add(count, Ordering::Relaxed);
}

/// Records one dataset job start.
pub(crate) fn observe_dataset_job_started() {
    DATASET_JOBS_STARTED_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Records one dataset job completion.
pub(crate) fn observe_dataset_job_completed() {
    DATASET_JOBS_COMPLETED_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// Adds dropped transaction events.
pub(crate) fn observe_tx_event_drops(count: u64) {
    TX_EVENT_DROPPED_TOTAL.fetch_add(count, Ordering::Relaxed);
}

/// Publishes the latest runtime freshness and repair-health gauges.
pub(crate) fn set_runtime_health_metrics(
    latest_shred_age_ms: u64,
    latest_dataset_age_ms: u64,
    gossip_runtime_stall_age_ms: u64,
    repair_dynamic_stream_healthy: bool,
) {
    LATEST_SHRED_AGE_MS.store(latest_shred_age_ms, Ordering::Relaxed);
    LATEST_DATASET_AGE_MS.store(latest_dataset_age_ms, Ordering::Relaxed);
    GOSSIP_RUNTIME_STALL_AGE_MS.store(gossip_runtime_stall_age_ms, Ordering::Relaxed);
    REPAIR_DYNAMIC_STREAM_HEALTHY
        .store(u64::from(repair_dynamic_stream_healthy), Ordering::Relaxed);
}

/// Publishes the latest relay, repair, and gossip-switch operational counters.
#[allow(clippy::too_many_arguments)]
pub(crate) fn set_network_operability_metrics(
    relay_cache_entries: u64,
    relay_cache_inserts_total: u64,
    relay_cache_replacements_total: u64,
    relay_cache_evictions_total: u64,
    udp_relay_candidates: u64,
    udp_relay_peers: u64,
    udp_relay_refreshes_total: u64,
    udp_relay_forwarded_packets_total: u64,
    udp_relay_send_attempts_total: u64,
    udp_relay_send_errors_total: u64,
    udp_relay_rate_limited_packets_total: u64,
    udp_relay_source_filtered_packets_total: u64,
    udp_relay_backoff_events_total: u64,
    udp_relay_backoff_drops_total: u64,
    repair_requests_total: u64,
    repair_requests_enqueued_total: u64,
    repair_requests_sent_total: u64,
    repair_requests_no_peer_total: u64,
    repair_request_errors_total: u64,
    repair_request_queue_drops_total: u64,
    repair_requests_skipped_outstanding_total: u64,
    repair_outstanding_entries: u64,
    repair_outstanding_purged_total: u64,
    repair_outstanding_cleared_on_receive_total: u64,
    repair_response_pings_total: u64,
    repair_response_ping_errors_total: u64,
    repair_ping_queue_drops_total: u64,
    repair_serve_requests_enqueued_total: u64,
    repair_serve_requests_handled_total: u64,
    repair_serve_responses_sent_total: u64,
    repair_serve_cache_misses_total: u64,
    repair_serve_rate_limited_total: u64,
    repair_serve_rate_limited_peer_total: u64,
    repair_serve_rate_limited_bytes_total: u64,
    repair_serve_errors_total: u64,
    repair_serve_queue_drops_total: u64,
    repair_source_hint_enqueued_total: u64,
    repair_source_hint_drops_total: u64,
    repair_source_hint_buffer_drops_total: u64,
    repair_peer_total: u64,
    repair_peer_active: u64,
    gossip_runtime_switch_attempts_total: u64,
    gossip_runtime_switch_successes_total: u64,
    gossip_runtime_switch_failures_total: u64,
) {
    RELAY_CACHE_ENTRIES.store(relay_cache_entries, Ordering::Relaxed);
    RELAY_CACHE_INSERTS_TOTAL.store(relay_cache_inserts_total, Ordering::Relaxed);
    RELAY_CACHE_REPLACEMENTS_TOTAL.store(relay_cache_replacements_total, Ordering::Relaxed);
    RELAY_CACHE_EVICTIONS_TOTAL.store(relay_cache_evictions_total, Ordering::Relaxed);
    UDP_RELAY_CANDIDATES.store(udp_relay_candidates, Ordering::Relaxed);
    UDP_RELAY_PEERS.store(udp_relay_peers, Ordering::Relaxed);
    UDP_RELAY_REFRESHES_TOTAL.store(udp_relay_refreshes_total, Ordering::Relaxed);
    UDP_RELAY_FORWARDED_PACKETS_TOTAL.store(udp_relay_forwarded_packets_total, Ordering::Relaxed);
    UDP_RELAY_SEND_ATTEMPTS_TOTAL.store(udp_relay_send_attempts_total, Ordering::Relaxed);
    UDP_RELAY_SEND_ERRORS_TOTAL.store(udp_relay_send_errors_total, Ordering::Relaxed);
    UDP_RELAY_RATE_LIMITED_PACKETS_TOTAL
        .store(udp_relay_rate_limited_packets_total, Ordering::Relaxed);
    UDP_RELAY_SOURCE_FILTERED_PACKETS_TOTAL
        .store(udp_relay_source_filtered_packets_total, Ordering::Relaxed);
    UDP_RELAY_BACKOFF_EVENTS_TOTAL.store(udp_relay_backoff_events_total, Ordering::Relaxed);
    UDP_RELAY_BACKOFF_DROPS_TOTAL.store(udp_relay_backoff_drops_total, Ordering::Relaxed);
    REPAIR_REQUESTS_TOTAL.store(repair_requests_total, Ordering::Relaxed);
    REPAIR_REQUESTS_ENQUEUED_TOTAL.store(repair_requests_enqueued_total, Ordering::Relaxed);
    REPAIR_REQUESTS_SENT_TOTAL.store(repair_requests_sent_total, Ordering::Relaxed);
    REPAIR_REQUESTS_NO_PEER_TOTAL.store(repair_requests_no_peer_total, Ordering::Relaxed);
    REPAIR_REQUEST_ERRORS_TOTAL.store(repair_request_errors_total, Ordering::Relaxed);
    REPAIR_REQUEST_QUEUE_DROPS_TOTAL.store(repair_request_queue_drops_total, Ordering::Relaxed);
    REPAIR_REQUESTS_SKIPPED_OUTSTANDING_TOTAL
        .store(repair_requests_skipped_outstanding_total, Ordering::Relaxed);
    REPAIR_OUTSTANDING_ENTRIES.store(repair_outstanding_entries, Ordering::Relaxed);
    REPAIR_OUTSTANDING_PURGED_TOTAL.store(repair_outstanding_purged_total, Ordering::Relaxed);
    REPAIR_OUTSTANDING_CLEARED_ON_RECEIVE_TOTAL.store(
        repair_outstanding_cleared_on_receive_total,
        Ordering::Relaxed,
    );
    REPAIR_RESPONSE_PINGS_TOTAL.store(repair_response_pings_total, Ordering::Relaxed);
    REPAIR_RESPONSE_PING_ERRORS_TOTAL.store(repair_response_ping_errors_total, Ordering::Relaxed);
    REPAIR_PING_QUEUE_DROPS_TOTAL.store(repair_ping_queue_drops_total, Ordering::Relaxed);
    REPAIR_SERVE_REQUESTS_ENQUEUED_TOTAL
        .store(repair_serve_requests_enqueued_total, Ordering::Relaxed);
    REPAIR_SERVE_REQUESTS_HANDLED_TOTAL
        .store(repair_serve_requests_handled_total, Ordering::Relaxed);
    REPAIR_SERVE_RESPONSES_SENT_TOTAL.store(repair_serve_responses_sent_total, Ordering::Relaxed);
    REPAIR_SERVE_CACHE_MISSES_TOTAL.store(repair_serve_cache_misses_total, Ordering::Relaxed);
    REPAIR_SERVE_RATE_LIMITED_TOTAL.store(repair_serve_rate_limited_total, Ordering::Relaxed);
    REPAIR_SERVE_RATE_LIMITED_PEER_TOTAL
        .store(repair_serve_rate_limited_peer_total, Ordering::Relaxed);
    REPAIR_SERVE_RATE_LIMITED_BYTES_TOTAL
        .store(repair_serve_rate_limited_bytes_total, Ordering::Relaxed);
    REPAIR_SERVE_ERRORS_TOTAL.store(repair_serve_errors_total, Ordering::Relaxed);
    REPAIR_SERVE_QUEUE_DROPS_TOTAL.store(repair_serve_queue_drops_total, Ordering::Relaxed);
    REPAIR_SOURCE_HINT_ENQUEUED_TOTAL.store(repair_source_hint_enqueued_total, Ordering::Relaxed);
    REPAIR_SOURCE_HINT_DROPS_TOTAL.store(repair_source_hint_drops_total, Ordering::Relaxed);
    REPAIR_SOURCE_HINT_BUFFER_DROPS_TOTAL
        .store(repair_source_hint_buffer_drops_total, Ordering::Relaxed);
    REPAIR_PEER_TOTAL.store(repair_peer_total, Ordering::Relaxed);
    REPAIR_PEER_ACTIVE.store(repair_peer_active, Ordering::Relaxed);
    GOSSIP_RUNTIME_SWITCH_ATTEMPTS_TOTAL
        .store(gossip_runtime_switch_attempts_total, Ordering::Relaxed);
    GOSSIP_RUNTIME_SWITCH_SUCCESSES_TOTAL
        .store(gossip_runtime_switch_successes_total, Ordering::Relaxed);
    GOSSIP_RUNTIME_SWITCH_FAILURES_TOTAL
        .store(gossip_runtime_switch_failures_total, Ordering::Relaxed);
}

/// Raises one monotonic runtime counter when `value` exceeds its current maximum.
fn observe_max_counter(counter: &CacheAlignedAtomicU64, value: u64) {
    let mut current = counter.load(Ordering::Relaxed);
    while value > current {
        match counter.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}
