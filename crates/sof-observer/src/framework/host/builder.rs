use super::dispatch::{PluginDispatcher, TransactionPluginDispatcher};
use super::*;

/// Builder for constructing an immutable [`PluginHost`].
pub struct PluginHostBuilder {
    /// Plugins accumulated in registration order.
    plugins: Vec<Arc<dyn ObserverPlugin>>,
    /// Bounded event queue capacity for async hook dispatch.
    event_queue_capacity: usize,
    /// Callback execution strategy used by the async dispatch worker.
    dispatch_mode: PluginDispatchMode,
    /// Number of accepted-transaction dispatch workers.
    transaction_dispatch_workers: usize,
}

impl Default for PluginHostBuilder {
    fn default() -> Self {
        Self {
            plugins: Vec::new(),
            event_queue_capacity: DEFAULT_EVENT_QUEUE_CAPACITY,
            dispatch_mode: PluginDispatchMode::default(),
            transaction_dispatch_workers: default_transaction_dispatch_workers(),
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

    /// Sets accepted-transaction dispatch worker count.
    #[must_use]
    pub fn with_transaction_dispatch_workers(mut self, workers: usize) -> Self {
        self.transaction_dispatch_workers = workers.max(1);
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
        let subscriptions = PluginHookSubscriptions {
            raw_packet: plugins.iter().any(|plugin| plugin.wants_raw_packet()),
            shred: plugins.iter().any(|plugin| plugin.wants_shred()),
            dataset: plugins.iter().any(|plugin| plugin.wants_dataset()),
            transaction: plugins.iter().any(|plugin| plugin.wants_transaction()),
            account_touch: plugins.iter().any(|plugin| plugin.wants_account_touch()),
            slot_status: plugins.iter().any(|plugin| plugin.wants_slot_status()),
            reorg: plugins.iter().any(|plugin| plugin.wants_reorg()),
            recent_blockhash: plugins.iter().any(|plugin| plugin.wants_recent_blockhash()),
            cluster_topology: plugins.iter().any(|plugin| plugin.wants_cluster_topology()),
            leader_schedule: plugins.iter().any(|plugin| plugin.wants_leader_schedule()),
        };
        let dispatcher = PluginDispatcher::new(
            plugins.clone(),
            self.event_queue_capacity,
            self.dispatch_mode,
        );
        let transaction_dispatcher = subscriptions.transaction.then(|| {
            TransactionPluginDispatcher::new(
                self.event_queue_capacity,
                self.transaction_dispatch_workers,
                self.dispatch_mode,
            )
        });
        PluginHost {
            plugins,
            dispatcher,
            transaction_dispatcher,
            subscriptions,
            latest_observed_recent_blockhash: ArcShift::new(None),
            latest_observed_tpu_leader: ArcShift::new(None),
        }
    }
}
