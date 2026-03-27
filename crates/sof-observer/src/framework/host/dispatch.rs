#![allow(clippy::missing_docs_in_private_items)]
#![allow(clippy::result_large_err)]

use super::*;
use crate::framework::TransactionInterest;
use crate::framework::{
    AccountTouchEvent, AccountUpdateEvent, BlockMetaEvent, TransactionStatusEvent,
};
use crossbeam_queue::ArrayQueue;
use futures_util::{FutureExt, StreamExt, stream};
use smallvec::SmallVec;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
use std::str::FromStr;
use std::sync::OnceLock;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::TrySendError as StdTrySendError;
use std::time::Instant;

struct DebugTxTraceConfig {
    pool_account: Pubkey,
}

fn debug_tx_trace_config() -> Option<&'static DebugTxTraceConfig> {
    static CONFIG: OnceLock<Option<DebugTxTraceConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            std::env::var("SOF_DEBUG_TRACE_TX_POOL")
                .ok()
                .and_then(|value| Pubkey::from_str(&value).ok())
                .map(|pool_account| DebugTxTraceConfig { pool_account })
        })
        .as_ref()
}

fn debug_tx_trace_signature(event: &TransactionEvent) -> Option<Signature> {
    let config = debug_tx_trace_config()?;
    if !event
        .tx
        .message
        .static_account_keys()
        .contains(&config.pool_account)
    {
        return None;
    }
    event
        .signature
        .map(sof_types::SignatureBytes::to_solana)
        .or_else(|| event.tx.signatures.first().copied())
}

/// Names one plugin callback family for queue-drop telemetry and worker logs.
#[derive(Clone, Copy, Debug)]
pub(super) enum PluginHookKind {
    /// Raw packet callbacks.
    RawPacket,
    /// Shred callbacks.
    Shred,
    /// Dataset callbacks.
    Dataset,
    /// Accepted transaction callbacks.
    Transaction,
    /// Transaction-log callbacks.
    TransactionLog,
    /// Transaction-status callbacks.
    TransactionStatus,
    /// Completed-dataset transaction-batch callbacks.
    TransactionBatch,
    /// Completed-dataset transaction-view-batch callbacks.
    TransactionViewBatch,
    /// Account-touch callbacks.
    AccountTouch,
    /// Account-update callbacks.
    AccountUpdate,
    /// Block-meta callbacks.
    BlockMeta,
    /// Slot-status callbacks.
    SlotStatus,
    /// Reorg callbacks.
    Reorg,
    /// Recent-blockhash callbacks.
    RecentBlockhash,
    /// Cluster-topology callbacks.
    ClusterTopology,
    /// Leader-schedule callbacks.
    LeaderSchedule,
}

impl PluginHookKind {
    /// Returns a stable label for logs and metrics.
    const fn as_str(self) -> &'static str {
        match self {
            Self::RawPacket => "on_raw_packet",
            Self::Shred => "on_shred",
            Self::Dataset => "on_dataset",
            Self::Transaction => "on_transaction",
            Self::TransactionLog => "on_transaction_log",
            Self::TransactionStatus => "on_transaction_status",
            Self::TransactionBatch => "on_transaction_batch",
            Self::TransactionViewBatch => "on_transaction_view_batch",
            Self::AccountTouch => "on_account_touch",
            Self::AccountUpdate => "on_account_update",
            Self::BlockMeta => "on_block_meta",
            Self::SlotStatus => "on_slot_status",
            Self::Reorg => "on_reorg",
            Self::RecentBlockhash => "on_recent_blockhash",
            Self::ClusterTopology => "on_cluster_topology",
            Self::LeaderSchedule => "on_leader_schedule",
        }
    }
}

/// Classifies why a plugin event could not enter its bounded queue.
#[derive(Clone, Copy, Debug)]
enum PluginDispatchFailureReason {
    /// The queue was full and the hot path dropped the event.
    QueueFull,
    /// The worker closed and can no longer accept events.
    QueueClosed,
}

impl PluginDispatchFailureReason {
    /// Returns a stable label for logs and metrics.
    const fn as_str(self) -> &'static str {
        match self {
            Self::QueueFull => "queue full",
            Self::QueueClosed => "queue closed",
        }
    }
}

/// Precomputed plugin targets per hook family so queued events only touch interested plugins.
#[derive(Clone)]
pub(super) struct PluginDispatchTargets {
    /// Plugins interested in raw-packet callbacks.
    pub(super) raw_packet: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in shred callbacks.
    pub(super) shred: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in dataset callbacks.
    pub(super) dataset: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in transaction-log callbacks.
    pub(super) transaction_log: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in transaction-status callbacks.
    pub(super) transaction_status: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in transaction-batch callbacks.
    pub(super) transaction_batch: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in transaction-view-batch callbacks.
    pub(super) transaction_view_batch: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in account-touch callbacks.
    pub(super) account_touch: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in account-update callbacks.
    pub(super) account_update: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in block-meta callbacks.
    pub(super) block_meta: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in slot-status callbacks.
    pub(super) slot_status: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in reorg callbacks.
    pub(super) reorg: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in recent-blockhash callbacks.
    pub(super) recent_blockhash: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in cluster-topology callbacks.
    pub(super) cluster_topology: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in leader-schedule callbacks.
    pub(super) leader_schedule: Arc<[Arc<dyn ObserverPlugin>]>,
}

impl PluginDispatchTargets {
    /// Returns true when no non-transaction hooks are registered.
    pub(super) fn is_empty(&self) -> bool {
        self.raw_packet.is_empty()
            && self.shred.is_empty()
            && self.dataset.is_empty()
            && self.transaction_log.is_empty()
            && self.transaction_status.is_empty()
            && self.transaction_batch.is_empty()
            && self.transaction_view_batch.is_empty()
            && self.account_touch.is_empty()
            && self.account_update.is_empty()
            && self.block_meta.is_empty()
            && self.slot_status.is_empty()
            && self.reorg.is_empty()
            && self.recent_blockhash.is_empty()
            && self.cluster_topology.is_empty()
            && self.leader_schedule.is_empty()
    }

    /// Returns the interested plugin slice for one hook family.
    fn plugins_for(&self, hook: PluginHookKind) -> &[Arc<dyn ObserverPlugin>] {
        match hook {
            PluginHookKind::RawPacket => &self.raw_packet,
            PluginHookKind::Shred => &self.shred,
            PluginHookKind::Dataset => &self.dataset,
            PluginHookKind::Transaction => &[],
            PluginHookKind::TransactionLog => &self.transaction_log,
            PluginHookKind::TransactionStatus => &self.transaction_status,
            PluginHookKind::TransactionBatch => &[],
            PluginHookKind::TransactionViewBatch => &[],
            PluginHookKind::AccountTouch => &self.account_touch,
            PluginHookKind::AccountUpdate => &self.account_update,
            PluginHookKind::BlockMeta => &self.block_meta,
            PluginHookKind::SlotStatus => &self.slot_status,
            PluginHookKind::Reorg => &self.reorg,
            PluginHookKind::RecentBlockhash => &self.recent_blockhash,
            PluginHookKind::ClusterTopology => &self.cluster_topology,
            PluginHookKind::LeaderSchedule => &self.leader_schedule,
        }
    }
}

#[derive(Clone)]
/// Shared sender/counters for asynchronous plugin event delivery.
pub(super) struct PluginDispatcher {
    /// Bounded queue sender for plugin dispatch events.
    tx: EventDispatchSender,
    /// Counter of hook events dropped due to queue pressure or closure.
    dropped_events: Arc<AtomicU64>,
    /// Current queue depth.
    queue_depth: Arc<AtomicU64>,
    /// Maximum queue depth observed since startup.
    max_queue_depth: Arc<AtomicU64>,
}

impl PluginDispatcher {
    /// Creates a dispatcher and background worker when at least one plugin is registered.
    pub(super) fn new(
        targets: PluginDispatchTargets,
        queue_capacity: usize,
        dispatch_mode: PluginDispatchMode,
    ) -> Option<Self> {
        if targets.is_empty() {
            return None;
        }
        let (tx, rx) = create_event_dispatch_queue(queue_capacity.max(1));
        let dropped_events = Arc::new(AtomicU64::new(0));
        let queue_depth = Arc::clone(&tx.queue.queue_depth);
        let max_queue_depth = Arc::clone(&tx.queue.max_queue_depth);
        spawn_dispatch_worker(targets, rx, dispatch_mode.normalized());
        Some(Self {
            tx,
            dropped_events,
            queue_depth,
            max_queue_depth,
        })
    }

    /// Attempts non-blocking enqueue of one hook event.
    pub(super) fn dispatch(&self, event: PluginDispatchEvent) {
        let hook = event.hook_kind();
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(StdTrySendError::Full(_)) => {
                self.record_drop(hook, PluginDispatchFailureReason::QueueFull);
            }
            Err(StdTrySendError::Disconnected(_)) => {
                self.record_drop(hook, PluginDispatchFailureReason::QueueClosed);
            }
        }
    }

    /// Returns total number of dropped hook events.
    pub(super) fn dropped_count(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    /// Returns current queue depth across the general plugin dispatcher.
    pub(super) fn queue_depth(&self) -> u64 {
        self.queue_depth.load(Ordering::Relaxed)
    }

    /// Returns maximum queue depth observed since startup.
    pub(super) fn max_queue_depth(&self) -> u64 {
        self.max_queue_depth.load(Ordering::Relaxed)
    }

    /// Increments dropped counters and emits sampled warning logs.
    fn record_drop(&self, hook: PluginHookKind, reason: PluginDispatchFailureReason) {
        let dropped = self
            .dropped_events
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if dropped <= INITIAL_DROP_LOG_LIMIT || dropped.is_multiple_of(DROP_LOG_SAMPLE_EVERY) {
            tracing::warn!(
                hook = hook.as_str(),
                reason = reason.as_str(),
                dropped,
                "dropping plugin hook event to protect ingest hot path"
            );
        }
    }
}

/// One completed-dataset transaction-batch callback fan-out unit.
pub(super) enum AcceptedTransactionBatchDispatch {
    /// One interested plugin receives the batch directly.
    Single {
        /// Interested plugin.
        plugin: Arc<dyn ObserverPlugin>,
        /// Decoded batch payload.
        event: crate::framework::TransactionBatchEvent,
        /// Time when the completed dataset became available to runtime processing.
        completed_at: Instant,
    },
    /// Multiple interested plugins share the same batch payload.
    Multi {
        /// Interested plugins in registration order.
        plugins: Arc<[Arc<dyn ObserverPlugin>]>,
        /// Shared decoded batch payload.
        event: Arc<crate::framework::TransactionBatchEvent>,
        /// Time when the completed dataset became available to runtime processing.
        completed_at: Instant,
        /// Whether all batch consumers prefer the inline low-jitter batch lane.
        prefers_inline: bool,
    },
}

