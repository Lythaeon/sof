use super::*;
use std::num::NonZeroUsize;
use std::sync::atomic::AtomicBool;

struct CompletedDatasetJob {
    slot: u64,
    start_index: u32,
    end_index: u32,
    last_in_slot: bool,
    serialized_shreds: Vec<Vec<u8>>,
}

#[derive(Clone)]
pub(in crate::app::runtime) struct DatasetDispatchQueue {
    ring: Arc<ArrayQueue<CompletedDatasetJob>>,
    notify: Arc<Notify>,
}

/// Managed dataset worker pool with queue handles and cooperative shutdown.
pub(in crate::app::runtime) struct DatasetWorkerPool {
    /// Per-worker dispatch queues used by the ingest hot path.
    queues: Vec<DatasetDispatchQueue>,
    /// Cooperative shutdown signal shared by all worker tasks.
    shutdown: Arc<AtomicBool>,
    /// Wakeup notifier for workers blocked on empty queues.
    shutdown_notify: Arc<Notify>,
    /// Join handles for all worker tasks.
    worker_handles: Vec<JoinHandle<()>>,
}

impl DatasetWorkerPool {
    /// Returns dispatch queues used for slot-to-worker routing.
    pub(in crate::app::runtime) const fn queues(&self) -> &[DatasetDispatchQueue] {
        self.queues.as_slice()
    }

    /// Gracefully stops workers after draining any already-enqueued jobs.
    pub(in crate::app::runtime) async fn shutdown(self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.shutdown_notify.notify_waiters();
        for handle in self.worker_handles {
            if handle.await.is_err() {
                // Worker task was already cancelled.
            }
        }
    }
}

impl DatasetDispatchQueue {
    fn new(capacity: usize) -> Self {
        Self {
            ring: Arc::new(ArrayQueue::new(capacity.max(1))),
            notify: Arc::new(Notify::new()),
        }
    }

    fn push_overwrite_oldest(&self, mut job: CompletedDatasetJob) -> u64 {
        let mut overwritten = 0_u64;
        loop {
            match self.ring.push(job) {
                Ok(()) => {
                    self.notify.notify_one();
                    return overwritten;
                }
                Err(returned) => {
                    job = returned;
                    if self.ring.pop().is_some() {
                        overwritten = overwritten.saturating_add(1);
                    }
                }
            }
        }
    }

    fn pop(&self) -> Option<CompletedDatasetJob> {
        self.ring.pop()
    }

    pub(in crate::app::runtime) fn len(&self) -> usize {
        self.ring.len()
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
}

#[derive(Clone)]
pub(in crate::app::runtime) struct DatasetWorkerShared {
    pub(in crate::app::runtime) plugin_host: PluginHost,
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
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_notify = Arc::new(Notify::new());
    let mut dispatch = Vec::with_capacity(worker_count);
    let mut worker_handles = Vec::with_capacity(worker_count);
    for _worker_id in 0..worker_count {
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
        let plugin_host = shared.plugin_host.clone();
        let attempt_cache_capacity = config.attempt_cache_capacity;
        let attempt_success_ttl = config.attempt_success_ttl;
        let attempt_failure_ttl = config.attempt_failure_ttl;
        let log_dataset_reconstruction = config.log_dataset_reconstruction;
        let worker_shutdown = shutdown.clone();
        let worker_shutdown_notify = shutdown_notify.clone();
        let worker_handle = tokio::spawn(async move {
            let mut attempt_cache = RecentDatasetAttemptCache::new(
                attempt_cache_capacity,
                attempt_success_ttl,
                attempt_failure_ttl,
            );
            loop {
                while let Some(job) = worker_queue.pop() {
                    let cache_key = DatasetAttemptKey {
                        slot: job.slot,
                        start_index: job.start_index,
                        end_index: job.end_index,
                    };
                    let now = Instant::now();
                    if attempt_cache.is_recent_duplicate(cache_key, now) {
                        let _ = dataset_duplicate_drop_count.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let _ = dataset_jobs_started_count.fetch_add(1, Ordering::Relaxed);
                    let outcome = process::process_completed_dataset(
                        process::DatasetProcessInput {
                            slot: job.slot,
                            start_index: job.start_index,
                            end_index: job.end_index,
                            last_in_slot: job.last_in_slot,
                            serialized_shreds: job.serialized_shreds,
                        },
                        &process::DatasetProcessContext {
                            plugin_host: &plugin_host,
                            tx_event_tx: &tx_event_tx,
                            tx_commitment_tracker: tx_commitment_tracker.as_ref(),
                            tx_event_drop_count: tx_event_drop_count.as_ref(),
                            dataset_decode_fail_count: dataset_decode_fail_count.as_ref(),
                            dataset_tail_skip_count: dataset_tail_skip_count.as_ref(),
                            log_dataset_reconstruction,
                        },
                    );
                    let _ = dataset_jobs_completed_count.fetch_add(1, Ordering::Relaxed);
                    attempt_cache.record(cache_key, now, outcome.status());
                }
                if worker_shutdown.load(Ordering::Relaxed) {
                    break;
                }
                tokio::select! {
                    () = worker_queue.notify.notified() => {}
                    () = worker_shutdown_notify.notified() => {}
                }
            }
        });
        worker_handles.push(worker_handle);
        dispatch.push(queue);
    }
    DatasetWorkerPool {
        queues: dispatch,
        shutdown,
        shutdown_notify,
        worker_handles,
    }
}

pub(in crate::app::runtime) fn dispatch_completed_dataset(
    dataset_dispatch: &[DatasetDispatchQueue],
    dataset: crate::reassembly::dataset::CompletedDataSet,
    dataset_jobs_enqueued_count: &AtomicU64,
    dataset_queue_drop_count: &AtomicU64,
) {
    let Some(dispatch_len) = NonZeroUsize::new(dataset_dispatch.len()) else {
        return;
    };
    let slot_index = usize::try_from(dataset.slot).unwrap_or(usize::MAX);
    let worker_index = slot_index.checked_rem(dispatch_len.get()).unwrap_or(0);
    let job = CompletedDatasetJob {
        slot: dataset.slot,
        start_index: dataset.start_index,
        end_index: dataset.end_index,
        last_in_slot: dataset.last_in_slot,
        serialized_shreds: dataset.serialized_shreds,
    };
    let Some(queue) = dataset_dispatch.get(worker_index) else {
        return;
    };
    let overwritten = queue.push_overwrite_oldest(job);
    let _ = dataset_jobs_enqueued_count.fetch_add(1, Ordering::Relaxed);
    if overwritten > 0 {
        let _ = dataset_queue_drop_count.fetch_add(overwritten, Ordering::Relaxed);
    }
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

    fn build_job(slot: u64, start_index: u32, end_index: u32) -> CompletedDatasetJob {
        CompletedDatasetJob {
            slot,
            start_index,
            end_index,
            last_in_slot: false,
            serialized_shreds: vec![vec![0_u8; 1]],
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
        assert_eq!(job.slot, 2);
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
        assert_eq!(first.slot, 98);
        assert_eq!(second.slot, 99);
        assert!(queue.pop().is_none());
    }
}
