use std::{
    any::Any,
    future::Future,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use solana_pubkey::Pubkey;
use tokio::sync::{Semaphore, mpsc};

use crate::framework::{
    events::{
        ClusterTopologyEvent, DatasetEvent, LeaderScheduleEntry, LeaderScheduleEvent,
        ObservedRecentBlockhashEvent, RawPacketEvent, ReorgEvent, ShredEvent, SlotStatusEvent,
        TransactionEvent,
    },
    plugin::ObserverPlugin,
};

/// Builder surface for assembling immutable plugin hosts.
mod builder;
/// Immutable host state and public dispatch entrypoints.
mod core;
/// Asynchronous hook queueing and worker execution internals.
mod dispatch;
/// Internal snapshots for deduplicated observed state.
mod state;
#[cfg(test)]
mod tests;

pub use builder::PluginHostBuilder;
pub use core::PluginHost;

/// Default bounded queue capacity for asynchronous plugin hook dispatch.
const DEFAULT_EVENT_QUEUE_CAPACITY: usize = 8_192;
/// Number of initial dropped-event warnings emitted without sampling.
const INITIAL_DROP_LOG_LIMIT: u64 = 5;
/// Warning sample cadence after the initial dropped-event warning burst.
const DROP_LOG_SAMPLE_EVERY: u64 = 1_000;

/// Dispatch strategy used by the async plugin worker.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum PluginDispatchMode {
    /// Process callbacks one plugin at a time in registration order.
    #[default]
    Sequential,
    /// Process callbacks with bounded parallelism per queued event.
    ///
    /// Values lower than `1` are normalized to `1`.
    BoundedConcurrent(usize),
}

impl PluginDispatchMode {
    /// Returns a normalized mode with valid bounded-concurrency parameters.
    #[must_use]
    pub fn normalized(self) -> Self {
        match self {
            Self::Sequential => Self::Sequential,
            Self::BoundedConcurrent(limit) => Self::BoundedConcurrent(limit.max(1)),
        }
    }
}

/// Normalizes panic payloads into string logs for hook-failure diagnostics.
fn panic_payload_to_string(payload: &(dyn Any + Send)) -> String {
    payload.downcast_ref::<&str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "non-string panic payload".to_owned())
        },
        |message| (*message).to_owned(),
    )
}