/// One completed-dataset transaction-view-batch callback fan-out unit.
pub(super) enum AcceptedTransactionViewBatchDispatch {
    /// One interested plugin receives the batch directly.
    Single {
        /// Interested plugin.
        plugin: Arc<dyn ObserverPlugin>,
        /// Validated transaction-view batch payload.
        event: crate::framework::TransactionViewBatchEvent,
        /// Time when the completed dataset became available to runtime processing.
        completed_at: Instant,
    },
    /// Multiple interested plugins share the same batch payload.
    Multi {
        /// Interested plugins in registration order.
        plugins: Arc<[Arc<dyn ObserverPlugin>]>,
        /// Shared validated transaction-view batch payload.
        event: Arc<crate::framework::TransactionViewBatchEvent>,
        /// Time when the completed dataset became available to runtime processing.
        completed_at: Instant,
        /// Whether all view-batch consumers prefer the inline low-jitter batch lane.
        prefers_inline: bool,
    },
}

/// Preclassified completed-dataset transaction-batch routing target.
pub(super) struct ClassifiedTransactionBatchDispatch;

impl ClassifiedTransactionBatchDispatch {
    /// Builds one shared batch dispatch only when at least one plugin subscribed.
    pub(super) fn from_plugins(
        plugins: &[Arc<dyn ObserverPlugin>],
        commitment_selectors: &[crate::framework::plugin::TransactionCommitmentSelector],
        inline_preferences: &[bool],
        event: crate::framework::TransactionBatchEvent,
        completed_at: Instant,
    ) -> Option<AcceptedTransactionBatchDispatch> {
        let selected_indices: SmallVec<[usize; 4]> = commitment_selectors
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(index, selector)| {
                selector.matches(event.commitment_status).then_some(index)
            })
            .collect();
        match selected_indices.len() {
            0 => None,
            1 => Some(AcceptedTransactionBatchDispatch::Single {
                plugin: Arc::clone(plugins.get(*selected_indices.first()?)?),
                event,
                completed_at,
            }),
            _ => Some(AcceptedTransactionBatchDispatch::Multi {
                plugins: Arc::from(
                    selected_indices
                        .iter()
                        .filter_map(|index| plugins.get(*index).cloned())
                        .collect::<Vec<_>>(),
                ),
                event: Arc::new(event),
                completed_at,
                prefers_inline: selected_indices
                    .iter()
                    .all(|index| inline_preferences.get(*index).copied().unwrap_or(false)),
            }),
        }
    }
}

/// Preclassified completed-dataset transaction-view-batch routing target.
pub(super) struct ClassifiedTransactionViewBatchDispatch;

impl ClassifiedTransactionViewBatchDispatch {
    /// Builds one shared view-batch dispatch only when at least one plugin subscribed.
    pub(super) fn from_plugins(
        plugins: &[Arc<dyn ObserverPlugin>],
        commitment_selectors: &[crate::framework::plugin::TransactionCommitmentSelector],
        inline_preferences: &[bool],
        event: crate::framework::TransactionViewBatchEvent,
        completed_at: Instant,
    ) -> Option<AcceptedTransactionViewBatchDispatch> {
        let selected_indices: SmallVec<[usize; 4]> = commitment_selectors
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(index, selector)| {
                selector.matches(event.commitment_status).then_some(index)
            })
            .collect();
        match selected_indices.len() {
            0 => None,
            1 => Some(AcceptedTransactionViewBatchDispatch::Single {
                plugin: Arc::clone(plugins.get(*selected_indices.first()?)?),
                event,
                completed_at,
            }),
            _ => Some(AcceptedTransactionViewBatchDispatch::Multi {
                plugins: Arc::from(
                    selected_indices
                        .iter()
                        .filter_map(|index| plugins.get(*index).cloned())
                        .collect::<Vec<_>>(),
                ),
                event: Arc::new(event),
                completed_at,
                prefers_inline: selected_indices
                    .iter()
                    .all(|index| inline_preferences.get(*index).copied().unwrap_or(false)),
            }),
        }
    }
}

/// One accepted-transaction callback fan-out unit.
pub(super) enum AcceptedTransactionDispatch {
    /// One interested plugin receives the event directly.
    SingleOwned {
        /// Interested plugin.
        plugin: Arc<dyn ObserverPlugin>,
        /// Accepted transaction payload.
        event: TransactionEvent,
        /// Time when the completed dataset became available to runtime processing.
        completed_at: Instant,
        /// Time when the first shred for this completed dataset entered runtime processing.
        first_shred_observed_at: Instant,
        /// Time when the last shred for this completed dataset entered runtime processing.
        last_shred_observed_at: Instant,
        /// Whether inline dispatch came from early prefix reconstruction or dataset fallback.
        inline_source: InlineTransactionDispatchSource,
        /// Delivery priority assigned by the host.
        interest: TransactionInterest,
        /// Number of decoded transactions in the completed dataset that produced this event.
        dataset_tx_count: u32,
        /// Zero-based transaction position inside the completed dataset.
        dataset_tx_position: u32,
    },
    /// Multiple interested plugins share the same event payload.
    Multi {
        /// Interested plugins.
        plugins: SmallVec<[Arc<dyn ObserverPlugin>; 2]>,
        /// Accepted transaction payload.
        event: Arc<TransactionEvent>,
        /// Time when the completed dataset became available to runtime processing.
        completed_at: Instant,
        /// Time when the first shred for this completed dataset entered runtime processing.
        first_shred_observed_at: Instant,
        /// Time when the last shred for this completed dataset entered runtime processing.
        last_shred_observed_at: Instant,
        /// Whether inline dispatch came from early prefix reconstruction or dataset fallback.
        inline_source: InlineTransactionDispatchSource,
        /// Delivery priority assigned by the host.
        interest: TransactionInterest,
        /// Number of decoded transactions in the completed dataset that produced this event.
        dataset_tx_count: u32,
        /// Zero-based transaction position inside the completed dataset.
        dataset_tx_position: u32,
    },
}

/// Preclassified accepted-transaction routing targets.
pub(crate) enum ClassifiedTransactionDispatch {
    /// No registered plugin is interested in this transaction.
    Empty,
    /// Exactly one plugin is interested, so the hot path stays allocation-free.
    Single {
        /// Interested plugin.
        plugin: Arc<dyn ObserverPlugin>,
        /// Delivery priority assigned by the host.
        interest: TransactionInterest,
        /// Whether this plugin prefers the inline critical lane.
        prefers_inline: bool,
    },
    /// Multiple interested plugins require explicit per-lane fanout storage.
    Multi {
        /// Interested critical plugins that prefer the inline low-jitter lane.
        critical_inline_plugins: SmallVec<[Arc<dyn ObserverPlugin>; 2]>,
        /// Interested critical-lane plugins.
        critical_plugins: SmallVec<[Arc<dyn ObserverPlugin>; 2]>,
        /// Interested background-lane plugins.
        background_plugins: SmallVec<[Arc<dyn ObserverPlugin>; 2]>,
    },
}

impl ClassifiedTransactionDispatch {
    /// Builds an empty routing table.
    pub(crate) const fn empty() -> Self {
        Self::Empty
    }

    /// Returns true when no plugin is interested in this transaction.
    pub(crate) const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// Records one plugin under the requested priority.
    pub(crate) fn push(
        &mut self,
        interest: TransactionInterest,
        prefers_inline: bool,
        plugin: Arc<dyn ObserverPlugin>,
    ) {
        if interest == TransactionInterest::Ignore {
            return;
        }
        match self {
            Self::Empty => {
                *self = Self::Single {
                    plugin,
                    interest,
                    prefers_inline,
                };
            }
            Self::Single { .. } => {
                let previous = std::mem::replace(self, Self::empty());
                let mut upgraded = Self::multi();
                if let Self::Single {
                    plugin: previous_plugin,
                    interest: previous_interest,
                    prefers_inline: previous_prefers_inline,
                } = previous
                {
                    upgraded.push_multi(
                        previous_plugin,
                        previous_interest,
                        previous_prefers_inline,
                    );
                }
                upgraded.push_multi(plugin, interest, prefers_inline);
                *self = upgraded;
            }
            Self::Multi { .. } => {
                self.push_multi(plugin, interest, prefers_inline);
            }
        }
    }

