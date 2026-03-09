use super::*;
use crate::framework::AccountTouchEvent;
use crate::framework::TransactionInterest;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::num::NonZeroUsize;

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
    /// Account-touch callbacks.
    AccountTouch,
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
            Self::AccountTouch => "on_account_touch",
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

#[derive(Clone)]
/// Shared sender/counters for asynchronous plugin event delivery.
pub(super) struct PluginDispatcher {
    /// Bounded queue sender for plugin dispatch events.
    tx: mpsc::Sender<PluginDispatchEvent>,
    /// Counter of hook events dropped due to queue pressure or closure.
    dropped_events: Arc<AtomicU64>,
}

impl PluginDispatcher {
    /// Creates a dispatcher and background worker when at least one plugin is registered.
    pub(super) fn new(
        plugins: Arc<[Arc<dyn ObserverPlugin>]>,
        queue_capacity: usize,
        dispatch_mode: PluginDispatchMode,
    ) -> Option<Self> {
        if plugins.is_empty() {
            return None;
        }
        let (tx, rx) = mpsc::channel(queue_capacity.max(1));
        let dropped_events = Arc::new(AtomicU64::new(0));
        spawn_dispatch_worker(plugins, rx, dispatch_mode.normalized());
        Some(Self { tx, dropped_events })
    }

