use std::{
    any::Any,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use tokio::sync::{Semaphore, mpsc};

use crate::framework::{
    events::{
        ClusterTopologyEvent, DatasetEvent, LeaderScheduleEvent, RawPacketEvent, ShredEvent,
        TransactionEvent,
    },
    plugin::ObserverPlugin,
};

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

/// Builder for constructing an immutable [`PluginHost`].
pub struct PluginHostBuilder {
    /// Plugins accumulated in registration order.
    plugins: Vec<Arc<dyn ObserverPlugin>>,
    /// Bounded event queue capacity for async hook dispatch.
    event_queue_capacity: usize,
    /// Callback execution strategy used by the async dispatch worker.
    dispatch_mode: PluginDispatchMode,
}

impl Default for PluginHostBuilder {
    fn default() -> Self {
        Self {
            plugins: Vec::new(),
            event_queue_capacity: DEFAULT_EVENT_QUEUE_CAPACITY,
            dispatch_mode: PluginDispatchMode::default(),
        }
    }
}

impl PluginHostBuilder {
    /// Creates an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets bounded async event queue capacity for plugin hook dispatch.
    #[must_use]
    pub fn with_event_queue_capacity(mut self, capacity: usize) -> Self {
        self.event_queue_capacity = capacity.max(1);
        self
    }

    /// Sets callback execution strategy for async plugin hook dispatch.
    #[must_use]
    pub fn with_dispatch_mode(mut self, mode: PluginDispatchMode) -> Self {
        self.dispatch_mode = mode.normalized();
        self
    }

    /// Adds one plugin value by storing it behind `Arc`.
    #[must_use]
    pub fn add_plugin<P>(mut self, plugin: P) -> Self
    where
        P: ObserverPlugin,
    {
        self.plugins.push(Arc::new(plugin));
        self
    }

    /// Adds one already-shared plugin.
    #[must_use]
    pub fn add_shared_plugin(mut self, plugin: Arc<dyn ObserverPlugin>) -> Self {
        self.plugins.push(plugin);
        self
    }

    /// Adds many plugin values by storing each behind `Arc`.
    #[must_use]
    pub fn add_plugins<P, I>(mut self, plugins: I) -> Self
    where
        P: ObserverPlugin,
        I: IntoIterator<Item = P>,
    {
        self.plugins.extend(
            plugins
                .into_iter()
                .map(|plugin| Arc::new(plugin) as Arc<dyn ObserverPlugin>),
        );
        self
    }

    /// Adds many already-shared plugins.
    #[must_use]
    pub fn add_shared_plugins<I>(mut self, plugins: I) -> Self
    where
        I: IntoIterator<Item = Arc<dyn ObserverPlugin>>,
    {
        self.plugins.extend(plugins);
        self
    }

    /// Compatibility alias for [`Self::add_plugin`].
    #[must_use]
    pub fn with_plugin<P>(mut self, plugin: P) -> Self
    where
        P: ObserverPlugin,
    {
        self = self.add_plugin(plugin);
        self
    }

    /// Compatibility alias for [`Self::add_shared_plugin`].
    #[must_use]
    pub fn with_plugin_arc(mut self, plugin: Arc<dyn ObserverPlugin>) -> Self {
        self = self.add_shared_plugin(plugin);
        self
    }

    /// Compatibility alias for [`Self::add_plugins`].
    #[must_use]
    pub fn with_plugins<P, I>(mut self, plugins: I) -> Self
    where
        P: ObserverPlugin,
        I: IntoIterator<Item = P>,
    {
        self = self.add_plugins(plugins);
        self
    }

    /// Compatibility alias for [`Self::add_shared_plugins`].
    #[must_use]
    pub fn with_plugin_arcs<I>(mut self, plugins: I) -> Self
    where
        I: IntoIterator<Item = Arc<dyn ObserverPlugin>>,
    {
        self = self.add_shared_plugins(plugins);
        self
    }

    /// Finalizes the builder into an immutable host.
    #[must_use]
    pub fn build(self) -> PluginHost {
        let plugins: Arc<[Arc<dyn ObserverPlugin>]> = Arc::from(self.plugins);
        let dispatcher = PluginDispatcher::new(
            plugins.clone(),
            self.event_queue_capacity,
            self.dispatch_mode,
        );
        PluginHost {
            plugins,
            dispatcher,
        }
    }
}

#[derive(Clone)]
/// Shared sender/counters for asynchronous plugin event delivery.
struct PluginDispatcher {
    /// Bounded queue sender for plugin dispatch events.
    tx: mpsc::Sender<PluginDispatchEvent>,
    /// Counter of hook events dropped due to queue pressure or closure.
    dropped_events: Arc<AtomicU64>,
}

impl PluginDispatcher {
    /// Creates a dispatcher and background worker when at least one plugin is registered.
    fn new(
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
    fn dispatch(&self, hook: &'static str, event: PluginDispatchEvent) {
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
    fn dropped_count(&self) -> u64 {
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
enum PluginDispatchEvent {
    /// Packet ingress callback payload.
    RawPacket(RawPacketEvent),
    /// Parsed shred callback payload.
    Shred(ShredEvent),
    /// Reassembled dataset callback payload.
    Dataset(DatasetEvent),
    /// Decoded transaction callback payload.
    Transaction(TransactionEvent),
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

/// Immutable plugin registry and async event dispatcher.
#[derive(Clone)]
pub struct PluginHost {
    /// Immutable plugin collection in registration order.
    plugins: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Optional async dispatcher state (absent when no plugins are registered).
    dispatcher: Option<PluginDispatcher>,
}

impl Default for PluginHost {
    fn default() -> Self {
        Self {
            plugins: Arc::from(Vec::<Arc<dyn ObserverPlugin>>::new()),
            dispatcher: None,
        }
    }
}

impl PluginHost {
    /// Starts a new host builder.
    #[must_use]
    pub fn builder() -> PluginHostBuilder {
        PluginHostBuilder::new()
    }

    /// Returns true when no plugins are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Returns number of registered plugins.
    #[must_use]
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Returns total dropped hook events due to queue backpressure/closure.
    #[must_use]
    pub fn dropped_event_count(&self) -> u64 {
        self.dispatcher
            .as_ref()
            .map_or(0, PluginDispatcher::dropped_count)
    }

    /// Returns plugin identifiers in registration order.
    #[must_use]
    pub fn plugin_names(&self) -> Vec<&'static str> {
        self.plugins.iter().map(|plugin| plugin.name()).collect()
    }

    /// Enqueues raw packet hook to registered plugins.
    pub fn on_raw_packet(&self, event: RawPacketEvent) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.dispatch("on_raw_packet", PluginDispatchEvent::RawPacket(event));
        }
    }

    /// Enqueues parsed shred hook to registered plugins.
    pub fn on_shred(&self, event: ShredEvent) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.dispatch("on_shred", PluginDispatchEvent::Shred(event));
        }
    }

    /// Enqueues reconstructed dataset hook to registered plugins.
    pub fn on_dataset(&self, event: DatasetEvent) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.dispatch("on_dataset", PluginDispatchEvent::Dataset(event));
        }
    }

    /// Enqueues reconstructed transaction hook to registered plugins.
    pub fn on_transaction(&self, event: TransactionEvent) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.dispatch("on_transaction", PluginDispatchEvent::Transaction(event));
        }
    }

    /// Enqueues cluster topology diff/snapshot hook to registered plugins.
    pub fn on_cluster_topology(&self, event: ClusterTopologyEvent) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.dispatch(
                "on_cluster_topology",
                PluginDispatchEvent::ClusterTopology(event),
            );
        }
    }

    /// Enqueues leader-schedule diff/snapshot hook to registered plugins.
    pub fn on_leader_schedule(&self, event: LeaderScheduleEvent) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.dispatch(
                "on_leader_schedule",
                PluginDispatchEvent::LeaderSchedule(event),
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::{
        sync::atomic::AtomicUsize,
        time::{Duration, Instant},
    };

    #[derive(Clone, Copy)]
    struct PluginA;

    #[async_trait]
    impl ObserverPlugin for PluginA {
        fn name(&self) -> &'static str {
            "plugin-a"
        }
    }

    #[derive(Clone, Copy)]
    struct PluginB;

    #[async_trait]
    impl ObserverPlugin for PluginB {
        fn name(&self) -> &'static str {
            "plugin-b"
        }
    }

    #[test]
    fn builder_registers_multiple_plugins() {
        let host = PluginHostBuilder::new()
            .with_plugin(PluginA)
            .with_plugin(PluginB)
            .build();
        assert_eq!(host.plugin_names(), vec!["plugin-a", "plugin-b"]);
    }

    #[test]
    fn builder_registers_multiple_plugins_from_iterator() {
        let host = PluginHostBuilder::new()
            .with_plugins([PluginA, PluginA])
            .with_plugin(PluginB)
            .build();
        assert_eq!(host.len(), 3);
    }

    #[derive(Clone, Copy)]
    struct PanicPlugin;

    #[async_trait]
    impl ObserverPlugin for PanicPlugin {
        fn name(&self) -> &'static str {
            "panic-plugin"
        }

        async fn on_dataset(&self, _event: DatasetEvent) {
            panic!("panic-plugin failed");
        }
    }

    #[derive(Clone)]
    struct CounterPlugin {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ObserverPlugin for CounterPlugin {
        fn name(&self) -> &'static str {
            "counter-plugin"
        }

        async fn on_dataset(&self, _event: DatasetEvent) {
            self.counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn dispatch_continues_after_plugin_panic() {
        let counter = Arc::new(AtomicUsize::new(0));
        let host = PluginHostBuilder::new()
            .add_plugin(PanicPlugin)
            .add_plugin(CounterPlugin {
                counter: Arc::clone(&counter),
            })
            .build();
        host.on_dataset(DatasetEvent {
            slot: 1,
            start_index: 0,
            end_index: 0,
            last_in_slot: false,
            shreds: 1,
            payload_len: 1,
            tx_count: 0,
        });
        assert!(wait_until_counter(
            counter.as_ref(),
            1,
            Duration::from_secs(2)
        ));
    }

    #[test]
    fn concurrent_multi_plugin_dispatch_is_consistent() {
        let counter_a = Arc::new(AtomicUsize::new(0));
        let counter_b = Arc::new(AtomicUsize::new(0));
        let threads = 8_usize;
        let iterations = 2_000_usize;
        let expected = threads.saturating_mul(iterations);
        let host = Arc::new(
            PluginHostBuilder::new()
                .with_event_queue_capacity(expected.saturating_mul(2))
                .add_plugin(CounterPlugin {
                    counter: counter_a.clone(),
                })
                .add_plugin(CounterPlugin {
                    counter: counter_b.clone(),
                })
                .build(),
        );
        let mut joins = Vec::with_capacity(threads);
        for _ in 0..threads {
            let worker_host = host.clone();
            joins.push(thread::spawn(move || {
                for _ in 0..iterations {
                    worker_host.on_dataset(DatasetEvent {
                        slot: 9,
                        start_index: 0,
                        end_index: 0,
                        last_in_slot: false,
                        shreds: 1,
                        payload_len: 1,
                        tx_count: 1,
                    });
                }
            }));
        }
        for join in joins {
            assert!(join.join().is_ok());
        }
        assert!(wait_until_counter(
            counter_a.as_ref(),
            expected,
            Duration::from_secs(5)
        ));
        assert!(wait_until_counter(
            counter_b.as_ref(),
            expected,
            Duration::from_secs(5)
        ));
        assert_eq!(host.dropped_event_count(), 0);
    }

    #[test]
    fn bounded_concurrent_dispatch_mode_is_consistent() {
        let counter_a = Arc::new(AtomicUsize::new(0));
        let counter_b = Arc::new(AtomicUsize::new(0));
        let threads = 8_usize;
        let iterations = 2_000_usize;
        let expected = threads.saturating_mul(iterations);
        let host = Arc::new(
            PluginHostBuilder::new()
                .with_event_queue_capacity(expected.saturating_mul(2))
                .with_dispatch_mode(PluginDispatchMode::BoundedConcurrent(8))
                .add_plugin(CounterPlugin {
                    counter: counter_a.clone(),
                })
                .add_plugin(CounterPlugin {
                    counter: counter_b.clone(),
                })
                .build(),
        );
        let mut joins = Vec::with_capacity(threads);
        for _ in 0..threads {
            let worker_host = host.clone();
            joins.push(thread::spawn(move || {
                for _ in 0..iterations {
                    worker_host.on_dataset(DatasetEvent {
                        slot: 9,
                        start_index: 0,
                        end_index: 0,
                        last_in_slot: false,
                        shreds: 1,
                        payload_len: 1,
                        tx_count: 1,
                    });
                }
            }));
        }
        for join in joins {
            assert!(join.join().is_ok());
        }
        assert!(wait_until_counter(
            counter_a.as_ref(),
            expected,
            Duration::from_secs(5)
        ));
        assert!(wait_until_counter(
            counter_b.as_ref(),
            expected,
            Duration::from_secs(5)
        ));
        assert_eq!(host.dropped_event_count(), 0);
    }

    fn wait_until_counter(counter: &AtomicUsize, expected: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if counter.load(Ordering::Relaxed) == expected {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }
}
