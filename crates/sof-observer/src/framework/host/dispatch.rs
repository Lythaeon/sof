use super::*;

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
    pub(super) fn dispatch(&self, hook: &'static str, event: PluginDispatchEvent) {
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.record_drop(hook, "queue full");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.record_drop(hook, "queue closed");
            }
        }
    }

    /// Returns total number of dropped hook events.
    pub(super) fn dropped_count(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    /// Increments dropped counters and emits sampled warning logs.
    fn record_drop(&self, hook: &'static str, reason: &'static str) {
        let dropped = self
            .dropped_events
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if dropped <= INITIAL_DROP_LOG_LIMIT || dropped.is_multiple_of(DROP_LOG_SAMPLE_EVERY) {
            tracing::warn!(
                hook,
                reason,
                dropped,
                "dropping plugin hook event to protect ingest hot path"
            );
        }
    }
}

/// Internal event envelope sent to the plugin worker queue.
pub(super) enum PluginDispatchEvent {
    /// Packet ingress callback payload.
    RawPacket(RawPacketEvent),
    /// Parsed shred callback payload.
    Shred(ShredEvent),
    /// Reassembled dataset callback payload.
    Dataset(DatasetEvent),
    /// Decoded transaction callback payload.
    Transaction(TransactionEvent),
    /// Observed recent blockhash callback payload.
    ObservedRecentBlockhash(ObservedRecentBlockhashEvent),
    /// Cluster topology callback payload.
    ClusterTopology(ClusterTopologyEvent),
    /// Leader schedule callback payload.
    LeaderSchedule(LeaderScheduleEvent),
}

/// Spawns a dedicated worker thread that drains hook events and executes plugin callbacks.
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
                "on_raw_packet",
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
                "on_shred",
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
                "on_dataset",
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_dataset(hook_event).await;
                },
            )
            .await
        }
        PluginDispatchEvent::Transaction(event) => {
            dispatch_hook_event(
                plugins,
                "on_transaction",
                event,
                dispatch_mode,
                |plugin, hook_event| async move {
                    plugin.on_transaction(hook_event).await;
                },
            )
            .await
        }
        PluginDispatchEvent::ObservedRecentBlockhash(event) => {
            dispatch_hook_event(
                plugins,
                "on_recent_blockhash",
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
                "on_cluster_topology",
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
                "on_leader_schedule",
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

/// Dispatches one hook payload to all plugins using the selected dispatch strategy.
async fn dispatch_hook_event<Event, Dispatch, HookFuture>(
    plugins: &[Arc<dyn ObserverPlugin>],
    hook: &'static str,
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
                let result = tokio::spawn(dispatch(plugin, hook_event)).await;
                if let Err(error) = result {
                    log_plugin_task_error(error, plugin_name, hook);
                }
            }
        }
        PluginDispatchMode::BoundedConcurrent(limit) => {
            let semaphore = Arc::new(Semaphore::new(limit.max(1)));
            let mut handles = Vec::with_capacity(plugins.len());
            for plugin in plugins {
                let Ok(permit) = semaphore.clone().acquire_owned().await else {
                    tracing::error!(
                        hook,
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
fn log_plugin_task_error(error: tokio::task::JoinError, plugin: &'static str, hook: &'static str) {
    if error.is_cancelled() {
        tracing::error!(plugin, hook, "plugin hook task cancelled");
        return;
    }
    if error.is_panic() {
        let panic = error.into_panic();
        tracing::error!(
            plugin,
            hook,
            panic = %panic_payload_to_string(panic.as_ref()),
            "plugin hook panicked; continuing runtime"
        );
    }
}