    /// Attempts non-blocking enqueue of one hook event.
    pub(super) fn dispatch(&self, event: PluginDispatchEvent) {
        let hook = event.hook_kind();
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.record_drop(hook, PluginDispatchFailureReason::QueueFull);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.record_drop(hook, PluginDispatchFailureReason::QueueClosed);
            }
        }
    }

    /// Returns total number of dropped hook events.
    pub(super) fn dropped_count(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
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

/// One accepted-transaction callback fan-out unit.
pub(super) enum AcceptedTransactionDispatch {
    /// One interested plugin receives the event directly.
    Single {
        /// Interested plugin.
        plugin: Arc<dyn ObserverPlugin>,
        /// Accepted transaction payload.
        event: Arc<TransactionEvent>,
        /// Delivery priority assigned by the host.
        interest: TransactionInterest,
    },
    /// Multiple interested plugins share the same event payload.
    Multi {
        /// Interested plugins.
        plugins: Arc<[Arc<dyn ObserverPlugin>]>,
        /// Accepted transaction payload.
        event: Arc<TransactionEvent>,
        /// Delivery priority assigned by the host.
        interest: TransactionInterest,
    },
}

/// Preclassified accepted-transaction routing targets.
pub(crate) struct ClassifiedTransactionDispatch {
    /// Interested critical-lane plugins.
    critical_plugins: Vec<Arc<dyn ObserverPlugin>>,
    /// Interested background-lane plugins.
    background_plugins: Vec<Arc<dyn ObserverPlugin>>,
}

impl ClassifiedTransactionDispatch {
    /// Builds an empty routing table.
    pub(crate) const fn empty() -> Self {
        Self {
            critical_plugins: Vec::new(),
            background_plugins: Vec::new(),
        }
    }

    /// Returns true when no plugin is interested in this transaction.
    pub(crate) fn is_empty(&self) -> bool {
        self.critical_plugins.is_empty() && self.background_plugins.is_empty()
    }

    /// Records one plugin under the requested priority.
    pub(crate) fn push(&mut self, interest: TransactionInterest, plugin: Arc<dyn ObserverPlugin>) {
        match interest {
            TransactionInterest::Ignore => {}
            TransactionInterest::Background => self.background_plugins.push(plugin),
            TransactionInterest::Critical => self.critical_plugins.push(plugin),
        }
    }

    /// Converts the routing table into queued dispatch envelopes.
    pub(super) fn into_dispatches(
        self,
        event: Arc<TransactionEvent>,
    ) -> (
        Option<AcceptedTransactionDispatch>,
        Option<AcceptedTransactionDispatch>,
    ) {
        let critical = AcceptedTransactionDispatch::from_plugins(
            self.critical_plugins,
            Arc::clone(&event),
            TransactionInterest::Critical,
        );
        let background = AcceptedTransactionDispatch::from_plugins(
            self.background_plugins,
            event,
            TransactionInterest::Background,
        );
        (critical, background)
    }
}

impl AcceptedTransactionDispatch {
    /// Builds a dispatch envelope only when at least one plugin is interested.
    pub(super) fn from_plugins(
        plugins: Vec<Arc<dyn ObserverPlugin>>,
        event: Arc<TransactionEvent>,
        interest: TransactionInterest,
    ) -> Option<Self> {
        match plugins.len() {
            0 => None,
            1 => Some(Self::Single {
                plugin: plugins.into_iter().next()?,
                event,
                interest,
            }),
            _ => Some(Self::Multi {
                plugins: Arc::from(plugins),
                event,
                interest,
            }),
        }
    }

    /// Derives a stable shard key so related transactions stay ordered per worker.
    fn shard_key(&self) -> u64 {
        let event = match self {
            Self::Single { event, .. } | Self::Multi { event, .. } => event,
        };
        let mut hasher = DefaultHasher::new();
        event.slot.hash(&mut hasher);
        if let Some(signature) = event
            .signature
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
    /// Bounded per-worker queues for critical transaction hooks.
    critical_txs: Arc<[mpsc::Sender<AcceptedTransactionDispatch>]>,
    /// Bounded per-worker queues for background transaction hooks.
    background_txs: Arc<[mpsc::Sender<AcceptedTransactionDispatch>]>,
    /// Total critical transaction hook drops.
    critical_dropped_events: Arc<AtomicU64>,
    /// Total background transaction hook drops.
    background_dropped_events: Arc<AtomicU64>,
}

impl TransactionPluginDispatcher {
    /// Creates per-priority worker queues for accepted transaction callbacks.
    pub(super) fn new(
        queue_capacity: usize,
        worker_count: usize,
        dispatch_mode: PluginDispatchMode,
    ) -> Self {
        let worker_count = worker_count.max(1);
        let background_worker_count = worker_count.clamp(1, 4);
        let critical_queue_capacity = queue_capacity.div_ceil(worker_count).max(1);
        let background_queue_capacity = queue_capacity.div_ceil(background_worker_count).max(1);
        let critical_dropped_events = Arc::new(AtomicU64::new(0));
        let background_dropped_events = Arc::new(AtomicU64::new(0));
        let mut critical_txs = Vec::with_capacity(worker_count);
        for worker_index in 0..worker_count {
            let (tx, rx) = mpsc::channel(critical_queue_capacity);
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
            let (tx, rx) = mpsc::channel(background_queue_capacity);
            spawn_transaction_dispatch_worker(
                worker_index,
                TransactionDispatchPriority::Background,
                rx,
                dispatch_mode.normalized(),
            );
            background_txs.push(tx);
        }
        Self {
            critical_txs: Arc::from(critical_txs),
            background_txs: Arc::from(background_txs),
            critical_dropped_events,
            background_dropped_events,
        }
    }

    /// Routes one accepted transaction to its priority queue shard.
    pub(super) fn dispatch(
        &self,
        priority: TransactionDispatchPriority,
        event: AcceptedTransactionDispatch,
    ) {
        let txs = match priority {
            TransactionDispatchPriority::Background => &self.background_txs,
            TransactionDispatchPriority::Critical => &self.critical_txs,
        };
        let Some(shard_count) = NonZeroUsize::new(txs.len()) else {
            self.record_drop(priority, PluginDispatchFailureReason::QueueClosed);
            return;
        };
        let shard = transaction_dispatch_shard(event.shard_key(), shard_count);
        let Some(tx) = txs.get(shard) else {
            self.record_drop(priority, PluginDispatchFailureReason::QueueClosed);
            return;
        };
        match tx.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.record_drop(priority, PluginDispatchFailureReason::QueueFull);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
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

    /// Records one dropped transaction callback.
    fn record_drop(
        &self,
        priority: TransactionDispatchPriority,
        reason: PluginDispatchFailureReason,
    ) {
        let dropped = match priority {
            TransactionDispatchPriority::Background => self
                .background_dropped_events
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1),
            TransactionDispatchPriority::Critical => self
                .critical_dropped_events
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
    /// Static account-touch callback payload.
    AccountTouch(Arc<AccountTouchEvent>),
    /// Static account-touch callback payload targeted to a subset of plugins.
    SelectedAccountTouch(SelectedAccountTouchDispatch),
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
            Self::AccountTouch(_) => PluginHookKind::AccountTouch,
            Self::SelectedAccountTouch(_) => PluginHookKind::AccountTouch,
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
        /// Shared event payload for the selected plugin.
        event: Arc<AccountTouchEvent>,
    },
    /// Multiple interested plugins share the same event payload.
    Multi {
        /// Selected plugin callback targets.
        plugins: Arc<[Arc<dyn ObserverPlugin>]>,
        /// Shared event payload for the selected plugin batch.
        event: Arc<AccountTouchEvent>,
    },
}

impl SelectedAccountTouchDispatch {
    /// Builds a selected account-touch dispatch only when at least one plugin is interested.
    pub(super) fn from_plugins(
        plugins: Vec<Arc<dyn ObserverPlugin>>,
        event: AccountTouchEvent,
    ) -> Option<Self> {
        let event = Arc::new(event);
        match plugins.len() {
            0 => None,
            1 => Some(Self::Single {
                plugin: plugins.into_iter().next()?,
                event,
            }),
            _ => Some(Self::Multi {
                plugins: Arc::from(plugins),
                event,
            }),
        }
    }
}

/// Spawns a dedicated worker thread that drains non-transaction hook events.
fn spawn_dispatch_worker(
    plugins: Arc<[Arc<dyn ObserverPlugin>]>,
    mut rx: mpsc::Receiver<PluginDispatchEvent>,
    dispatch_mode: PluginDispatchMode,
) {
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
            runtime.block_on(async move {
                while let Some(event) = rx.recv().await {
                    dispatch_event(&plugins, event, dispatch_mode).await;
                }
            });
        });
    if let Err(error) = spawn_result {
        tracing::error!(error = %error, "failed to spawn plugin dispatch worker");
    }
}

/// Spawns one dedicated worker thread for one transaction priority shard.
fn spawn_transaction_dispatch_worker(
    worker_index: usize,
    priority: TransactionDispatchPriority,
    mut rx: mpsc::Receiver<AcceptedTransactionDispatch>,
    dispatch_mode: PluginDispatchMode,
) {
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
        runtime.block_on(async move {
            while let Some(event) = rx.recv().await {
                dispatch_accepted_transaction_event(event, dispatch_mode).await;
            }
        });
    });
    if let Err(error) = spawn_result {
        tracing::error!(
            error = %error,
            priority = priority.as_str(),
            "failed to spawn transaction plugin dispatch worker"
        );
    }
}

