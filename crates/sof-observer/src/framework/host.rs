use std::{
    any::Any,
    panic::{self, AssertUnwindSafe},
    sync::Arc,
};

use crate::framework::{
    events::{DatasetEvent, RawPacketEvent, ShredEvent, TransactionEvent},
    plugin::ObserverPlugin,
};

/// Builder for constructing an immutable [`PluginHost`].
#[derive(Default)]
pub struct PluginHostBuilder {
    /// Plugins accumulated in registration order.
    plugins: Vec<Arc<dyn ObserverPlugin>>,
}

impl PluginHostBuilder {
    /// Creates an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
        PluginHost {
            plugins: Arc::from(self.plugins),
        }
    }
}

/// Immutable plugin registry and synchronous event dispatcher.
#[derive(Clone, Default)]
pub struct PluginHost {
    /// Immutable plugin collection in registration order.
    plugins: Arc<[Arc<dyn ObserverPlugin>]>,
}

impl PluginHost {
    /// Dispatches one event to all plugins and isolates individual plugin panics.
    fn dispatch_event<Event, Dispatch>(&self, hook: &'static str, event: Event, dispatch: Dispatch)
    where
        Event: Copy,
        Dispatch: Fn(&dyn ObserverPlugin, Event),
    {
        if self.plugins.is_empty() {
            return;
        }
        for plugin in self.plugins.iter() {
            let plugin_name = plugin.name();
            if let Err(payload) =
                panic::catch_unwind(AssertUnwindSafe(|| dispatch(plugin.as_ref(), event)))
            {
                tracing::error!(
                    plugin = plugin_name,
                    hook,
                    panic = %panic_payload_to_string(payload.as_ref()),
                    "plugin hook panicked; continuing runtime"
                );
            }
        }
    }

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

    /// Returns plugin identifiers in registration order.
    #[must_use]
    pub fn plugin_names(&self) -> Vec<&'static str> {
        self.plugins.iter().map(|plugin| plugin.name()).collect()
    }

    /// Dispatches raw packet hook to each registered plugin.
    pub fn on_raw_packet(&self, event: RawPacketEvent<'_>) {
        self.dispatch_event("on_raw_packet", event, |plugin, hook_event| {
            plugin.on_raw_packet(hook_event);
        });
    }

    /// Dispatches parsed shred hook to each registered plugin.
    pub fn on_shred(&self, event: ShredEvent<'_>) {
        self.dispatch_event("on_shred", event, |plugin, hook_event| {
            plugin.on_shred(hook_event);
        });
    }

    /// Dispatches reconstructed dataset hook to each registered plugin.
    pub fn on_dataset(&self, event: DatasetEvent) {
        self.dispatch_event("on_dataset", event, |plugin, hook_event| {
            plugin.on_dataset(hook_event);
        });
    }

    /// Dispatches reconstructed transaction hook to each registered plugin.
    pub fn on_transaction(&self, event: TransactionEvent<'_>) {
        self.dispatch_event("on_transaction", event, |plugin, hook_event| {
            plugin.on_transaction(hook_event);
        });
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use super::*;

    #[derive(Clone, Copy)]
    struct PluginA;

    impl ObserverPlugin for PluginA {
        fn name(&self) -> &'static str {
            "plugin-a"
        }
    }

    #[derive(Clone, Copy)]
    struct PluginB;

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

    impl ObserverPlugin for PanicPlugin {
        fn name(&self) -> &'static str {
            "panic-plugin"
        }

        fn on_dataset(&self, _event: DatasetEvent) {
            panic!("panic-plugin failed");
        }
    }

    #[derive(Clone)]
    struct CounterPlugin {
        counter: Arc<AtomicUsize>,
    }

    impl ObserverPlugin for CounterPlugin {
        fn name(&self) -> &'static str {
            "counter-plugin"
        }

        fn on_dataset(&self, _event: DatasetEvent) {
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
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn concurrent_multi_plugin_dispatch_is_consistent() {
        let counter_a = Arc::new(AtomicUsize::new(0));
        let counter_b = Arc::new(AtomicUsize::new(0));
        let host = Arc::new(
            PluginHostBuilder::new()
                .add_plugin(CounterPlugin {
                    counter: counter_a.clone(),
                })
                .add_plugin(CounterPlugin {
                    counter: counter_b.clone(),
                })
                .build(),
        );
        let threads = 8_usize;
        let iterations = 2_000_usize;
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
        let expected = threads.saturating_mul(iterations);
        assert_eq!(counter_a.load(Ordering::Relaxed), expected);
        assert_eq!(counter_b.load(Ordering::Relaxed), expected);
    }
}