    /// Converts the routing table into queued dispatch envelopes.
    #[expect(
        clippy::too_many_arguments,
        reason = "dispatch envelope construction keeps timing metadata explicit on the hot path"
    )]
    pub(super) fn into_dispatches(
        self,
        event: TransactionEvent,
        completed_at: Instant,
        first_shred_observed_at: Instant,
        last_shred_observed_at: Instant,
        inline_source: InlineTransactionDispatchSource,
        dataset_tx_count: u32,
        dataset_tx_position: u32,
    ) -> (
        Option<AcceptedTransactionDispatch>,
        Option<AcceptedTransactionDispatch>,
        Option<AcceptedTransactionDispatch>,
    ) {
        match self {
            Self::Empty => (None, None, None),
            Self::Single {
                plugin,
                interest,
                prefers_inline,
            } => {
                let dispatch = Some(AcceptedTransactionDispatch::SingleOwned {
                    plugin,
                    event,
                    completed_at,
                    first_shred_observed_at,
                    last_shred_observed_at,
                    inline_source,
                    interest,
                    dataset_tx_count,
                    dataset_tx_position,
                });
                match interest {
                    TransactionInterest::Critical if prefers_inline => (dispatch, None, None),
                    TransactionInterest::Critical => (None, dispatch, None),
                    TransactionInterest::Background => (None, None, dispatch),
                    TransactionInterest::Ignore => (None, None, None),
                }
            }
            Self::Multi {
                critical_inline_plugins,
                critical_plugins,
                background_plugins,
            } => {
                let has_critical_inline = !critical_inline_plugins.is_empty();
                let has_critical = !critical_plugins.is_empty();
                let has_background = !background_plugins.is_empty();
                let mut remaining_lanes = 0usize;
                if has_critical_inline {
                    remaining_lanes = remaining_lanes.saturating_add(1);
                }
                if has_critical {
                    remaining_lanes = remaining_lanes.saturating_add(1);
                }
                if has_background {
                    remaining_lanes = remaining_lanes.saturating_add(1);
                }
                let fallback_event = event.clone();
                let mut shared_event = Some(event);
                let mut next_lane_event = || -> Option<TransactionEvent> {
                    remaining_lanes = remaining_lanes.saturating_sub(1);
                    if remaining_lanes == 0 {
                        shared_event.take()
                    } else {
                        shared_event.as_ref().cloned()
                    }
                };
                let critical_inline = if has_critical_inline {
                    AcceptedTransactionDispatch::from_plugins(
                        critical_inline_plugins,
                        next_lane_event().unwrap_or_else(|| fallback_event.clone()),
                        completed_at,
                        first_shred_observed_at,
                        last_shred_observed_at,
                        inline_source,
                        TransactionInterest::Critical,
                        dataset_tx_count,
                        dataset_tx_position,
                    )
                } else {
                    None
                };
                let critical = if has_critical {
                    AcceptedTransactionDispatch::from_plugins(
                        critical_plugins,
                        next_lane_event().unwrap_or_else(|| fallback_event.clone()),
                        completed_at,
                        first_shred_observed_at,
                        last_shred_observed_at,
                        inline_source,
                        TransactionInterest::Critical,
                        dataset_tx_count,
                        dataset_tx_position,
                    )
                } else {
                    None
                };
                let background = if has_background {
                    AcceptedTransactionDispatch::from_plugins(
                        background_plugins,
                        next_lane_event().unwrap_or_else(|| fallback_event.clone()),
                        completed_at,
                        first_shred_observed_at,
                        last_shred_observed_at,
                        inline_source,
                        TransactionInterest::Background,
                        dataset_tx_count,
                        dataset_tx_position,
                    )
                } else {
                    None
                };
                (critical_inline, critical, background)
            }
        }
    }

    /// Builds per-lane fanout storage only when the routing result is no longer singular.
    fn multi() -> Self {
        Self::Multi {
            critical_inline_plugins: SmallVec::new(),
            critical_plugins: SmallVec::new(),
            background_plugins: SmallVec::new(),
        }
    }

    /// Appends one interested plugin into the explicit multi-plugin lane storage.
    fn push_multi(
        &mut self,
        plugin: Arc<dyn ObserverPlugin>,
        interest: TransactionInterest,
        prefers_inline: bool,
    ) {
        let Self::Multi {
            critical_inline_plugins,
            critical_plugins,
            background_plugins,
        } = self
        else {
            return;
        };
        match interest {
            TransactionInterest::Ignore => {}
            TransactionInterest::Critical if prefers_inline => {
                critical_inline_plugins.push(plugin);
            }
            TransactionInterest::Background => background_plugins.push(plugin),
            TransactionInterest::Critical => critical_plugins.push(plugin),
        }
    }
}

impl AcceptedTransactionDispatch {
    /// Builds a dispatch envelope only when at least one plugin is interested.
    #[expect(
        clippy::too_many_arguments,
        reason = "accepted transaction fanout preserves explicit timing and dataset metadata"
    )]
    pub(super) fn from_plugins(
        plugins: SmallVec<[Arc<dyn ObserverPlugin>; 2]>,
        event: TransactionEvent,
        completed_at: Instant,
        first_shred_observed_at: Instant,
        last_shred_observed_at: Instant,
        inline_source: InlineTransactionDispatchSource,
        interest: TransactionInterest,
        dataset_tx_count: u32,
        dataset_tx_position: u32,
    ) -> Option<Self> {
        match plugins.len() {
            0 => None,
            1 => Some(Self::SingleOwned {
                plugin: plugins.into_iter().next()?,
                event,
                completed_at,
                first_shred_observed_at,
                last_shred_observed_at,
                inline_source,
                interest,
                dataset_tx_count,
                dataset_tx_position,
            }),
            _ => Some(Self::Multi {
                plugins,
                event: Arc::new(event),
                completed_at,
                first_shred_observed_at,
                last_shred_observed_at,
                inline_source,
                interest,
                dataset_tx_count,
                dataset_tx_position,
            }),
        }
    }

    /// Derives a stable shard key so related transactions stay ordered per worker.
    fn shard_key(&self) -> u64 {
        let event = match self {
            Self::SingleOwned { event, .. } => event,
            Self::Multi { event, .. } => event.as_ref(),
        };
        let mut hasher = DefaultHasher::new();
        event.slot.hash(&mut hasher);
        if let Some(signature) = event
            .signature
            .map(sof_types::SignatureBytes::to_solana)
            .or_else(|| event.tx.signatures.first().copied())
        {
            signature.hash(&mut hasher);
        }
        hasher.finish()
    }
}

#[derive(Clone)]
/// Sharded dispatcher for accepted-transaction callbacks.
pub(super) struct TransactionPluginDispatcher {
    /// Bounded low-jitter queue for inline-preferred critical transaction hooks.
    inline_critical_txs: Arc<[TxDispatchSender]>,
    /// Bounded per-worker queues for critical transaction hooks.
    critical_txs: Arc<[TxDispatchSender]>,
    /// Bounded per-worker queues for background transaction hooks.
    background_txs: Arc<[TxDispatchSender]>,
    /// Total critical transaction hook drops.
    critical_dropped_events: Arc<AtomicU64>,
    /// Total background transaction hook drops.
    background_dropped_events: Arc<AtomicU64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TransactionDispatchQueueMetrics {
    /// Current queue depth for the inline-critical lane.
    pub(crate) inline_critical_queue_depth: u64,
    /// Maximum queue depth observed for the inline-critical lane.
    pub(crate) inline_critical_max_queue_depth: u64,
    /// Current aggregate queue depth across critical lanes.
    pub(crate) critical_queue_depth: u64,
    /// Maximum aggregate queue depth observed across critical lanes.
    pub(crate) critical_max_queue_depth: u64,
    /// Current aggregate queue depth across background lanes.
    pub(crate) background_queue_depth: u64,
    /// Maximum aggregate queue depth observed across background lanes.
    pub(crate) background_max_queue_depth: u64,
}

impl TransactionPluginDispatcher {
    /// Creates per-priority worker queues for accepted transaction callbacks.
    pub(super) fn new(
        queue_capacity: usize,
        worker_count: usize,
        dispatch_mode: PluginDispatchMode,
    ) -> Self {
        let worker_count = worker_count.max(1);
        let inline_worker_count = 1;
        let background_worker_count = worker_count.clamp(1, 4);
        let inline_queue_capacity = queue_capacity.max(1);
        let critical_queue_capacity = queue_capacity.div_ceil(worker_count).max(1);
        let background_queue_capacity = queue_capacity.div_ceil(background_worker_count).max(1);
        let critical_dropped_events = Arc::new(AtomicU64::new(0));
        let background_dropped_events = Arc::new(AtomicU64::new(0));
        let mut inline_critical_txs = Vec::with_capacity(inline_worker_count);
        for worker_index in 0..inline_worker_count {
            let (tx, rx) = create_tx_dispatch_queue(inline_queue_capacity);
            spawn_transaction_dispatch_worker(
                worker_index,
                TransactionDispatchPriority::InlineCritical,
                rx,
                dispatch_mode.normalized(),
            );
            inline_critical_txs.push(tx);
        }
        let mut critical_txs = Vec::with_capacity(worker_count);
        for worker_index in 0..worker_count {
            let (tx, rx) = create_tx_dispatch_queue(critical_queue_capacity);
            spawn_transaction_dispatch_worker(
                worker_index,
                TransactionDispatchPriority::Critical,
                rx,
                dispatch_mode.normalized(),
            );
            critical_txs.push(tx);
        }
        let mut background_txs = Vec::with_capacity(background_worker_count);
        for worker_index in 0..background_worker_count {
            let (tx, rx) = create_tx_dispatch_queue(background_queue_capacity);
            spawn_transaction_dispatch_worker(
                worker_index,
                TransactionDispatchPriority::Background,
                rx,
                dispatch_mode.normalized(),
            );
            background_txs.push(tx);
        }
        Self {
            inline_critical_txs: Arc::from(inline_critical_txs),
            critical_txs: Arc::from(critical_txs),
            background_txs: Arc::from(background_txs),
            critical_dropped_events,
            background_dropped_events,
        }
    }

    /// Routes one inline-preferred critical transaction onto the low-jitter lane.
    pub(super) fn dispatch_inline_critical(&self, event: AcceptedTransactionDispatch) {
        self.dispatch_to_lane(
            TransactionDispatchPriority::InlineCritical,
            &self.inline_critical_txs,
            0,
            event,
        );
    }

    /// Routes one accepted transaction to its priority queue shard.
    pub(super) fn dispatch(
        &self,
        priority: TransactionDispatchPriority,
        event: AcceptedTransactionDispatch,
    ) {
        let txs = match priority {
            TransactionDispatchPriority::InlineCritical => &self.inline_critical_txs,
            TransactionDispatchPriority::Background => &self.background_txs,
            TransactionDispatchPriority::Critical => &self.critical_txs,
        };
        let Some(shard_count) = NonZeroUsize::new(txs.len()) else {
            self.record_drop(priority, PluginDispatchFailureReason::QueueClosed);
            return;
        };
        let shard = transaction_dispatch_shard(event.shard_key(), shard_count);
        self.dispatch_to_lane(priority, txs, shard, event);
    }

    /// Attempts non-blocking enqueue into one transaction lane shard.
    fn dispatch_to_lane(
        &self,
        priority: TransactionDispatchPriority,
        txs: &[TxDispatchSender],
        shard: usize,
        event: AcceptedTransactionDispatch,
    ) {
        let Some(tx) = txs.get(shard) else {
            self.record_drop(priority, PluginDispatchFailureReason::QueueClosed);
            return;
        };
        match tx.try_send(event) {
            Ok(()) => {}
            Err(StdTrySendError::Full(_)) => {
                self.record_drop(priority, PluginDispatchFailureReason::QueueFull);
            }
            Err(StdTrySendError::Disconnected(_)) => {
                self.record_drop(priority, PluginDispatchFailureReason::QueueClosed);
            }
        }
    }

    /// Returns dropped critical transaction callbacks.
    pub(super) fn critical_dropped_count(&self) -> u64 {
        self.critical_dropped_events.load(Ordering::Relaxed)
    }

    /// Returns dropped background transaction callbacks.
    pub(super) fn background_dropped_count(&self) -> u64 {
        self.background_dropped_events.load(Ordering::Relaxed)
    }

