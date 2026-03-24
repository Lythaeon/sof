use super::*;
use crate::framework::host::TransactionDispatchScope;
use std::num::NonZeroUsize;
use std::sync::{OnceLock, atomic::AtomicBool};

/// Caps per-worker burst size so one hot dispatch turn cannot create a large
/// head-of-line stall inside a single queued job.
const MAX_COMPLETED_DATASETS_PER_JOB: usize = 4;

/// Reassembled dataset payload scheduled onto one dataset worker queue.
struct CompletedDatasetJobItem {
    slot: u64,
    start_index: u32,
    end_index: u32,
    last_in_slot: bool,
    completed_at: Instant,
    first_shred_observed_at: Instant,
    last_shred_observed_at: Instant,
    payload_fragments: crate::reassembly::dataset::PayloadFragmentBatch,
}

/// One burst of reassembled dataset payloads scheduled onto one worker queue.
struct CompletedDatasetJob {
    items: Vec<CompletedDatasetJobItem>,
}

impl CompletedDatasetJob {
    fn dataset_count(&self) -> u64 {
        u64::try_from(self.items.len()).unwrap_or(u64::MAX)
    }
}

#[derive(Clone)]
pub(in crate::app::runtime) struct DatasetDispatchQueue {
    ring: Arc<ArrayQueue<CompletedDatasetJob>>,
    worker_thread: Arc<OnceLock<std::thread::Thread>>,
    queued_datasets: Arc<AtomicU64>,
}

/// Managed dataset worker pool with queue handles and cooperative shutdown.
pub(in crate::app::runtime) struct DatasetWorkerPool {
    /// Per-worker dispatch queues used by the ingest hot path.
    queues: Vec<DatasetDispatchQueue>,
    /// Cooperative shutdown signal shared by all worker tasks.
    shutdown: Arc<AtomicBool>,
    /// Join handles for all worker tasks.
    worker_handles: Vec<std::thread::JoinHandle<()>>,
}

impl DatasetWorkerPool {
    /// Returns dispatch queues used for slot-to-worker routing.
    pub(in crate::app::runtime) const fn queues(&self) -> &[DatasetDispatchQueue] {
        self.queues.as_slice()
    }

    /// Gracefully stops workers after draining any already-enqueued jobs.
    pub(in crate::app::runtime) async fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        for queue in &self.queues {
            queue.notify_all();
        }
        for handle in std::mem::take(&mut self.worker_handles) {
            let worker_name = handle.thread().name().map(str::to_owned);
            if handle.join().is_err() {
                tracing::error!(
                    worker = worker_name.as_deref().unwrap_or("sof-dataset"),
                    "dataset worker thread panicked during shutdown"
                );
            }
        }
    }
}

