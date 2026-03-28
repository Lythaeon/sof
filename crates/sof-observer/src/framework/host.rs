use std::{
    any::Any,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};

use crate::framework::{
    events::{
        ClusterTopologyEvent, DatasetEvent, LeaderScheduleEntry, LeaderScheduleEvent,
        ObservedRecentBlockhashEvent, RawPacketEvent, ReorgEvent, ShredEvent, SlotStatusEvent,
        TransactionBatchEvent, TransactionEvent,
    },
    plugin::{ObserverPlugin, PluginConfig},
};
use arcshift::ArcShift;
use solana_pubkey::Pubkey;
use thiserror::Error;

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
pub(crate) use core::TransactionDispatchScope;
pub(crate) use dispatch::ClassifiedTransactionDispatch;
pub(crate) use dispatch::TransactionDispatchMetricsBatch;

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

/// Identifies how an inline transaction reached plugin dispatch.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum InlineTransactionDispatchSource {
    /// The transaction became reconstructable from an anchored contiguous open-dataset prefix.
    EarlyPrefix,
    /// The transaction waited for the completed-dataset decode path before inline dispatch.
    CompletedDatasetFallback,
}

impl InlineTransactionDispatchSource {
    /// Returns a stable label for metrics export.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::EarlyPrefix => "early_prefix",
            Self::CompletedDatasetFallback => "completed_dataset_fallback",
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
    /// Lowest commitment required by any transaction subscriber.
    transaction_min_commitment: crate::event::TxCommitmentStatus,
    /// At least one transaction plugin exposes a compiled prefilter.
    transaction_prefilter: bool,
    /// At least one plugin wants transaction-log callbacks.
    transaction_log: bool,
    /// At least one plugin wants transaction-status callbacks.
    transaction_status: bool,
    /// At least one plugin requested inline transaction dispatch.
    inline_transaction: bool,
    /// At least one plugin wants transaction batch callbacks.
    transaction_batch: bool,
    /// At least one plugin requested inline transaction-batch dispatch.
    inline_transaction_batch: bool,
    /// At least one plugin wants transaction-view-batch callbacks.
    transaction_view_batch: bool,
    /// At least one plugin requested inline transaction-view-batch dispatch.
    inline_transaction_view_batch: bool,
    /// At least one plugin wants account-touch callbacks.
    account_touch: bool,
    /// At least one plugin wants account-update callbacks.
    account_update: bool,
    /// At least one plugin wants block-meta callbacks.
    block_meta: bool,
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
            transaction_min_commitment: config.transaction_commitment.minimum_required(),
            transaction_prefilter: false,
            transaction_log: config.transaction_log,
            transaction_status: config.transaction_status,
            inline_transaction: config.transaction
                && matches!(
                    config.transaction_dispatch_mode,
                    crate::framework::plugin::TransactionDispatchMode::Inline
                ),
            transaction_batch: config.transaction_batch,
            inline_transaction_batch: config.transaction_batch
                && matches!(
                    config.transaction_batch_dispatch_mode,
                    crate::framework::plugin::TransactionDispatchMode::Inline
                ),
            transaction_view_batch: config.transaction_view_batch,
            inline_transaction_view_batch: config.transaction_view_batch
                && matches!(
                    config.transaction_view_batch_dispatch_mode,
                    crate::framework::plugin::TransactionDispatchMode::Inline
                ),
            account_touch: config.account_touch,
            account_update: config.account_update,
            block_meta: config.block_meta,
            slot_status: config.slot_status,
            reorg: config.reorg,
            recent_blockhash: config.recent_blockhash,
            cluster_topology: config.cluster_topology,
            leader_schedule: config.leader_schedule,
        }
    }
}