    /// Returns queue-depth telemetry aggregated by transaction-dispatch lane.
    pub(super) fn queue_metrics(&self) -> TransactionDispatchQueueMetrics {
        TransactionDispatchQueueMetrics {
            inline_critical_queue_depth: self
                .inline_critical_txs
                .iter()
                .map(TxDispatchSender::queue_depth)
                .sum(),
            inline_critical_max_queue_depth: self
                .inline_critical_txs
                .iter()
                .map(TxDispatchSender::max_queue_depth)
                .max()
                .unwrap_or(0),
            critical_queue_depth: self
                .critical_txs
                .iter()
                .map(TxDispatchSender::queue_depth)
                .sum(),
            critical_max_queue_depth: self
                .critical_txs
                .iter()
                .map(TxDispatchSender::max_queue_depth)
                .sum(),
            background_queue_depth: self
                .background_txs
                .iter()
                .map(TxDispatchSender::queue_depth)
                .sum(),
            background_max_queue_depth: self
                .background_txs
                .iter()
                .map(TxDispatchSender::max_queue_depth)
                .sum(),
        }
    }

    /// Records one dropped transaction callback.
    fn record_drop(
        &self,
        priority: TransactionDispatchPriority,
        reason: PluginDispatchFailureReason,
    ) {
        let dropped = match priority {
            TransactionDispatchPriority::InlineCritical | TransactionDispatchPriority::Critical => {
                self.critical_dropped_events
                    .fetch_add(1, Ordering::Relaxed)
                    .saturating_add(1)
            }
            TransactionDispatchPriority::Background => self
                .background_dropped_events
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1),
        };
        if dropped <= INITIAL_DROP_LOG_LIMIT || dropped.is_multiple_of(DROP_LOG_SAMPLE_EVERY) {
            tracing::warn!(
                hook = PluginHookKind::Transaction.as_str(),
                priority = priority.as_str(),
                reason = reason.as_str(),
                dropped,
                "dropping accepted transaction hook event to protect ingest hot path"
            );
        }
    }
}

/// Maps a stable transaction shard key onto one worker queue.
fn transaction_dispatch_shard(shard_key: u64, worker_count: NonZeroUsize) -> usize {
    let worker_count_u64 = u64::try_from(worker_count.get()).unwrap_or(1);
    let worker_index = shard_key.checked_rem(worker_count_u64).unwrap_or(0);
    usize::try_from(worker_index).unwrap_or(0)
}

/// Internal event envelope sent to the plugin worker queue.
pub(super) enum PluginDispatchEvent {
    /// Packet ingress callback payload.
    RawPacket(RawPacketEvent),
    /// Parsed shred callback payload.
    Shred(ShredEvent),
    /// Reassembled dataset callback payload.
    Dataset(DatasetEvent),
    /// Provider/websocket transaction-log callback payload targeted to a subset of plugins.
    SelectedTransactionLog(SelectedTransactionLogDispatch),
    /// Provider transaction-status callback payload targeted to a subset of plugins.
    SelectedTransactionStatus(SelectedTransactionStatusDispatch),
    /// Completed-dataset transaction-batch callback payload.
    TransactionBatch(AcceptedTransactionBatchDispatch),
    /// Completed-dataset transaction-view-batch callback payload.
    TransactionViewBatch(AcceptedTransactionViewBatchDispatch),
    /// Static account-touch callback payload.
    AccountTouch(Arc<AccountTouchEvent>),
    /// Static account-touch callback payload targeted to a subset of plugins.
    SelectedAccountTouch(SelectedAccountTouchDispatch),
    /// Upstream account-update callback payload targeted to a subset of plugins.
    SelectedAccountUpdate(SelectedAccountUpdateDispatch),
    /// Upstream block-meta callback payload targeted to a subset of plugins.
    SelectedBlockMeta(SelectedBlockMetaDispatch),
    /// Slot-status callback payload.
    SlotStatus(SlotStatusEvent),
    /// Canonical reorg callback payload.
    Reorg(ReorgEvent),
    /// Observed recent blockhash callback payload.
    ObservedRecentBlockhash(ObservedRecentBlockhashEvent),
    /// Cluster topology callback payload.
    ClusterTopology(ClusterTopologyEvent),
    /// Leader schedule callback payload.
    LeaderSchedule(LeaderScheduleEvent),
}

impl PluginDispatchEvent {
    /// Returns the callback family associated with this queued event.
    const fn hook_kind(&self) -> PluginHookKind {
        match self {
            Self::RawPacket(_) => PluginHookKind::RawPacket,
            Self::Shred(_) => PluginHookKind::Shred,
            Self::Dataset(_) => PluginHookKind::Dataset,
            Self::SelectedTransactionLog(_) => PluginHookKind::TransactionLog,
            Self::SelectedTransactionStatus(_) => PluginHookKind::TransactionStatus,
            Self::TransactionBatch(_) => PluginHookKind::TransactionBatch,
            Self::TransactionViewBatch(_) => PluginHookKind::TransactionViewBatch,
            Self::AccountTouch(_) => PluginHookKind::AccountTouch,
            Self::SelectedAccountTouch(_) => PluginHookKind::AccountTouch,
            Self::SelectedAccountUpdate(_) => PluginHookKind::AccountUpdate,
            Self::SelectedBlockMeta(_) => PluginHookKind::BlockMeta,
            Self::SlotStatus(_) => PluginHookKind::SlotStatus,
            Self::Reorg(_) => PluginHookKind::Reorg,
            Self::ObservedRecentBlockhash(_) => PluginHookKind::RecentBlockhash,
            Self::ClusterTopology(_) => PluginHookKind::ClusterTopology,
            Self::LeaderSchedule(_) => PluginHookKind::LeaderSchedule,
        }
    }
}

/// One selected account-touch callback fan-out unit.
pub(super) enum SelectedAccountTouchDispatch {
    /// One interested plugin receives the event directly.
    Single {
        /// Selected plugin callback target.
        plugin: Arc<dyn ObserverPlugin>,
        /// Event payload for the selected plugin.
        event: AccountTouchEvent,
    },
    /// Multiple interested plugins share the same event payload.
    Multi {
        /// Selected plugin callback targets.
        plugins: SmallVec<[Arc<dyn ObserverPlugin>; 2]>,
        /// Shared event payload for the selected plugin batch.
        event: Arc<AccountTouchEvent>,
    },
}

/// One selected transaction-log callback fan-out unit.
pub(super) enum SelectedTransactionLogDispatch {
    /// One interested plugin receives the event directly.
    Single {
        /// Selected plugin callback target.
        plugin: Arc<dyn ObserverPlugin>,
        /// Event payload for the selected plugin.
        event: crate::framework::TransactionLogEvent,
    },
    /// Multiple interested plugins share the same event payload.
    Multi {
        /// Selected plugin callback targets.
        plugins: SmallVec<[Arc<dyn ObserverPlugin>; 4]>,
        /// Shared event payload.
        event: Arc<crate::framework::TransactionLogEvent>,
    },
}

/// One selected transaction-status callback fan-out unit.
pub(super) enum SelectedTransactionStatusDispatch {
    /// One interested plugin receives the event directly.
    Single {
        /// Selected plugin callback target.
        plugin: Arc<dyn ObserverPlugin>,
        /// Event payload for the selected plugin.
        event: TransactionStatusEvent,
    },
    /// Multiple interested plugins share the same event payload.
    Multi {
        /// Selected plugin callback targets.
        plugins: SmallVec<[Arc<dyn ObserverPlugin>; 4]>,
        /// Shared event payload.
        event: Arc<TransactionStatusEvent>,
    },
}

impl SelectedTransactionLogDispatch {
    pub(super) fn from_plugins(
        plugins: &[Arc<dyn ObserverPlugin>],
        commitment_selectors: &[crate::framework::plugin::TransactionCommitmentSelector],
        event: crate::framework::TransactionLogEvent,
    ) -> Option<Self> {
        let interested: SmallVec<[Arc<dyn ObserverPlugin>; 4]> = plugins
            .iter()
            .zip(commitment_selectors.iter().copied())
            .filter(|(_plugin, selector)| selector.matches(event.commitment_status))
            .filter(|(plugin, _selector)| plugin.accepts_transaction_log(&event))
            .map(|(plugin, _minimum)| Arc::clone(plugin))
            .collect();
        match interested.len() {
            0 => None,
            1 => Some(Self::Single {
                plugin: interested.into_iter().next()?,
                event,
            }),
            _ => Some(Self::Multi {
                plugins: interested,
                event: Arc::new(event),
            }),
        }
    }
}

impl SelectedTransactionStatusDispatch {
    pub(super) fn from_plugins(
        plugins: &[Arc<dyn ObserverPlugin>],
        commitment_selectors: &[crate::framework::plugin::TransactionCommitmentSelector],
        event: TransactionStatusEvent,
    ) -> Option<Self> {
        let interested: SmallVec<[Arc<dyn ObserverPlugin>; 4]> = plugins
            .iter()
            .zip(commitment_selectors.iter().copied())
            .filter(|(_plugin, selector)| selector.matches(event.commitment_status))
            .filter(|(plugin, _selector)| plugin.accepts_transaction_status(&event))
            .map(|(plugin, _selector)| Arc::clone(plugin))
            .collect();
        match interested.len() {
            0 => None,
            1 => Some(Self::Single {
                plugin: interested.into_iter().next()?,
                event,
            }),
            _ => Some(Self::Multi {
                plugins: interested,
                event: Arc::new(event),
            }),
        }
    }
}

/// One selected account-update callback fan-out unit.
pub(super) enum SelectedAccountUpdateDispatch {
    /// One interested plugin receives the event directly.
    Single {
        /// Selected plugin callback target.
        plugin: Arc<dyn ObserverPlugin>,
        /// Event payload for the selected plugin.
        event: AccountUpdateEvent,
    },
    /// Multiple interested plugins share the same event payload.
    Multi {
        /// Selected plugin callback targets.
        plugins: SmallVec<[Arc<dyn ObserverPlugin>; 4]>,
        /// Shared event payload.
        event: Arc<AccountUpdateEvent>,
    },
}

impl SelectedAccountUpdateDispatch {
    pub(super) fn from_plugins(
        plugins: &[Arc<dyn ObserverPlugin>],
        event: AccountUpdateEvent,
    ) -> Option<Self> {
        let interested: SmallVec<[Arc<dyn ObserverPlugin>; 4]> = plugins
            .iter()
            .filter(|plugin| plugin.accepts_account_update(&event))
            .map(Arc::clone)
            .collect();
        match interested.len() {
            0 => None,
            1 => Some(Self::Single {
                plugin: interested.into_iter().next()?,
                event,
            }),
            _ => Some(Self::Multi {
                plugins: interested,
                event: Arc::new(event),
            }),
        }
    }
}

