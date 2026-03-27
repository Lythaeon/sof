use super::dispatch::{PluginDispatchTargets, PluginDispatcher, TransactionPluginDispatcher};
use super::*;

/// Collects plugins that subscribed to one hook family while preserving registration order.
fn collect_hook_plugins(
    plugins: &Arc<[Arc<dyn ObserverPlugin>]>,
    plugin_subscriptions: &[PluginHookSubscriptions],
    wants_hook: impl Fn(&PluginHookSubscriptions) -> bool,
) -> Arc<[Arc<dyn ObserverPlugin>]> {
    Arc::from(
        plugins
            .iter()
            .zip(plugin_subscriptions.iter())
            .filter(|(_, subscription)| wants_hook(subscription))
            .map(|(plugin, _)| Arc::clone(plugin))
            .collect::<Vec<_>>(),
    )
}

/// Builder for constructing an immutable [`PluginHost`].
///
/// # Examples
///
/// ```rust
/// use async_trait::async_trait;
/// use sof::framework::{ObserverPlugin, PluginConfig, PluginHost};
///
/// struct TransactionsOnly;
///
/// #[async_trait]
/// impl ObserverPlugin for TransactionsOnly {
///     fn config(&self) -> PluginConfig {
///         PluginConfig::new().with_transaction()
///     }
/// }
///
/// let host = PluginHost::builder()
///     .with_event_queue_capacity(4096)
///     .add_plugin(TransactionsOnly)
///     .build();
///
/// assert_eq!(host.len(), 1);
/// assert!(host.wants_transaction());
/// ```
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
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sof::framework::PluginHostBuilder;
    ///
    /// let _builder = PluginHostBuilder::new();
    /// ```
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
    ///
    /// # Examples
    ///
    /// ```rust
    /// use async_trait::async_trait;
    /// use sof::framework::{ObserverPlugin, PluginConfig, PluginHost};
    ///
    /// struct DatasetPlugin;
    ///
    /// #[async_trait]
    /// impl ObserverPlugin for DatasetPlugin {
    ///     fn config(&self) -> PluginConfig {
    ///         PluginConfig::new().with_dataset()
    ///     }
    /// }
    ///
    /// let host = PluginHost::builder().add_plugin(DatasetPlugin).build();
    /// assert!(host.wants_dataset());
    /// ```
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
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sof::framework::PluginHostBuilder;
    ///
    /// let host = PluginHostBuilder::new().build();
    /// assert!(host.is_empty());
    /// ```
    #[must_use]
    pub fn build(self) -> PluginHost {
        let plugins: Arc<[Arc<dyn ObserverPlugin>]> = Arc::from(self.plugins);
        let plugin_subscriptions: Vec<PluginHookSubscriptions> = plugins
            .iter()
            .map(|plugin| PluginHookSubscriptions::from(&plugin.config()))
            .collect();
        let raw_packet_plugins =
            collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
                subscription.raw_packet
            });
        let shred_plugins = collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
            subscription.shred
        });
        let dataset_plugins =
            collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
                subscription.dataset
            });
        let transaction_plugins =
            collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
                subscription.transaction
            });
        let transaction_log_plugins =
            collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
                subscription.transaction_log
            });
        let transaction_status_plugins =
            collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
                subscription.transaction_status
            });
        let transaction_batch_plugins =
            collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
                subscription.transaction_batch
            });
        let transaction_view_batch_plugins =
            collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
                subscription.transaction_view_batch
            });
        let transaction_plugin_inline_preferences: Arc<[bool]> = Arc::from(
            plugin_subscriptions
                .iter()
                .filter(|subscription| subscription.transaction)
                .map(|subscription| subscription.inline_transaction)
                .collect::<Vec<_>>(),
        );
        let transaction_plugin_commitments: Arc<
            [crate::framework::plugin::TransactionCommitmentSelector],
        > = Arc::from(
            plugins
                .iter()
                .zip(plugin_subscriptions.iter())
                .filter(|(_plugin, subscription)| subscription.transaction)
                .map(|(plugin, _subscription)| plugin.config().transaction_commitment)
                .collect::<Vec<_>>(),
        );
        let transaction_plugin_prefilters: Arc<[Option<crate::framework::TransactionPrefilter>]> =
            Arc::from(
                plugins
                    .iter()
                    .zip(plugin_subscriptions.iter())
                    .filter(|(_plugin, subscription)| subscription.transaction)
                    .map(|(plugin, _subscription)| plugin.transaction_prefilter().cloned())
                    .collect::<Vec<_>>(),
            );
        let transaction_batch_plugin_inline_preferences: Arc<[bool]> = Arc::from(
            plugin_subscriptions
                .iter()
                .filter(|subscription| subscription.transaction_batch)
                .map(|subscription| subscription.inline_transaction_batch)
                .collect::<Vec<_>>(),
        );
        let transaction_log_plugin_commitments: Arc<
            [crate::framework::plugin::TransactionCommitmentSelector],
        > = Arc::from(
            plugins
                .iter()
                .zip(plugin_subscriptions.iter())
                .filter(|(_plugin, subscription)| subscription.transaction_log)
                .map(|(plugin, _subscription)| plugin.config().transaction_commitment)
                .collect::<Vec<_>>(),
        );
        let transaction_status_plugin_commitments: Arc<
            [crate::framework::plugin::TransactionCommitmentSelector],
        > = Arc::from(
            plugins
                .iter()
                .zip(plugin_subscriptions.iter())
                .filter(|(_plugin, subscription)| subscription.transaction_status)
                .map(|(plugin, _subscription)| plugin.config().transaction_commitment)
                .collect::<Vec<_>>(),
        );
        let transaction_batch_plugin_commitments: Arc<
            [crate::framework::plugin::TransactionCommitmentSelector],
        > = Arc::from(
            plugins
                .iter()
                .zip(plugin_subscriptions.iter())
                .filter(|(_plugin, subscription)| subscription.transaction_batch)
                .map(|(plugin, _subscription)| plugin.config().transaction_commitment)
                .collect::<Vec<_>>(),
        );
        let transaction_view_batch_plugin_inline_preferences: Arc<[bool]> = Arc::from(
            plugin_subscriptions
                .iter()
                .filter(|subscription| subscription.transaction_view_batch)
                .map(|subscription| subscription.inline_transaction_view_batch)
                .collect::<Vec<_>>(),
        );
        let transaction_view_batch_plugin_commitments: Arc<
            [crate::framework::plugin::TransactionCommitmentSelector],
        > = Arc::from(
            plugins
                .iter()
                .zip(plugin_subscriptions.iter())
                .filter(|(_plugin, subscription)| subscription.transaction_view_batch)
                .map(|(plugin, _subscription)| plugin.config().transaction_commitment)
                .collect::<Vec<_>>(),
        );
        let account_touch_plugins =
            collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
                subscription.account_touch
            });
        let account_update_plugins =
            collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
                subscription.account_update
            });
        let block_meta_plugins =
            collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
                subscription.block_meta
            });
        let slot_status_plugins =
            collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
                subscription.slot_status
            });
        let reorg_plugins = collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
            subscription.reorg
        });
        let recent_blockhash_plugins =
            collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
                subscription.recent_blockhash
            });
        let cluster_topology_plugins =
            collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
                subscription.cluster_topology
            });
        let leader_schedule_plugins =
            collect_hook_plugins(&plugins, &plugin_subscriptions, |subscription| {
                subscription.leader_schedule
            });
        let subscriptions = PluginHookSubscriptions {
            raw_packet: !raw_packet_plugins.is_empty(),
            shred: !shred_plugins.is_empty(),
            dataset: !dataset_plugins.is_empty(),
            transaction: !transaction_plugins.is_empty(),
            transaction_min_commitment: transaction_plugin_commitments
                .iter()
                .copied()
                .map(crate::framework::plugin::TransactionCommitmentSelector::minimum_required)
                .min()
                .unwrap_or(crate::event::TxCommitmentStatus::Processed),
            transaction_prefilter: transaction_plugin_prefilters.iter().any(Option::is_some),
            transaction_log: !transaction_log_plugins.is_empty(),
            transaction_status: !transaction_status_plugins.is_empty(),
            inline_transaction: plugin_subscriptions
                .iter()
                .any(|subscription| subscription.inline_transaction),
            transaction_batch: !transaction_batch_plugins.is_empty(),
            inline_transaction_batch: plugin_subscriptions
                .iter()
                .any(|subscription| subscription.inline_transaction_batch),
            transaction_view_batch: !transaction_view_batch_plugins.is_empty(),
            inline_transaction_view_batch: plugin_subscriptions
                .iter()
                .any(|subscription| subscription.inline_transaction_view_batch),
            account_touch: !account_touch_plugins.is_empty(),
            account_update: !account_update_plugins.is_empty(),
            block_meta: !block_meta_plugins.is_empty(),
            slot_status: !slot_status_plugins.is_empty(),
            reorg: !reorg_plugins.is_empty(),
            recent_blockhash: !recent_blockhash_plugins.is_empty(),
            cluster_topology: !cluster_topology_plugins.is_empty(),
            leader_schedule: !leader_schedule_plugins.is_empty(),
        };
        let dispatcher = PluginDispatcher::new(
            PluginDispatchTargets {
                raw_packet: raw_packet_plugins,
                shred: shred_plugins,
                dataset: dataset_plugins,
                transaction_log: transaction_log_plugins.clone(),
                transaction_status: transaction_status_plugins.clone(),
                transaction_batch: transaction_batch_plugins.clone(),
                transaction_view_batch: transaction_view_batch_plugins.clone(),
                account_touch: account_touch_plugins.clone(),
                account_update: account_update_plugins.clone(),
                block_meta: block_meta_plugins.clone(),
                slot_status: slot_status_plugins,
                reorg: reorg_plugins,
                recent_blockhash: recent_blockhash_plugins,
                cluster_topology: cluster_topology_plugins,
                leader_schedule: leader_schedule_plugins,
            },
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
            transaction_plugins,
            transaction_plugin_commitments,
            transaction_log_plugins,
            transaction_log_plugin_commitments,
            transaction_status_plugins,
            transaction_status_plugin_commitments,
            transaction_plugin_inline_preferences,
            transaction_plugin_prefilters,
            transaction_batch_plugins,
            transaction_batch_plugin_commitments,
            transaction_batch_plugin_inline_preferences,
            transaction_view_batch_plugins,
            transaction_view_batch_plugin_commitments,
            transaction_view_batch_plugin_inline_preferences,
            account_touch_plugins,
            account_update_plugins,
            block_meta_plugins,
            dispatcher,
            transaction_dispatcher,
            subscriptions,
            latest_observed_recent_blockhash: ArcShift::new(None),
            latest_observed_tpu_leader: ArcShift::new(None),
            lifecycle: Arc::new(PluginHostLifecycleState::default()),
        }
    }
}
