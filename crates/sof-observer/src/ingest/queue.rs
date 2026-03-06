use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use crossbeam_queue::ArrayQueue;
use tokio::sync::{Notify, mpsc};

use super::RawPacketBatch;
use crate::runtime_env::read_env_var;

const DEFAULT_INGEST_QUEUE_CAPACITY: usize = 16_384;
const SPIN_BACKOFF_MICROS: u64 = 50;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum IngestQueueMode {
    Bounded,
    Unbounded,
    LockFree,
}

impl IngestQueueMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Bounded => "bounded",
            Self::Unbounded => "unbounded",
            Self::LockFree => "lockfree",
        }
    }
}

#[derive(Clone)]
pub struct RawPacketBatchSender {
    inner: RawPacketBatchSenderInner,
}

#[derive(Debug)]
pub enum RawPacketBatchReceiver {
    Bounded(mpsc::Receiver<RawPacketBatch>),
    Unbounded(mpsc::UnboundedReceiver<RawPacketBatch>),
    LockFree(LockFreeBatchReceiver),
}

#[derive(Clone)]
enum RawPacketBatchSenderInner {
    Bounded(mpsc::Sender<RawPacketBatch>),
    Unbounded(mpsc::UnboundedSender<RawPacketBatch>),
    LockFree(LockFreeBatchSender),
}

#[derive(Debug)]
struct LockFreeBatchQueue {
    queue: ArrayQueue<RawPacketBatch>,
    notify: Notify,
    closed: AtomicBool,
    sender_count: AtomicUsize,
}

struct LockFreeBatchSender {
    queue: Arc<LockFreeBatchQueue>,
}

#[derive(Debug)]
pub struct LockFreeBatchReceiver {
    queue: Arc<LockFreeBatchQueue>,
}

impl RawPacketBatchSender {
    #[must_use]
    pub fn send_batch(&self, mut batch: RawPacketBatch, drop_on_full: bool) -> bool {
        match &self.inner {
            RawPacketBatchSenderInner::Bounded(sender) => {
                if drop_on_full {
                    sender.try_send(batch).is_ok()
                } else {
                    sender.blocking_send(batch).is_ok()
                }
            }
            RawPacketBatchSenderInner::Unbounded(sender) => sender.send(batch).is_ok(),
            RawPacketBatchSenderInner::LockFree(sender) => {
                if drop_on_full {
                    if sender.queue.closed.load(Ordering::Acquire) {
                        return false;
                    }
                    match sender.queue.queue.push(batch) {
                        Ok(()) => {
                            sender.queue.notify.notify_one();
                            true
                        }
                        Err(_full_batch) => false,
                    }
                } else {
                    loop {
                        if sender.queue.closed.load(Ordering::Acquire) {
                            return false;
                        }
                        match sender.queue.queue.push(batch) {
                            Ok(()) => {
                                sender.queue.notify.notify_one();
                                return true;
                            }
                            Err(returned_batch) => {
                                batch = returned_batch;
                                std::thread::sleep(Duration::from_micros(SPIN_BACKOFF_MICROS));
                            }
                        }
                    }
                }
            }
        }
    }
}

impl Drop for LockFreeBatchSender {
    fn drop(&mut self) {
        if self.queue.sender_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.queue.closed.store(true, Ordering::Release);
            self.queue.notify.notify_waiters();
        }
    }
}

impl Clone for LockFreeBatchSender {
    fn clone(&self) -> Self {
        self.queue.sender_count.fetch_add(1, Ordering::AcqRel);
        Self {
            queue: Arc::clone(&self.queue),
        }
    }
}

impl Drop for LockFreeBatchReceiver {
    fn drop(&mut self) {
        self.queue.closed.store(true, Ordering::Release);
        self.queue.notify.notify_waiters();
    }
}

impl RawPacketBatchReceiver {
    pub async fn recv(&mut self) -> Option<RawPacketBatch> {
        match self {
            Self::Bounded(receiver) => receiver.recv().await,
            Self::Unbounded(receiver) => receiver.recv().await,
            Self::LockFree(receiver) => receiver.recv().await,
        }
    }
}

impl LockFreeBatchReceiver {
    async fn recv(&mut self) -> Option<RawPacketBatch> {
        loop {
            if let Some(batch) = self.queue.queue.pop() {
                return Some(batch);
            }
            if self.queue.closed.load(Ordering::Acquire) {
                return None;
            }

            let notified = self.queue.notify.notified();
            if let Some(batch) = self.queue.queue.pop() {
                return Some(batch);
            }
            if self.queue.closed.load(Ordering::Acquire) {
                return None;
            }
            notified.await;
        }
    }
}

impl From<mpsc::Receiver<RawPacketBatch>> for RawPacketBatchReceiver {
    fn from(value: mpsc::Receiver<RawPacketBatch>) -> Self {
        Self::Bounded(value)
    }
}

#[must_use]
pub fn create_raw_packet_batch_queue() -> (RawPacketBatchSender, RawPacketBatchReceiver) {
    let mode = read_ingest_queue_mode();
    let capacity = read_ingest_queue_capacity();
    tracing::info!(
        mode = %mode.as_str(),
        capacity,
        "configured ingest queue"
    );
    match mode {
        IngestQueueMode::Bounded => {
            let (tx, rx) = mpsc::channel::<RawPacketBatch>(capacity);
            (
                RawPacketBatchSender {
                    inner: RawPacketBatchSenderInner::Bounded(tx),
                },
                RawPacketBatchReceiver::Bounded(rx),
            )
        }
        IngestQueueMode::Unbounded => {
            let (tx, rx) = mpsc::unbounded_channel::<RawPacketBatch>();
            (
                RawPacketBatchSender {
                    inner: RawPacketBatchSenderInner::Unbounded(tx),
                },
                RawPacketBatchReceiver::Unbounded(rx),
            )
        }
        IngestQueueMode::LockFree => {
            let queue = Arc::new(LockFreeBatchQueue {
                queue: ArrayQueue::new(capacity),
                notify: Notify::new(),
                closed: AtomicBool::new(false),
                sender_count: AtomicUsize::new(1),
            });
            (
                RawPacketBatchSender {
                    inner: RawPacketBatchSenderInner::LockFree(LockFreeBatchSender {
                        queue: Arc::clone(&queue),
                    }),
                },
                RawPacketBatchReceiver::LockFree(LockFreeBatchReceiver { queue }),
            )
        }
    }
}

fn read_ingest_queue_mode() -> IngestQueueMode {
    let raw = read_env_var("SOF_INGEST_QUEUE_MODE").unwrap_or_else(|| "bounded".to_owned());
    match raw.trim().to_ascii_lowercase().as_str() {
        "bounded" | "tokio-bounded" | "tokio_bounded" => IngestQueueMode::Bounded,
        "unbounded" | "tokio-unbounded" | "tokio_unbounded" => IngestQueueMode::Unbounded,
        "lockfree" | "lock-free" | "ring" | "arrayqueue" => IngestQueueMode::LockFree,
        unknown => {
            tracing::warn!(
                value = %unknown,
                "unknown SOF_INGEST_QUEUE_MODE value; falling back to bounded"
            );
            IngestQueueMode::Bounded
        }
    }
}

fn read_ingest_queue_capacity() -> usize {
    read_env_var("SOF_INGEST_QUEUE_CAPACITY")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|capacity| *capacity > 0)
        .unwrap_or(DEFAULT_INGEST_QUEUE_CAPACITY)
}