/// One selected block-meta callback fan-out unit.
pub(super) enum SelectedBlockMetaDispatch {
    /// One interested plugin receives the event directly.
    Single {
        /// Selected plugin callback target.
        plugin: Arc<dyn ObserverPlugin>,
        /// Event payload for the selected plugin.
        event: BlockMetaEvent,
    },
    /// Multiple interested plugins share the same event payload.
    Multi {
        /// Selected plugin callback targets.
        plugins: SmallVec<[Arc<dyn ObserverPlugin>; 4]>,
        /// Shared event payload.
        event: Arc<BlockMetaEvent>,
    },
}

impl SelectedBlockMetaDispatch {
    pub(super) fn from_plugins(
        plugins: &[Arc<dyn ObserverPlugin>],
        event: BlockMetaEvent,
    ) -> Option<Self> {
        let interested: SmallVec<[Arc<dyn ObserverPlugin>; 4]> = plugins
            .iter()
            .filter(|plugin| plugin.accepts_block_meta(&event))
            .map(Arc::clone)
            .collect();
        match interested.len() {
            0 => None,
            1 => Some(Self::Single {
                plugin: interested.into_iter().next()?,
                event,
            }),
            _ => Some(Self::Multi {
                plugins: interested,
                event: Arc::new(event),
            }),
        }
    }
}

/// Preclassified account-touch routing target.
pub(crate) enum ClassifiedAccountTouchDispatch {
    /// No registered plugin is interested in this account-touch event.
    Empty,
    /// Exactly one plugin is interested in this account-touch event.
    Single {
        /// Interested plugin.
        plugin: Arc<dyn ObserverPlugin>,
    },
    /// Multiple plugins are interested in this account-touch event.
    Multi {
        /// Interested plugins in registration order.
        plugins: SmallVec<[Arc<dyn ObserverPlugin>; 2]>,
    },
}

impl ClassifiedAccountTouchDispatch {
    /// Builds an empty routing table.
    pub(super) const fn empty() -> Self {
        Self::Empty
    }

    /// Returns true when no plugin is interested in this account-touch event.
    pub(crate) const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// Records one interested plugin.
    pub(super) fn push(&mut self, plugin: Arc<dyn ObserverPlugin>) {
        match self {
            Self::Empty => {
                *self = Self::Single { plugin };
            }
            Self::Single { .. } => {
                let previous = std::mem::replace(self, Self::empty());
                let mut plugins = SmallVec::new();
                if let Self::Single {
                    plugin: previous_plugin,
                } = previous
                {
                    plugins.push(previous_plugin);
                }
                plugins.push(plugin);
                *self = Self::Multi { plugins };
            }
            Self::Multi { plugins } => {
                plugins.push(plugin);
            }
        }
    }
}

impl SelectedAccountTouchDispatch {
    /// Builds a selected account-touch dispatch only when at least one plugin is interested.
    pub(super) fn from_classified(
        dispatch: ClassifiedAccountTouchDispatch,
        event: AccountTouchEvent,
    ) -> Option<Self> {
        match dispatch {
            ClassifiedAccountTouchDispatch::Empty => None,
            ClassifiedAccountTouchDispatch::Single { plugin } => {
                Some(Self::Single { plugin, event })
            }
            ClassifiedAccountTouchDispatch::Multi { plugins } => Some(Self::Multi {
                plugins,
                event: Arc::new(event),
            }),
        }
    }
}

/// Spawns a dedicated worker thread that drains non-transaction hook events.
fn spawn_dispatch_worker(
    targets: PluginDispatchTargets,
    mut rx: EventDispatchReceiver,
    dispatch_mode: PluginDispatchMode,
) {
    const DISPATCH_WORKER_DRAIN_BATCH_MAX: usize = 32;
    let spawn_result = thread::Builder::new()
        .name("sof-plugin-dispatch".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let Ok(runtime) = runtime else {
                tracing::error!("failed to create plugin dispatch runtime");
                return;
            };
            rx.register_current_thread();
            while let Some(first_event) = rx.recv() {
                let mut batch = SmallVec::<[PluginDispatchEvent; 8]>::new();
                batch.push(first_event);
                while batch.len() < DISPATCH_WORKER_DRAIN_BATCH_MAX {
                    match rx.try_recv() {
                        Ok(event) => batch.push(event),
                        Err(StdTrySendError::Full(())) | Err(StdTrySendError::Disconnected(())) => {
                            break;
                        }
                    }
                }
                runtime.block_on(async {
                    for event in batch {
                        dispatch_event(&targets, event, dispatch_mode).await;
                    }
                });
            }
        });
    if let Err(error) = spawn_result {
        tracing::error!(error = %error, "failed to spawn plugin dispatch worker");
    }
}

struct EventDispatchQueue {
    ring: ArrayQueue<PluginDispatchEvent>,
    queue_depth: Arc<AtomicU64>,
    max_queue_depth: Arc<AtomicU64>,
    worker_thread: OnceLock<std::thread::Thread>,
    closed: AtomicBool,
    sender_count: AtomicUsize,
}

struct EventDispatchSender {
    queue: Arc<EventDispatchQueue>,
}

struct EventDispatchReceiver {
    queue: Arc<EventDispatchQueue>,
}

fn create_event_dispatch_queue(capacity: usize) -> (EventDispatchSender, EventDispatchReceiver) {
    let queue = Arc::new(EventDispatchQueue {
        ring: ArrayQueue::new(capacity.max(1)),
        queue_depth: Arc::new(AtomicU64::new(0)),
        max_queue_depth: Arc::new(AtomicU64::new(0)),
        worker_thread: OnceLock::new(),
        closed: AtomicBool::new(false),
        sender_count: AtomicUsize::new(1),
    });
    (
        EventDispatchSender {
            queue: Arc::clone(&queue),
        },
        EventDispatchReceiver { queue },
    )
}

impl EventDispatchSender {
    fn try_send(
        &self,
        event: PluginDispatchEvent,
    ) -> Result<(), StdTrySendError<PluginDispatchEvent>> {
        if self.queue.closed.load(Ordering::Acquire) {
            return Err(StdTrySendError::Disconnected(event));
        }
        let queue_depth = self
            .queue
            .queue_depth
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        self.queue
            .max_queue_depth
            .fetch_max(queue_depth, Ordering::Relaxed);
        match self.queue.ring.push(event) {
            Ok(()) => {
                if let Some(thread) = self.queue.worker_thread.get() {
                    thread.unpark();
                }
                Ok(())
            }
            Err(event) => {
                self.queue.queue_depth.fetch_sub(1, Ordering::Relaxed);
                if self.queue.closed.load(Ordering::Acquire) {
                    Err(StdTrySendError::Disconnected(event))
                } else {
                    Err(StdTrySendError::Full(event))
                }
            }
        }
    }
}

impl Clone for EventDispatchSender {
    fn clone(&self) -> Self {
        self.queue.sender_count.fetch_add(1, Ordering::AcqRel);
        Self {
            queue: Arc::clone(&self.queue),
        }
    }
}

impl Drop for EventDispatchSender {
    fn drop(&mut self) {
        if self.queue.sender_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.queue.closed.store(true, Ordering::Release);
            if let Some(thread) = self.queue.worker_thread.get() {
                thread.unpark();
            }
        }
    }
}

impl EventDispatchReceiver {
    fn register_current_thread(&self) {
        drop(self.queue.worker_thread.set(std::thread::current()));
    }

    fn recv(&mut self) -> Option<PluginDispatchEvent> {
        loop {
            if let Some(event) = self.queue.ring.pop() {
                self.queue.queue_depth.fetch_sub(1, Ordering::Relaxed);
                return Some(event);
            }
            if self.queue.closed.load(Ordering::Acquire) {
                return None;
            }
            std::thread::park();
        }
    }

    fn try_recv(&mut self) -> Result<PluginDispatchEvent, StdTrySendError<()>> {
        if let Some(event) = self.queue.ring.pop() {
            self.queue.queue_depth.fetch_sub(1, Ordering::Relaxed);
            return Ok(event);
        }
        if self.queue.closed.load(Ordering::Acquire) {
            Err(StdTrySendError::Disconnected(()))
        } else {
            Err(StdTrySendError::Full(()))
        }
    }
}

/// Spawns one dedicated worker thread for one transaction priority shard.
fn spawn_transaction_dispatch_worker(
    worker_index: usize,
    priority: TransactionDispatchPriority,
    mut rx: TxDispatchReceiver,
    dispatch_mode: PluginDispatchMode,
) {
    const TX_DISPATCH_WORKER_DRAIN_BATCH_MAX: usize = 32;
    let thread_name = format!("sof-plugin-tx-{}-{worker_index:02}", priority.as_str());
    let spawn_result = thread::Builder::new().name(thread_name).spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let Ok(runtime) = runtime else {
            tracing::error!(
                priority = priority.as_str(),
                "failed to create transaction plugin dispatch runtime"
            );
            return;
        };
        rx.register_current_thread();
        while let Some(first_event) = rx.recv() {
            let mut batch = SmallVec::<[AcceptedTransactionDispatch; 8]>::new();
            batch.push(first_event);
            while batch.len() < TX_DISPATCH_WORKER_DRAIN_BATCH_MAX {
                match rx.try_recv() {
                    Ok(event) => batch.push(event),
                    Err(StdTrySendError::Full(())) | Err(StdTrySendError::Disconnected(())) => {
                        break;
                    }
                }
            }
            let mut metrics_batch = TransactionDispatchMetricsBatch::default();
            runtime.block_on(async {
                for event in batch {
                    metrics_batch.observe(
                        dispatch_accepted_transaction_event(event, dispatch_mode, priority).await,
                    );
                }
            });
            crate::runtime_metrics::observe_transaction_dispatch_metrics_batch(&metrics_batch);
        }
    });
    if let Err(error) = spawn_result {
        tracing::error!(
            error = %error,
            priority = priority.as_str(),
            "failed to spawn transaction plugin dispatch worker"
        );
    }
}

struct TxDispatchQueue {
    ring: ArrayQueue<AcceptedTransactionDispatch>,
    queue_depth: Arc<AtomicU64>,
    max_queue_depth: Arc<AtomicU64>,
    worker_thread: OnceLock<std::thread::Thread>,
    closed: AtomicBool,
    sender_count: AtomicUsize,
}

struct TxDispatchSender {
    queue: Arc<TxDispatchQueue>,
}

struct TxDispatchReceiver {
    queue: Arc<TxDispatchQueue>,
}