impl DatasetDispatchQueue {
    /// Creates one bounded queue for a dataset worker shard.
    fn new(capacity: usize) -> Self {
        Self {
            ring: Arc::new(ArrayQueue::new(capacity.max(1))),
            worker_thread: Arc::new(OnceLock::new()),
            queued_datasets: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Pushes a completed dataset, evicting oldest jobs when the queue is full.
    fn push_overwrite_oldest(&self, mut job: CompletedDatasetJob) -> u64 {
        let mut overwritten = 0_u64;
        loop {
            let should_notify = self.ring.is_empty();
            let job_dataset_count = job.dataset_count();
            match self.ring.push(job) {
                Ok(()) => {
                    let _ = self
                        .queued_datasets
                        .fetch_add(job_dataset_count, Ordering::Relaxed);
                    if should_notify {
                        self.notify_one();
                    }
                    return overwritten;
                }
                Err(returned) => {
                    job = returned;
                    if let Some(dropped) = self.ring.pop() {
                        let dropped_count = dropped.dataset_count();
                        overwritten = overwritten.saturating_add(dropped_count);
                        let _ = self
                            .queued_datasets
                            .fetch_sub(dropped_count, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    /// Pops one queued dataset job when work is available.
    fn pop(&self) -> Option<CompletedDatasetJob> {
        let job = self.ring.pop()?;
        let _ = self
            .queued_datasets
            .fetch_sub(job.dataset_count(), Ordering::Relaxed);
        Some(job)
    }

    /// Parks until more work arrives or shutdown is requested.
    fn wait_for_work(&self, shutdown: &AtomicBool) {
        while self.ring.is_empty() && !shutdown.load(Ordering::Relaxed) {
            std::thread::park();
        }
    }

    /// Wakes the registered worker thread when the queue transitions to ready.
    fn notify_one(&self) {
        if let Some(thread) = self.worker_thread.get() {
            thread.unpark();
        }
    }

    /// Wakes all waiters associated with this queue.
    fn notify_all(&self) {
        self.notify_one();
    }

    /// Records the current worker thread so producers can wake it directly.
    fn register_current_thread(&self) {
        drop(self.worker_thread.set(std::thread::current()));
    }

    pub(in crate::app::runtime) fn len(&self) -> usize {
        usize::try_from(self.queued_datasets.load(Ordering::Relaxed)).unwrap_or(usize::MAX)
    }
}

#[derive(Clone, Copy)]
pub(in crate::app::runtime) struct DatasetWorkerConfig {
    pub(in crate::app::runtime) workers: usize,
    pub(in crate::app::runtime) queue_capacity: usize,
    pub(in crate::app::runtime) attempt_cache_capacity: usize,
    pub(in crate::app::runtime) attempt_success_ttl: Duration,
    pub(in crate::app::runtime) attempt_failure_ttl: Duration,
    pub(in crate::app::runtime) log_dataset_reconstruction: bool,
    pub(in crate::app::runtime) log_all_txs: bool,
    pub(in crate::app::runtime) log_non_vote_txs: bool,
    pub(in crate::app::runtime) skip_vote_only_tx_detail_path: bool,
}

#[derive(Clone)]
pub(in crate::app::runtime) struct DatasetWorkerShared {
    pub(in crate::app::runtime) derived_state_host: DerivedStateHost,
    pub(in crate::app::runtime) plugin_host: PluginHost,
    pub(in crate::app::runtime) transaction_dispatch_scope: TransactionDispatchScope,
    pub(in crate::app::runtime) tx_event_tx: mpsc::Sender<TxObservedEvent>,
    pub(in crate::app::runtime) tx_commitment_tracker: Arc<CommitmentSlotTracker>,
    pub(in crate::app::runtime) tx_event_drop_count: Arc<AtomicU64>,
    pub(in crate::app::runtime) dataset_decode_fail_count: Arc<AtomicU64>,
    pub(in crate::app::runtime) dataset_tail_skip_count: Arc<AtomicU64>,
    pub(in crate::app::runtime) dataset_duplicate_drop_count: Arc<AtomicU64>,
    pub(in crate::app::runtime) dataset_jobs_started_count: Arc<AtomicU64>,
    pub(in crate::app::runtime) dataset_jobs_completed_count: Arc<AtomicU64>,
}

pub(in crate::app::runtime) fn spawn_dataset_workers(
    config: DatasetWorkerConfig,
    shared: &DatasetWorkerShared,
) -> DatasetWorkerPool {
    let worker_count = config.workers.max(1);
    let queue_capacity = config.queue_capacity.max(1);
    let allowed_core_ids = allowed_dataset_worker_cores();
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut dispatch = Vec::with_capacity(worker_count);
    let mut worker_handles = Vec::with_capacity(worker_count);
    for worker_id in 0..worker_count {
        let queue = DatasetDispatchQueue::new(queue_capacity);
        let worker_queue = queue.clone();
        let tx_event_tx = shared.tx_event_tx.clone();
        let tx_commitment_tracker = shared.tx_commitment_tracker.clone();
        let tx_event_drop_count = shared.tx_event_drop_count.clone();
        let dataset_decode_fail_count = shared.dataset_decode_fail_count.clone();
        let dataset_tail_skip_count = shared.dataset_tail_skip_count.clone();
        let dataset_duplicate_drop_count = shared.dataset_duplicate_drop_count.clone();
        let dataset_jobs_started_count = shared.dataset_jobs_started_count.clone();
        let dataset_jobs_completed_count = shared.dataset_jobs_completed_count.clone();
        let derived_state_host = shared.derived_state_host.clone();
        let plugin_host = shared.plugin_host.clone();
        let transaction_dispatch_scope = shared.transaction_dispatch_scope;
        let attempt_cache_capacity = config.attempt_cache_capacity;
        let attempt_success_ttl = config.attempt_success_ttl;
        let attempt_failure_ttl = config.attempt_failure_ttl;
        let log_dataset_reconstruction = config.log_dataset_reconstruction;
        let log_all_txs = config.log_all_txs;
        let log_non_vote_txs = config.log_non_vote_txs;
        let skip_vote_only_tx_detail_path = config.skip_vote_only_tx_detail_path;
        let worker_shutdown = shutdown.clone();
        let worker_core_id = NonZeroUsize::new(allowed_core_ids.len()).and_then(|core_count| {
            let worker_core_index = worker_id.checked_rem(core_count.get())?;
            allowed_core_ids.get(worker_core_index).copied()
        });
        let worker_handle = match std::thread::Builder::new()
            .name(format!("sof-dataset-{worker_id}"))
            .spawn(move || {
                maybe_pin_dataset_worker_thread(worker_id, worker_core_id);
                worker_queue.register_current_thread();
                let mut scratch = process::DatasetWorkerScratch::default();
                let mut attempt_cache = RecentDatasetAttemptCache::new(
                    attempt_cache_capacity,
                    attempt_success_ttl,
                    attempt_failure_ttl,
                );
                loop {
                    while let Some(job) = worker_queue.pop() {
                        for item in job.items {
                            let cache_key = DatasetAttemptKey {
                                slot: item.slot,
                                start_index: item.start_index,
                                end_index: item.end_index,
                            };
                            let now = Instant::now();
                            if attempt_cache.is_recent_duplicate(cache_key, now) {
                                let _ =
                                    dataset_duplicate_drop_count.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            let _ = dataset_jobs_started_count.fetch_add(1, Ordering::Relaxed);
                            crate::runtime_metrics::observe_dataset_job_started(
                                u64::try_from(
                                    now.saturating_duration_since(item.completed_at).as_millis(),
                                )
                                .unwrap_or(u64::MAX),
                            );
                            let processing_started_at = Instant::now();
                            let outcome = process::process_completed_dataset(
                                process::DatasetProcessInput {
                                    slot: item.slot,
                                    start_index: item.start_index,
                                    end_index: item.end_index,
                                    last_in_slot: item.last_in_slot,
                                    completed_at: item.completed_at,
                                    first_shred_observed_at: item.first_shred_observed_at,
                                    last_shred_observed_at: item.last_shred_observed_at,
                                    payload_fragments: item.payload_fragments,
                                },
                                &process::DatasetProcessContext {
                                    derived_state_host: &derived_state_host,
                                    plugin_host: &plugin_host,
                                    transaction_dispatch_scope,
                                    tx_event_tx: &tx_event_tx,
                                    tx_commitment_tracker: tx_commitment_tracker.as_ref(),
                                    tx_event_drop_count: tx_event_drop_count.as_ref(),
                                    dataset_decode_fail_count: dataset_decode_fail_count.as_ref(),
                                    dataset_tail_skip_count: dataset_tail_skip_count.as_ref(),
                                    log_dataset_reconstruction,
                                    log_all_txs,
                                    log_non_vote_txs,
                                    skip_vote_only_tx_detail_path,
                                },
                                &mut scratch,
                            );
                            let _ = dataset_jobs_completed_count.fetch_add(1, Ordering::Relaxed);
                            crate::runtime_metrics::observe_dataset_job_completed(
                                u64::try_from(processing_started_at.elapsed().as_micros())
                                    .unwrap_or(u64::MAX),
                            );
                            attempt_cache.record(cache_key, now, outcome.status());
                        }
                    }
                    if worker_shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    worker_queue.wait_for_work(&worker_shutdown);
                }
            }) {
            Ok(handle) => handle,
            Err(error) => {
                tracing::error!(
                    worker_id,
                    error = %error,
                    "failed to spawn dataset worker thread"
                );
                continue;
            }
        };
        worker_handles.push(worker_handle);
        dispatch.push(queue);
    }
    DatasetWorkerPool {
        queues: dispatch,
        shutdown,
        worker_handles,
    }
}

pub(in crate::app::runtime) fn dispatch_completed_dataset(
    dataset_dispatch: &[DatasetDispatchQueue],
    completed_datasets: Vec<crate::reassembly::dataset::CompletedDataSet>,
    observed_at: Instant,
    dataset_jobs_enqueued_count: &AtomicU64,
    dataset_queue_drop_count: &AtomicU64,
) {
    let Some(dispatch_len) = NonZeroUsize::new(dataset_dispatch.len()) else {
        return;
    };
    let mut batched = (0..dispatch_len.get())
        .map(|_| Vec::<CompletedDatasetJobItem>::new())
        .collect::<Vec<_>>();
    for dataset in completed_datasets {
        let worker_index = dataset_worker_index(
            dispatch_len,
            dataset.slot,
            dataset.start_index,
            dataset.end_index,
        );
        if let Some(worker_batch) = batched.get_mut(worker_index) {
            worker_batch.push(CompletedDatasetJobItem {
                slot: dataset.slot,
                start_index: dataset.start_index,
                end_index: dataset.end_index,
                last_in_slot: dataset.last_in_slot,
                completed_at: observed_at,
                first_shred_observed_at: dataset.first_shred_observed_at,
                last_shred_observed_at: dataset.last_shred_observed_at,
                payload_fragments: dataset.payload_fragments,
            });
        }
    }
    for (worker_index, items) in batched.into_iter().enumerate() {
        if items.is_empty() {
            continue;
        }
        let Some(queue) = dataset_dispatch.get(worker_index) else {
            continue;
        };
        let item_count = u64::try_from(items.len()).unwrap_or(u64::MAX);
        let mut overwritten = 0_u64;
        let mut items = items.into_iter();
        loop {
            let chunk = items
                .by_ref()
                .take(MAX_COMPLETED_DATASETS_PER_JOB)
                .collect::<Vec<_>>();
            if chunk.is_empty() {
                break;
            }
            overwritten = overwritten
                .saturating_add(queue.push_overwrite_oldest(CompletedDatasetJob { items: chunk }));
        }
        let _ = dataset_jobs_enqueued_count.fetch_add(item_count, Ordering::Relaxed);
        crate::runtime_metrics::observe_dataset_jobs_enqueued(item_count);
        if overwritten > 0 {
            let _ = dataset_queue_drop_count.fetch_add(overwritten, Ordering::Relaxed);
            crate::runtime_metrics::observe_dataset_queue_dropped_jobs(overwritten);
        }
    }
}

fn dataset_worker_index(
    dispatch_len: NonZeroUsize,
    slot: u64,
    start_index: u32,
    end_index: u32,
) -> usize {
    // Live datasets often advance in fixed shred windows (for example 32-shred
    // ranges). Worker selection only uses the low bits when `dispatch_len` is a
    // power of two, so weak low-bit mixing can collapse almost all live work
    // onto one queue. Build a composite key and run it through a splitmix64-style
    // finalizer so hot-slot ranges fan out evenly while duplicate ranges remain
    // deterministic.
    let mut hash = slot;
    hash ^= u64::from(start_index) << 21;
    hash ^= u64::from(end_index).rotate_left(7);
    hash ^= u64::from(start_index ^ end_index).wrapping_mul(0x9E37_79B9);
    hash ^= hash >> 30;
    hash = hash.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    hash ^= hash >> 27;
    hash = hash.wrapping_mul(0x94D0_49BB_1331_11EB);
    hash ^= hash >> 31;
    let dispatch_len_u64 = u64::try_from(dispatch_len.get()).unwrap_or(1);
    let worker_index = hash.checked_rem(dispatch_len_u64).unwrap_or(0);
    usize::try_from(worker_index).unwrap_or(0)
}

fn allowed_dataset_worker_cores() -> Vec<core_affinity::CoreId> {
    let Some(all_cores) = core_affinity::get_core_ids() else {
        return Vec::new();
    };
    let Some(allowed_core_ids) = std::fs::read_to_string("/proc/self/status")
        .ok()
        .as_deref()
        .and_then(parse_allowed_core_ids_from_proc_status)
    else {
        return all_cores;
    };
    let filtered: Vec<core_affinity::CoreId> = all_cores
        .into_iter()
        .filter(|core| allowed_core_ids.contains(&core.id))
        .collect();
    if filtered.is_empty() {
        return Vec::new();
    }
    filtered
}

fn maybe_pin_dataset_worker_thread(worker_id: usize, core_id: Option<core_affinity::CoreId>) {
    let Some(core_id) = core_id else {
        return;
    };
    if core_affinity::set_for_current(core_id) {
        tracing::info!(
            worker_id,
            assigned_core = core_id.id,
            "pinned dataset worker thread to CPU core"
        );
    } else {
        tracing::warn!(
            worker_id,
            assigned_core = core_id.id,
            "failed to pin dataset worker thread to CPU core"
        );
    }
}

fn parse_allowed_core_ids_from_proc_status(status: &str) -> Option<Vec<usize>> {
    let cpus_line = status
        .lines()
        .find(|line| line.starts_with("Cpus_allowed_list:"))?;
    let raw = cpus_line.split_once(':')?.1.trim();
    if raw.is_empty() {
        return None;
    }
    let mut cores = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            let start = start.trim().parse::<usize>().ok()?;
            let end = end.trim().parse::<usize>().ok()?;
            if start > end {
                return None;
            }
            cores.extend(start..=end);
        } else {
            cores.push(part.parse::<usize>().ok()?);
        }
    }
    (!cores.is_empty()).then_some(cores)
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(in crate::app::runtime) enum DatasetProcessOutcome {
    Decoded,
    DecodeFailed,
}

impl DatasetProcessOutcome {
    pub(in crate::app::runtime) const fn status(self) -> DatasetAttemptStatus {
        match self {
            Self::Decoded => DatasetAttemptStatus::Success,
            Self::DecodeFailed => DatasetAttemptStatus::Failure,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reassembly::dataset::CompletedDataSet;

    fn build_job(slot: u64, start_index: u32, end_index: u32) -> CompletedDatasetJob {
        CompletedDatasetJob {
            items: vec![build_job_item(slot, start_index, end_index)],
        }
    }

    fn build_job_item(slot: u64, start_index: u32, end_index: u32) -> CompletedDatasetJobItem {
        CompletedDatasetJobItem {
            slot,
            start_index,
            end_index,
            last_in_slot: false,
            completed_at: Instant::now(),
            first_shred_observed_at: Instant::now(),
            last_shred_observed_at: Instant::now(),
            payload_fragments:
                crate::reassembly::dataset::PayloadFragmentBatch::from_owned_fragments(vec![
                    vec![0_u8; 1],
                ]),
        }
    }

    #[test]
    fn queue_overwrites_oldest_when_capacity_reached() {
        let queue = DatasetDispatchQueue::new(1);
        assert_eq!(queue.push_overwrite_oldest(build_job(1, 0, 0)), 0);
        assert_eq!(queue.push_overwrite_oldest(build_job(2, 1, 1)), 1);
        let Some(job) = queue.pop() else {
            panic!("expected queued dataset job");
        };
        assert_eq!(job.items[0].slot, 2);
        assert!(queue.pop().is_none());
    }

    #[test]
    fn queue_backpressure_keeps_latest_jobs() {
        let queue = DatasetDispatchQueue::new(2);
        let mut overwritten_total = 0_u64;
        for slot in 0_u64..100 {
            overwritten_total = overwritten_total
                .saturating_add(queue.push_overwrite_oldest(build_job(slot, 0, 0)));
        }
        assert_eq!(overwritten_total, 98);
        let Some(first) = queue.pop() else {
            panic!("expected first remaining queued job");
        };
        let Some(second) = queue.pop() else {
            panic!("expected second remaining queued job");
        };
        assert_eq!(first.items[0].slot, 98);
        assert_eq!(second.items[0].slot, 99);
        assert!(queue.pop().is_none());
    }

    #[test]
    fn queue_depth_tracks_datasets_not_batch_nodes() {
        let queue = DatasetDispatchQueue::new(4);
        assert_eq!(queue.len(), 0);
        assert_eq!(
            queue.push_overwrite_oldest(CompletedDatasetJob {
                items: vec![build_job_item(1, 0, 0), build_job_item(1, 1, 1)],
            }),
            0
        );
        assert_eq!(queue.len(), 2);
        let _ = queue.pop();
        assert_eq!(queue.len(), 0);
    }

    fn build_completed_dataset(slot: u64, start_index: u32, end_index: u32) -> CompletedDataSet {
        CompletedDataSet {
            slot,
            start_index,
            end_index,
            payload_fragments:
                crate::reassembly::dataset::PayloadFragmentBatch::from_owned_fragments(vec![
                    vec![0_u8; 1],
                ]),
            last_in_slot: false,
            first_shred_observed_at: Instant::now(),
            last_shred_observed_at: Instant::now(),
        }
    }

    #[test]
    fn dispatch_completed_dataset_splits_large_worker_bursts() {
        let queue = DatasetDispatchQueue::new(8);
        let dispatch = vec![queue.clone()];
        let datasets = (0_u32..9)
            .map(|index| build_completed_dataset(7, index, index))
            .collect::<Vec<_>>();
        let enqueued = AtomicU64::new(0);
        let dropped = AtomicU64::new(0);

        dispatch_completed_dataset(&dispatch, datasets, Instant::now(), &enqueued, &dropped);

        assert_eq!(enqueued.load(Ordering::Relaxed), 9);
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        assert_eq!(queue.len(), 9);

        let Some(first) = queue.pop() else {
            panic!("expected first queued dataset job");
        };
        let Some(second) = queue.pop() else {
            panic!("expected second queued dataset job");
        };
        let Some(third) = queue.pop() else {
            panic!("expected third queued dataset job");
        };
        assert_eq!(first.items.len(), MAX_COMPLETED_DATASETS_PER_JOB);
        assert_eq!(second.items.len(), MAX_COMPLETED_DATASETS_PER_JOB);
        assert_eq!(third.items.len(), 1);
        assert!(queue.pop().is_none());
    }

    #[test]
    fn worker_index_fans_out_same_slot_ranges() {
        let worker_count = NonZeroUsize::new(4).expect("non-zero worker count");
        let mut seen = std::collections::BTreeSet::new();
        for start_index in 0_u32..32 {
            seen.insert(dataset_worker_index(
                worker_count,
                404_931_885,
                start_index,
                start_index.saturating_add(1),
            ));
        }
        assert!(seen.len() > 1);
    }

    #[test]
    fn worker_index_avoids_fixed_window_live_skew() {
        let worker_count = NonZeroUsize::new(4).expect("non-zero worker count");
        let mut counts = [0_usize; 4];
        for start_index in (0_u32..(32 * 128)).step_by(32) {
            let worker_index = dataset_worker_index(
                worker_count,
                405_028_422,
                start_index,
                start_index.saturating_add(31),
            );
            counts[worker_index] += 1;
        }
        let busiest_worker = counts.into_iter().max().unwrap_or(0);
        assert!(busiest_worker < 64, "unexpected fixed-window skew");
    }

    #[test]
    fn parses_allowed_core_ids_from_proc_status_ranges() {
        let status = String::from("Name:\tsof\nCpus_allowed_list:\t0-2,5,7-8\n");
        let allowed = parse_allowed_core_ids_from_proc_status(&status)
            .expect("expected parsed core ids from proc status");
        assert_eq!(allowed, vec![0, 1, 2, 5, 7, 8]);
    }
}