/// Dispatches one queued event to all registered plugins in registration order.
async fn dispatch_event(
    plugins: &[Arc<dyn ObserverPlugin>],
    event: PluginDispatchEvent,
    dispatch_mode: PluginDispatchMode,
) {
    match event {
        PluginDispatchEvent::RawPacket(event) => {
            dispatch_hook_event(
                plugins,
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
                plugins,
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
                plugins,
                PluginHookKind::Dataset,
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_dataset(hook_event).await;
                },
            )
            .await
        }
        PluginDispatchEvent::AccountTouch(event) => {
            dispatch_hook_event(
                plugins,
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
        PluginDispatchEvent::SlotStatus(event) => {
            dispatch_hook_event(
                plugins,
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
                plugins,
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
                plugins,
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
                plugins,
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
                plugins,
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

/// Dispatches one account-touch callback to only the interested plugins.
async fn dispatch_selected_account_touch_event(
    event: SelectedAccountTouchDispatch,
    dispatch_mode: PluginDispatchMode,
) {
    match event {
        SelectedAccountTouchDispatch::Single { plugin, event } => {
            plugin.on_account_touch(event.as_ref()).await;
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

/// Dispatches one accepted transaction event to one or many interested plugins.
async fn dispatch_accepted_transaction_event(
    dispatch: AcceptedTransactionDispatch,
    dispatch_mode: PluginDispatchMode,
) {
    match dispatch {
        AcceptedTransactionDispatch::Single {
            plugin,
            event,
            interest,
        } => {
            let plugin_name = plugin.name();
            let handle = tokio::spawn(async move {
                plugin
                    .on_transaction_with_interest(event.as_ref(), interest)
                    .await;
            });
            if let Err(error) = handle.await {
                log_plugin_task_error(error, plugin_name, PluginHookKind::Transaction);
            }
        }
        AcceptedTransactionDispatch::Multi {
            plugins,
            event,
            interest,
        } => {
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
    match dispatch_mode {
        PluginDispatchMode::Sequential => {
            for plugin in plugins {
                let plugin = Arc::clone(plugin);
                let plugin_name = plugin.name();
                let hook_event = event.clone();
                let handle = tokio::spawn(async move {
                    dispatch(plugin, hook_event).await;
                });
                if let Err(error) = handle.await {
                    log_plugin_task_error(error, plugin_name, hook);
                }
            }
        }
        PluginDispatchMode::BoundedConcurrent(limit) => {
            if plugins.len() <= 1 || limit <= 1 {
                for plugin in plugins {
                    let plugin = Arc::clone(plugin);
                    let plugin_name = plugin.name();
                    let hook_event = event.clone();
                    let handle = tokio::spawn(async move {
                        dispatch(plugin, hook_event).await;
                    });
                    if let Err(error) = handle.await {
                        log_plugin_task_error(error, plugin_name, hook);
                    }
                }
                return;
            }
            let semaphore = Arc::new(Semaphore::new(limit.max(1)));
            let mut handles = Vec::with_capacity(plugins.len());
            for plugin in plugins {
                let Ok(permit) = semaphore.clone().acquire_owned().await else {
                    tracing::error!(
                        hook = hook.as_str(),
                        "plugin dispatch semaphore closed unexpectedly; skipping remaining callbacks"
                    );
                    break;
                };
                let plugin = Arc::clone(plugin);
                let plugin_name = plugin.name();
                let hook_event = event.clone();
                let handle = tokio::spawn(async move {
                    let _permit = permit;
                    dispatch(plugin, hook_event).await;
                });
                handles.push((plugin_name, handle));
            }
            for (plugin_name, handle) in handles {
                if let Err(error) = handle.await {
                    log_plugin_task_error(error, plugin_name, hook);
                }
            }
        }
    }
}

/// Logs plugin task failures while preserving runtime progress.
fn log_plugin_task_error(
    error: tokio::task::JoinError,
    plugin: &'static str,
    hook: PluginHookKind,
) {
    if error.is_cancelled() {
        tracing::error!(plugin, hook = hook.as_str(), "plugin hook task cancelled");
        return;
    }
    if error.is_panic() {
        let panic = error.into_panic();
        tracing::error!(
            plugin,
            hook = hook.as_str(),
            panic = %panic_payload_to_string(panic.as_ref()),
            "plugin hook panicked; continuing runtime"
        );
    }
}
#[derive(Clone, Copy, Debug)]
/// Priority lane for accepted transaction callbacks.
pub(super) enum TransactionDispatchPriority {
    /// Lower-priority transaction callbacks that may lag critical consumers.
    Background,
    /// Critical transaction callbacks that should retain more parallelism.
    Critical,
}

impl TransactionDispatchPriority {
    /// Returns a stable label for logs and worker names.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Critical => "critical",
        }
    }
}