#[derive(Clone, Copy)]
struct InlineLatencyObservation {
    source: InlineTransactionDispatchSource,
    first_shred_lag_us: u64,
    last_shred_lag_us: u64,
    completed_dataset_lag_us: u64,
}

#[derive(Clone, Copy)]
struct TransactionDispatchObservation {
    visibility_lag_us: u64,
    queue_wait_us: u64,
    callback_duration_us: u64,
    dataset_tx_count: u32,
    dataset_tx_position: u32,
    inline_latency: Option<InlineLatencyObservation>,
}

#[derive(Default)]
pub(crate) struct TransactionDispatchMetricsBatch {
    pub(crate) visibility_samples_total: u64,
    pub(crate) visibility_lag_us_total: u64,
    pub(crate) latest_visibility_lag_us: u64,
    pub(crate) max_visibility_lag_us: u64,
    pub(crate) queue_wait_us_total: u64,
    pub(crate) latest_queue_wait_us: u64,
    pub(crate) max_queue_wait_us: u64,
    pub(crate) callback_duration_us_total: u64,
    pub(crate) latest_callback_duration_us: u64,
    pub(crate) max_callback_duration_us: u64,
    pub(crate) latest_dataset_tx_count: u32,
    pub(crate) max_dataset_tx_count: u32,
    pub(crate) latest_dataset_tx_position: u32,
    pub(crate) max_dataset_tx_position: u32,
    pub(crate) first_in_dataset_samples_total: u64,
    pub(crate) first_in_dataset_queue_wait_us_total: u64,
    pub(crate) max_first_in_dataset_queue_wait_us: u64,
    pub(crate) nonfirst_in_dataset_samples_total: u64,
    pub(crate) nonfirst_in_dataset_queue_wait_us_total: u64,
    pub(crate) max_nonfirst_in_dataset_queue_wait_us: u64,
    pub(crate) inline_latency_samples_total: u64,
    pub(crate) inline_first_shred_lag_us_total: u64,
    pub(crate) latest_inline_first_shred_lag_us: u64,
    pub(crate) max_inline_first_shred_lag_us: u64,
    pub(crate) inline_last_shred_lag_us_total: u64,
    pub(crate) latest_inline_last_shred_lag_us: u64,
    pub(crate) max_inline_last_shred_lag_us: u64,
    pub(crate) inline_completed_dataset_lag_us_total: u64,
    pub(crate) latest_inline_completed_dataset_lag_us: u64,
    pub(crate) max_inline_completed_dataset_lag_us: u64,
    pub(crate) early_prefix_latency_samples_total: u64,
    pub(crate) early_prefix_first_shred_lag_us_total: u64,
    pub(crate) early_prefix_last_shred_lag_us_total: u64,
    pub(crate) early_prefix_completed_dataset_lag_us_total: u64,
    pub(crate) completed_dataset_fallback_latency_samples_total: u64,
    pub(crate) completed_dataset_fallback_first_shred_lag_us_total: u64,
    pub(crate) completed_dataset_fallback_last_shred_lag_us_total: u64,
    pub(crate) completed_dataset_fallback_completed_dataset_lag_us_total: u64,
}

impl TransactionDispatchMetricsBatch {
    fn observe(&mut self, observation: TransactionDispatchObservation) {
        self.visibility_samples_total = self.visibility_samples_total.saturating_add(1);
        self.visibility_lag_us_total = self
            .visibility_lag_us_total
            .saturating_add(observation.visibility_lag_us);
        self.latest_visibility_lag_us = observation.visibility_lag_us;
        self.max_visibility_lag_us = self
            .max_visibility_lag_us
            .max(observation.visibility_lag_us);

        self.queue_wait_us_total = self
            .queue_wait_us_total
            .saturating_add(observation.queue_wait_us);
        self.latest_queue_wait_us = observation.queue_wait_us;
        self.max_queue_wait_us = self.max_queue_wait_us.max(observation.queue_wait_us);

        self.callback_duration_us_total = self
            .callback_duration_us_total
            .saturating_add(observation.callback_duration_us);
        self.latest_callback_duration_us = observation.callback_duration_us;
        self.max_callback_duration_us = self
            .max_callback_duration_us
            .max(observation.callback_duration_us);

        self.latest_dataset_tx_count = observation.dataset_tx_count;
        self.max_dataset_tx_count = self.max_dataset_tx_count.max(observation.dataset_tx_count);
        self.latest_dataset_tx_position = observation.dataset_tx_position;
        self.max_dataset_tx_position = self
            .max_dataset_tx_position
            .max(observation.dataset_tx_position);

        if observation.dataset_tx_position == 0 {
            self.first_in_dataset_samples_total =
                self.first_in_dataset_samples_total.saturating_add(1);
            self.first_in_dataset_queue_wait_us_total = self
                .first_in_dataset_queue_wait_us_total
                .saturating_add(observation.queue_wait_us);
            self.max_first_in_dataset_queue_wait_us = self
                .max_first_in_dataset_queue_wait_us
                .max(observation.queue_wait_us);
        } else {
            self.nonfirst_in_dataset_samples_total =
                self.nonfirst_in_dataset_samples_total.saturating_add(1);
            self.nonfirst_in_dataset_queue_wait_us_total = self
                .nonfirst_in_dataset_queue_wait_us_total
                .saturating_add(observation.queue_wait_us);
            self.max_nonfirst_in_dataset_queue_wait_us = self
                .max_nonfirst_in_dataset_queue_wait_us
                .max(observation.queue_wait_us);
        }

        if let Some(inline) = observation.inline_latency {
            self.inline_latency_samples_total = self.inline_latency_samples_total.saturating_add(1);
            self.inline_first_shred_lag_us_total = self
                .inline_first_shred_lag_us_total
                .saturating_add(inline.first_shred_lag_us);
            self.latest_inline_first_shred_lag_us = inline.first_shred_lag_us;
            self.max_inline_first_shred_lag_us = self
                .max_inline_first_shred_lag_us
                .max(inline.first_shred_lag_us);

            self.inline_last_shred_lag_us_total = self
                .inline_last_shred_lag_us_total
                .saturating_add(inline.last_shred_lag_us);
            self.latest_inline_last_shred_lag_us = inline.last_shred_lag_us;
            self.max_inline_last_shred_lag_us = self
                .max_inline_last_shred_lag_us
                .max(inline.last_shred_lag_us);

            self.inline_completed_dataset_lag_us_total = self
                .inline_completed_dataset_lag_us_total
                .saturating_add(inline.completed_dataset_lag_us);
            self.latest_inline_completed_dataset_lag_us = inline.completed_dataset_lag_us;
            self.max_inline_completed_dataset_lag_us = self
                .max_inline_completed_dataset_lag_us
                .max(inline.completed_dataset_lag_us);

            match inline.source {
                InlineTransactionDispatchSource::EarlyPrefix => {
                    self.early_prefix_latency_samples_total =
                        self.early_prefix_latency_samples_total.saturating_add(1);
                    self.early_prefix_first_shred_lag_us_total = self
                        .early_prefix_first_shred_lag_us_total
                        .saturating_add(inline.first_shred_lag_us);
                    self.early_prefix_last_shred_lag_us_total = self
                        .early_prefix_last_shred_lag_us_total
                        .saturating_add(inline.last_shred_lag_us);
                    self.early_prefix_completed_dataset_lag_us_total = self
                        .early_prefix_completed_dataset_lag_us_total
                        .saturating_add(inline.completed_dataset_lag_us);
                }
                InlineTransactionDispatchSource::CompletedDatasetFallback => {
                    self.completed_dataset_fallback_latency_samples_total = self
                        .completed_dataset_fallback_latency_samples_total
                        .saturating_add(1);
                    self.completed_dataset_fallback_first_shred_lag_us_total = self
                        .completed_dataset_fallback_first_shred_lag_us_total
                        .saturating_add(inline.first_shred_lag_us);
                    self.completed_dataset_fallback_last_shred_lag_us_total = self
                        .completed_dataset_fallback_last_shred_lag_us_total
                        .saturating_add(inline.last_shred_lag_us);
                    self.completed_dataset_fallback_completed_dataset_lag_us_total = self
                        .completed_dataset_fallback_completed_dataset_lag_us_total
                        .saturating_add(inline.completed_dataset_lag_us);
                }
            }
        }
    }
}

fn create_tx_dispatch_queue(capacity: usize) -> (TxDispatchSender, TxDispatchReceiver) {
    let queue = Arc::new(TxDispatchQueue {
        ring: ArrayQueue::new(capacity.max(1)),
        queue_depth: Arc::new(AtomicU64::new(0)),
        max_queue_depth: Arc::new(AtomicU64::new(0)),
        worker_thread: OnceLock::new(),
        closed: AtomicBool::new(false),
        sender_count: AtomicUsize::new(1),
    });
    (
        TxDispatchSender {
            queue: Arc::clone(&queue),
        },
        TxDispatchReceiver { queue },
    )
}

impl TxDispatchSender {
    fn try_send(
        &self,
        event: AcceptedTransactionDispatch,
    ) -> Result<(), StdTrySendError<AcceptedTransactionDispatch>> {
        if self.queue.closed.load(Ordering::Acquire) {
            return Err(StdTrySendError::Disconnected(event));
        }
        let queue_depth = self
            .queue
            .queue_depth
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        self.queue
            .max_queue_depth
            .fetch_max(queue_depth, Ordering::Relaxed);
        match self.queue.ring.push(event) {
            Ok(()) => {
                if let Some(thread) = self.queue.worker_thread.get() {
                    thread.unpark();
                }
                Ok(())
            }
            Err(event) => {
                self.queue.queue_depth.fetch_sub(1, Ordering::Relaxed);
                if self.queue.closed.load(Ordering::Acquire) {
                    Err(StdTrySendError::Disconnected(event))
                } else {
                    Err(StdTrySendError::Full(event))
                }
            }
        }
    }
}

impl Clone for TxDispatchSender {
    fn clone(&self) -> Self {
        self.queue.sender_count.fetch_add(1, Ordering::AcqRel);
        Self {
            queue: Arc::clone(&self.queue),
        }
    }
}

impl TxDispatchSender {
    fn queue_depth(&self) -> u64 {
        self.queue.queue_depth.load(Ordering::Relaxed)
    }

    fn max_queue_depth(&self) -> u64 {
        self.queue.max_queue_depth.load(Ordering::Relaxed)
    }
}

