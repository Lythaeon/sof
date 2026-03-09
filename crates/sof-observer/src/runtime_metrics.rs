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
    /// Total recovered data shreds accepted after FEC repair.
    pub recovered_data_packets_total: u64,
    /// Current number of packets pending in packet-worker queues.
    pub packet_worker_queue_depth: u64,
    /// Maximum packet-worker queue depth observed since startup.
    pub packet_worker_max_queue_depth: u64,
    /// Total packet-worker batches dropped due to full worker queues.
    pub packet_worker_dropped_batches_total: u64,
    /// Total packets dropped due to full packet-worker queues.
    pub packet_worker_dropped_packets_total: u64,
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
}

/// Total recovered data shreds accepted after FEC repair.
static RECOVERED_DATA_PACKETS_TOTAL: CacheAlignedAtomicU64 =
    CacheAlignedAtomicU64(AtomicU64::new(0));
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

/// Returns the latest SOF runtime-stage counter snapshot.
#[must_use]
pub fn snapshot() -> ObserverRuntimeMetricsSnapshot {
    ObserverRuntimeMetricsSnapshot {
        recovered_data_packets_total: RECOVERED_DATA_PACKETS_TOTAL.load(Ordering::Relaxed),
        packet_worker_queue_depth: PACKET_WORKER_QUEUE_DEPTH.load(Ordering::Relaxed),
        packet_worker_max_queue_depth: PACKET_WORKER_MAX_QUEUE_DEPTH.load(Ordering::Relaxed),
        packet_worker_dropped_batches_total: PACKET_WORKER_DROPPED_BATCHES_TOTAL
            .load(Ordering::Relaxed),
        packet_worker_dropped_packets_total: PACKET_WORKER_DROPPED_PACKETS_TOTAL
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
    }
}

/// Adds recovered-data packets accepted after FEC repair.
pub(crate) fn observe_recovered_data_packets(count: u64) {
    RECOVERED_DATA_PACKETS_TOTAL.fetch_add(count, Ordering::Relaxed);
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
