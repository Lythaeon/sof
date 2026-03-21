use std::{
    any::Any,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};

use arcshift::ArcShift;
use solana_pubkey::Pubkey;
use thiserror::Error;
use tokio::sync::{Semaphore, mpsc};

use crate::framework::{
    events::{
        ClusterTopologyEvent, DatasetEvent, LeaderScheduleEntry, LeaderScheduleEvent,
        ObservedRecentBlockhashEvent, RawPacketEvent, ReorgEvent, ShredEvent, SlotStatusEvent,
        TransactionEvent,
    },
    plugin::{ObserverPlugin, PluginConfig},
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
/// Default number of transaction-dispatch workers for accepted transaction callbacks.
const DEFAULT_TRANSACTION_DISPATCH_WORKERS_CAP: usize = 8;
/// Number of initial dropped-event warnings emitted without sampling.
const INITIAL_DROP_LOG_LIMIT: u64 = 5;
/// Warning sample cadence after the initial dropped-event warning burst.
const DROP_LOG_SAMPLE_EVERY: u64 = 1_000;

#[derive(Debug, Default)]
/// Shared lifecycle state so cloned plugin hosts run startup/shutdown hooks once.
struct PluginHostLifecycleState {
    /// Tracks whether plugin startup completed and shutdown is still pending.
    started: AtomicBool,
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
#[error("plugin `{plugin}` startup failed: {reason}")]
/// Startup failure returned by [`PluginHost::startup`].
pub struct PluginHostStartupError {
    /// Plugin identifier.
    pub plugin: &'static str,
    /// Human-readable failure reason.
    pub reason: String,
}

/// Chooses a bounded default worker count for accepted-transaction dispatch.
fn default_transaction_dispatch_workers() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .clamp(1, DEFAULT_TRANSACTION_DISPATCH_WORKERS_CAP)
}

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

/// Hook-interest bitmap used to skip enqueueing callbacks no plugin requested.
#[derive(Debug, Clone, Copy, Default)]
struct PluginHookSubscriptions {
    /// At least one plugin wants raw-packet callbacks.
    raw_packet: bool,
    /// At least one plugin wants parsed-shred callbacks.
    shred: bool,
    /// At least one plugin wants dataset callbacks.
    dataset: bool,
    /// At least one plugin wants transaction callbacks.
    transaction: bool,
    /// At least one plugin wants account-touch callbacks.
    account_touch: bool,
    /// At least one plugin wants slot-status callbacks.
    slot_status: bool,
    /// At least one plugin wants reorg callbacks.
    reorg: bool,
    /// At least one plugin wants recent-blockhash callbacks.
    recent_blockhash: bool,
    /// At least one plugin wants cluster-topology callbacks.
    cluster_topology: bool,
    /// At least one plugin wants leader-schedule callbacks.
    leader_schedule: bool,
}

impl From<&PluginConfig> for PluginHookSubscriptions {
    fn from(config: &PluginConfig) -> Self {
        Self {
            raw_packet: config.raw_packet,
            shred: config.shred,
            dataset: config.dataset,
            transaction: config.transaction,
            account_touch: config.account_touch,
            slot_status: config.slot_status,
            reorg: config.reorg,
            recent_blockhash: config.recent_blockhash,
            cluster_topology: config.cluster_topology,
            leader_schedule: config.leader_schedule,
        }
    }
}