impl Drop for TxDispatchSender {
    fn drop(&mut self) {
        if self.queue.sender_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.queue.closed.store(true, Ordering::Release);
            if let Some(thread) = self.queue.worker_thread.get() {
                thread.unpark();
            }
        }
    }
}

impl TxDispatchReceiver {
    fn register_current_thread(&self) {
        drop(self.queue.worker_thread.set(std::thread::current()));
    }

    fn recv(&mut self) -> Option<AcceptedTransactionDispatch> {
        loop {
            if let Some(event) = self.queue.ring.pop() {
                self.queue.queue_depth.fetch_sub(1, Ordering::Relaxed);
                return Some(event);
            }
            if self.queue.closed.load(Ordering::Acquire) {
                return None;
            }
            std::thread::park();
        }
    }

    fn try_recv(&mut self) -> Result<AcceptedTransactionDispatch, StdTrySendError<()>> {
        if let Some(event) = self.queue.ring.pop() {
            self.queue.queue_depth.fetch_sub(1, Ordering::Relaxed);
            return Ok(event);
        }
        if self.queue.closed.load(Ordering::Acquire) {
            Err(StdTrySendError::Disconnected(()))
        } else {
            Err(StdTrySendError::Full(()))
        }
    }
}

/// Dispatches one queued event to all registered plugins in registration order.
async fn dispatch_event(
    targets: &PluginDispatchTargets,
    event: PluginDispatchEvent,
    dispatch_mode: PluginDispatchMode,
) {
    match event {
        PluginDispatchEvent::RawPacket(event) => {
            dispatch_hook_event(
                targets.plugins_for(PluginHookKind::RawPacket),
                PluginHookKind::RawPacket,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_raw_packet(hook_event).await;
                },
            )
            .await
        }
        PluginDispatchEvent::Shred(event) => {
            dispatch_hook_event(
                targets.plugins_for(PluginHookKind::Shred),
                PluginHookKind::Shred,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_shred(hook_event).await;
                },
            )
            .await
        }
        PluginDispatchEvent::Dataset(event) => {
            dispatch_hook_event(
                targets.plugins_for(PluginHookKind::Dataset),
                PluginHookKind::Dataset,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_dataset(hook_event).await;
                },
            )
            .await
        }
        PluginDispatchEvent::SelectedTransactionLog(event) => {
            dispatch_selected_transaction_log_event(event, dispatch_mode).await
        }
        PluginDispatchEvent::SelectedTransactionStatus(event) => {
            dispatch_selected_transaction_status_event(event, dispatch_mode).await
        }
        PluginDispatchEvent::TransactionBatch(event) => {
            dispatch_transaction_batch_event(event, dispatch_mode).await
        }
        PluginDispatchEvent::TransactionViewBatch(event) => {
            dispatch_transaction_view_batch_event(event, dispatch_mode).await
        }
        PluginDispatchEvent::AccountTouch(event) => {
            dispatch_hook_event(
                targets.plugins_for(PluginHookKind::AccountTouch),
                PluginHookKind::AccountTouch,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_account_touch(hook_event.as_ref()).await;
                },
            )
            .await
        }
        PluginDispatchEvent::SelectedAccountTouch(event) => {
            dispatch_selected_account_touch_event(event, dispatch_mode).await
        }
        PluginDispatchEvent::SelectedAccountUpdate(event) => {
            dispatch_selected_account_update_event(event, dispatch_mode).await
        }
        PluginDispatchEvent::SelectedBlockMeta(event) => {
            dispatch_selected_block_meta_event(event, dispatch_mode).await
        }
        PluginDispatchEvent::SlotStatus(event) => {
            dispatch_hook_event(
                targets.plugins_for(PluginHookKind::SlotStatus),
                PluginHookKind::SlotStatus,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_slot_status(hook_event).await;
                },
            )
            .await
        }
        PluginDispatchEvent::Reorg(event) => {
            dispatch_hook_event(
                targets.plugins_for(PluginHookKind::Reorg),
                PluginHookKind::Reorg,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_reorg(hook_event).await;
                },
            )
            .await
        }
        PluginDispatchEvent::ObservedRecentBlockhash(event) => {
            dispatch_hook_event(
                targets.plugins_for(PluginHookKind::RecentBlockhash),
                PluginHookKind::RecentBlockhash,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_recent_blockhash(hook_event).await;
                },
            )
            .await
        }
        PluginDispatchEvent::ClusterTopology(event) => {
            dispatch_hook_event(
                targets.plugins_for(PluginHookKind::ClusterTopology),
                PluginHookKind::ClusterTopology,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_cluster_topology(hook_event).await;
                },
            )
            .await
        }
        PluginDispatchEvent::LeaderSchedule(event) => {
            dispatch_hook_event(
                targets.plugins_for(PluginHookKind::LeaderSchedule),
                PluginHookKind::LeaderSchedule,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_leader_schedule(hook_event).await;
                },
            )
            .await
        }
    }
}

/// Dispatches one completed-dataset transaction-batch event.
async fn dispatch_transaction_batch_event(
    dispatch: AcceptedTransactionBatchDispatch,
    dispatch_mode: PluginDispatchMode,
) {
    let dequeued_at = Instant::now();
    match dispatch {
        AcceptedTransactionBatchDispatch::Single {
            plugin,
            event,
            completed_at,
            ..
        } => {
            let queue_wait_us = u64::try_from(
                dequeued_at
                    .saturating_duration_since(completed_at)
                    .as_micros(),
            )
            .unwrap_or(u64::MAX);
            crate::runtime_metrics::observe_transaction_batch_plugin_visibility_lag(queue_wait_us);
            let callback_started_at = Instant::now();
            if let Err(panic) = AssertUnwindSafe(plugin.on_transaction_batch(&event))
                .catch_unwind()
                .await
            {
                tracing::error!(
                    plugin = plugin.name(),
                    hook = PluginHookKind::TransactionBatch.as_str(),
                    panic = %panic_payload_to_string(panic.as_ref()),
                    "plugin hook panicked; continuing runtime"
                );
            }
            crate::runtime_metrics::observe_transaction_batch_plugin_callback_duration(
                u64::try_from(
                    Instant::now()
                        .saturating_duration_since(callback_started_at)
                        .as_micros(),
                )
                .unwrap_or(u64::MAX),
            );
        }
        AcceptedTransactionBatchDispatch::Multi {
            plugins,
            event,
            completed_at,
            prefers_inline,
        } => {
            let queue_wait_us = u64::try_from(
                dequeued_at
                    .saturating_duration_since(completed_at)
                    .as_micros(),
            )
            .unwrap_or(u64::MAX);
            crate::runtime_metrics::observe_transaction_batch_plugin_visibility_lag(queue_wait_us);
            let callback_started_at = Instant::now();
            let mode = if prefers_inline {
                PluginDispatchMode::Sequential
            } else {
                dispatch_mode
            };
            dispatch_hook_event(
                plugins.as_ref(),
                PluginHookKind::TransactionBatch,
                event,
                mode,
                |plugin, hook_event| async move {
                    plugin.on_transaction_batch(hook_event.as_ref()).await;
                },
            )
            .await;
            crate::runtime_metrics::observe_transaction_batch_plugin_callback_duration(
                u64::try_from(
                    Instant::now()
                        .saturating_duration_since(callback_started_at)
                        .as_micros(),
                )
                .unwrap_or(u64::MAX),
            );
        }
    }
}

/// Dispatches one completed-dataset transaction-view-batch event.
async fn dispatch_transaction_view_batch_event(
    dispatch: AcceptedTransactionViewBatchDispatch,
    dispatch_mode: PluginDispatchMode,
) {
    let dequeued_at = Instant::now();
    match dispatch {
        AcceptedTransactionViewBatchDispatch::Single {
            plugin,
            event,
            completed_at,
        } => {
            let queue_wait_us = u64::try_from(
                dequeued_at
                    .saturating_duration_since(completed_at)
                    .as_micros(),
            )
            .unwrap_or(u64::MAX);
            crate::runtime_metrics::observe_transaction_view_batch_plugin_visibility_lag(
                queue_wait_us,
            );
            let callback_started_at = Instant::now();
            if let Err(panic) = AssertUnwindSafe(plugin.on_transaction_view_batch(&event))
                .catch_unwind()
                .await
            {
                tracing::error!(
                    plugin = plugin.name(),
                    hook = PluginHookKind::TransactionViewBatch.as_str(),
                    panic = %panic_payload_to_string(panic.as_ref()),
                    "plugin hook panicked; continuing runtime"
                );
            }
            crate::runtime_metrics::observe_transaction_view_batch_plugin_callback_duration(
                u64::try_from(
                    Instant::now()
                        .saturating_duration_since(callback_started_at)
                        .as_micros(),
                )
                .unwrap_or(u64::MAX),
            );
        }
        AcceptedTransactionViewBatchDispatch::Multi {
            plugins,
            event,
            completed_at,
            prefers_inline,
        } => {
            let queue_wait_us = u64::try_from(
                dequeued_at
                    .saturating_duration_since(completed_at)
                    .as_micros(),
            )
            .unwrap_or(u64::MAX);
            crate::runtime_metrics::observe_transaction_view_batch_plugin_visibility_lag(
                queue_wait_us,
            );
            let callback_started_at = Instant::now();
            let mode = if prefers_inline {
                PluginDispatchMode::Sequential
            } else {
                dispatch_mode
            };
            dispatch_hook_event(
                plugins.as_ref(),
                PluginHookKind::TransactionViewBatch,
                event,
                mode,
                |plugin, hook_event| async move {
                    plugin.on_transaction_view_batch(hook_event.as_ref()).await;
                },
            )
            .await;
            crate::runtime_metrics::observe_transaction_view_batch_plugin_callback_duration(
                u64::try_from(
                    Instant::now()
                        .saturating_duration_since(callback_started_at)
                        .as_micros(),
                )
                .unwrap_or(u64::MAX),
            );
        }
    }
}

/// Dispatches one account-touch callback to only the interested plugins.
async fn dispatch_selected_account_touch_event(
    event: SelectedAccountTouchDispatch,
    dispatch_mode: PluginDispatchMode,
) {
    match event {
        SelectedAccountTouchDispatch::Single { plugin, event } => {
            plugin.on_account_touch(&event).await;
        }
        SelectedAccountTouchDispatch::Multi { plugins, event } => {
            dispatch_hook_event(
                &plugins,
                PluginHookKind::AccountTouch,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_account_touch(hook_event.as_ref()).await;
                },
            )
            .await;
        }
    }
}

async fn dispatch_selected_transaction_log_event(
    event: SelectedTransactionLogDispatch,
    dispatch_mode: PluginDispatchMode,
) {
    match event {
        SelectedTransactionLogDispatch::Single { plugin, event } => {
            plugin.on_transaction_log(&event).await;
        }
        SelectedTransactionLogDispatch::Multi { plugins, event } => {
            dispatch_hook_event(
                &plugins,
                PluginHookKind::TransactionLog,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_transaction_log(hook_event.as_ref()).await;
                },
            )
            .await;
        }
    }
}

async fn dispatch_selected_transaction_status_event(
    event: SelectedTransactionStatusDispatch,
    dispatch_mode: PluginDispatchMode,
) {
    match event {
        SelectedTransactionStatusDispatch::Single { plugin, event } => {
            plugin.on_transaction_status(&event).await;
        }
        SelectedTransactionStatusDispatch::Multi { plugins, event } => {
            dispatch_hook_event(
                &plugins,
                PluginHookKind::TransactionStatus,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_transaction_status(hook_event.as_ref()).await;
                },
            )
            .await;
        }
    }
}

async fn dispatch_selected_account_update_event(
    event: SelectedAccountUpdateDispatch,
    dispatch_mode: PluginDispatchMode,
) {
    match event {
        SelectedAccountUpdateDispatch::Single { plugin, event } => {
            plugin.on_account_update(&event).await;
        }
        SelectedAccountUpdateDispatch::Multi { plugins, event } => {
            dispatch_hook_event(
                &plugins,
                PluginHookKind::AccountUpdate,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_account_update(hook_event.as_ref()).await;
                },
            )
            .await;
        }
    }
}

async fn dispatch_selected_block_meta_event(
    event: SelectedBlockMetaDispatch,
    dispatch_mode: PluginDispatchMode,
) {
    match event {
        SelectedBlockMetaDispatch::Single { plugin, event } => {
            plugin.on_block_meta(&event).await;
        }
        SelectedBlockMetaDispatch::Multi { plugins, event } => {
            dispatch_hook_event(
                &plugins,
                PluginHookKind::BlockMeta,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_block_meta(hook_event.as_ref()).await;
                },
            )
            .await;
        }
    }
}

/// Dispatches one accepted transaction event to one or many interested plugins.
async fn dispatch_accepted_transaction_event(
    dispatch: AcceptedTransactionDispatch,
    dispatch_mode: PluginDispatchMode,
    priority: TransactionDispatchPriority,
) -> TransactionDispatchObservation {
    let dequeued_at = Instant::now();
    match dispatch {
        AcceptedTransactionDispatch::SingleOwned {
            plugin,
            event,
            completed_at,
            first_shred_observed_at,
            last_shred_observed_at,
            inline_source,
            interest,
            dataset_tx_count,
            dataset_tx_position,
        } => {
            let queue_wait_us = u64::try_from(
                dequeued_at
                    .saturating_duration_since(completed_at)
                    .as_micros(),
            )
            .unwrap_or(u64::MAX);
            let callback_started_at = Instant::now();
            let first_shred_lag_us = u64::try_from(
                callback_started_at
                    .saturating_duration_since(first_shred_observed_at)
                    .as_micros(),
            )
            .unwrap_or(u64::MAX);
            let last_shred_lag_us = u64::try_from(
                callback_started_at
                    .saturating_duration_since(last_shred_observed_at)
                    .as_micros(),
            )
            .unwrap_or(u64::MAX);
            if let Some(signature) = debug_tx_trace_signature(&event) {
                tracing::info!(
                    signature = %signature,
                    priority = priority.as_str(),
                    queue_wait_us,
                    first_shred_to_callback_us = first_shred_lag_us,
                    last_shred_to_callback_us = last_shred_lag_us,
                    ready_to_callback_us = queue_wait_us,
                    "debug tx timing trace"
                );
            }
            let inline_latency = (priority == TransactionDispatchPriority::InlineCritical)
                .then_some(InlineLatencyObservation {
                    source: inline_source,
                    first_shred_lag_us,
                    last_shred_lag_us,
                    completed_dataset_lag_us: queue_wait_us,
                });
            if let Err(panic) =
                AssertUnwindSafe(plugin.on_transaction_with_interest(&event, interest))
                    .catch_unwind()
                    .await
            {
                tracing::error!(
                    plugin = plugin.name(),
                    hook = PluginHookKind::Transaction.as_str(),
                    panic = %panic_payload_to_string(panic.as_ref()),
                    "plugin hook panicked; continuing runtime"
                );
            }
            TransactionDispatchObservation {
                visibility_lag_us: queue_wait_us,
                queue_wait_us,
                callback_duration_us: u64::try_from(
                    Instant::now()
                        .saturating_duration_since(callback_started_at)
                        .as_micros(),
                )
                .unwrap_or(u64::MAX),
                dataset_tx_count,
                dataset_tx_position,
                inline_latency,
            }
        }
        AcceptedTransactionDispatch::Multi {
            plugins,
            event,
            completed_at,
            first_shred_observed_at,
            last_shred_observed_at,
            inline_source,
            interest,
            dataset_tx_count,
            dataset_tx_position,
        } => {
            let queue_wait_us = u64::try_from(
                dequeued_at
                    .saturating_duration_since(completed_at)
                    .as_micros(),
            )
            .unwrap_or(u64::MAX);
            let callback_started_at = Instant::now();
            let first_shred_lag_us = u64::try_from(
                callback_started_at
                    .saturating_duration_since(first_shred_observed_at)
                    .as_micros(),
            )
            .unwrap_or(u64::MAX);
            let last_shred_lag_us = u64::try_from(
                callback_started_at
                    .saturating_duration_since(last_shred_observed_at)
                    .as_micros(),
            )
            .unwrap_or(u64::MAX);
            if let Some(signature) = debug_tx_trace_signature(event.as_ref()) {
                tracing::info!(
                    signature = %signature,
                    priority = priority.as_str(),
                    queue_wait_us,
                    first_shred_to_callback_us = first_shred_lag_us,
                    last_shred_to_callback_us = last_shred_lag_us,
                    ready_to_callback_us = queue_wait_us,
                    "debug tx timing trace"
                );
            }
            let inline_latency = (priority == TransactionDispatchPriority::InlineCritical)
                .then_some(InlineLatencyObservation {
                    source: inline_source,
                    first_shred_lag_us,
                    last_shred_lag_us,
                    completed_dataset_lag_us: queue_wait_us,
                });
            dispatch_hook_event(
                plugins.as_ref(),
                PluginHookKind::Transaction,
                event,
                dispatch_mode,
                move |plugin, hook_event| async move {
                    plugin
                        .on_transaction_with_interest(hook_event.as_ref(), interest)
                        .await;
                },
            )
            .await;
            TransactionDispatchObservation {
                visibility_lag_us: queue_wait_us,
                queue_wait_us,
                callback_duration_us: u64::try_from(
                    Instant::now()
                        .saturating_duration_since(callback_started_at)
                        .as_micros(),
                )
                .unwrap_or(u64::MAX),
                dataset_tx_count,
                dataset_tx_position,
                inline_latency,
            }
        }
    }
}

/// Dispatches one hook payload to all plugins using the selected dispatch strategy.
async fn dispatch_hook_event<Event, Dispatch, HookFuture>(
    plugins: &[Arc<dyn ObserverPlugin>],
    hook: PluginHookKind,
    event: Event,
    dispatch_mode: PluginDispatchMode,
    dispatch: Dispatch,
) where
    Event: Clone + Send + 'static,
    Dispatch: Fn(Arc<dyn ObserverPlugin>, Event) -> HookFuture + Copy + Send + Sync + 'static,
    HookFuture: Future<Output = ()> + Send + 'static,
{
    match plugins {
        [] => return,
        [plugin] => {
            let plugin = Arc::clone(plugin);
            let plugin_name = plugin.name();
            if let Err(panic) = AssertUnwindSafe(dispatch(plugin, event))
                .catch_unwind()
                .await
            {
                tracing::error!(
                    plugin = plugin_name,
                    hook = hook.as_str(),
                    panic = %panic_payload_to_string(panic.as_ref()),
                    "plugin hook panicked; continuing runtime"
                );
            }
            return;
        }
        _ => {}
    }
    match dispatch_mode {
        PluginDispatchMode::Sequential => {
            for plugin in plugins {
                let hook_event = event.clone();
                let plugin = Arc::clone(plugin);
                if let Err(panic) = AssertUnwindSafe(dispatch(Arc::clone(&plugin), hook_event))
                    .catch_unwind()
                    .await
                {
                    tracing::error!(
                        plugin = plugin.name(),
                        hook = hook.as_str(),
                        panic = %panic_payload_to_string(panic.as_ref()),
                        "plugin hook panicked; continuing runtime"
                    );
                }
            }
        }
        PluginDispatchMode::BoundedConcurrent(limit) => {
            if plugins.len() <= 1 || limit <= 1 {
                for plugin in plugins {
                    let hook_event = event.clone();
                    let plugin = Arc::clone(plugin);
                    if let Err(panic) = AssertUnwindSafe(dispatch(Arc::clone(&plugin), hook_event))
                        .catch_unwind()
                        .await
                    {
                        tracing::error!(
                            plugin = plugin.name(),
                            hook = hook.as_str(),
                            panic = %panic_payload_to_string(panic.as_ref()),
                            "plugin hook panicked; continuing runtime"
                        );
                    }
                }
                return;
            }
            stream::iter(plugins.iter().cloned())
                .for_each_concurrent(limit.max(1), |plugin| {
                    let hook_event = event.clone();
                    async move {
                        if let Err(panic) =
                            AssertUnwindSafe(dispatch(Arc::clone(&plugin), hook_event))
                                .catch_unwind()
                                .await
                        {
                            tracing::error!(
                                plugin = plugin.name(),
                                hook = hook.as_str(),
                                panic = %panic_payload_to_string(panic.as_ref()),
                                "plugin hook panicked; continuing runtime"
                            );
                        }
                    }
                })
                .await;
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Priority lane for accepted transaction callbacks.
pub(super) enum TransactionDispatchPriority {
    /// Inline-preferred HFT-critical transaction callbacks on the lowest-jitter lane.
    InlineCritical,
    /// Lower-priority transaction callbacks that may lag critical consumers.
    Background,
    /// Critical transaction callbacks that should retain more parallelism.
    Critical,
}

impl TransactionDispatchPriority {
    /// Returns a stable label for logs and worker names.
    const fn as_str(self) -> &'static str {
        match self {
            Self::InlineCritical => "inline_critical",
            Self::Background => "background",
            Self::Critical => "critical",
        }
    }
}
