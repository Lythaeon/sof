use super::dispatch::{PluginDispatchEvent, PluginDispatcher};
use super::state::{ObservedRecentBlockhashState, ObservedTpuLeaderState};
use super::*;

/// Immutable plugin registry and async event dispatcher.
#[derive(Clone)]
pub struct PluginHost {
    /// Immutable plugin collection in registration order.
    pub(super) plugins: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Optional async dispatcher state (absent when no plugins are registered).
    pub(super) dispatcher: Option<PluginDispatcher>,
    /// Latest observed recent blockhash snapshot.
    pub(super) latest_observed_recent_blockhash: Arc<RwLock<Option<ObservedRecentBlockhashState>>>,
    /// Latest observed TPU leader snapshot.
    pub(super) latest_observed_tpu_leader: Arc<RwLock<Option<ObservedTpuLeaderState>>>,
}

impl Default for PluginHost {
    fn default() -> Self {
        Self {
            plugins: Arc::from(Vec::<Arc<dyn ObserverPlugin>>::new()),
            dispatcher: None,
            latest_observed_recent_blockhash: Arc::new(RwLock::new(None)),
            latest_observed_tpu_leader: Arc::new(RwLock::new(None)),
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

    /// Returns latest observed recent blockhash snapshot from live runtime data.
    #[must_use]
    pub fn latest_observed_recent_blockhash(&self) -> Option<(u64, [u8; 32])> {
        self.latest_observed_recent_blockhash
            .read()
            .ok()
            .and_then(|guard| {
                guard
                    .as_ref()
                    .map(|state| (state.slot, state.recent_blockhash))
            })
    }

    /// Returns latest observed TPU leader from leader-schedule events.
    #[must_use]
    pub fn latest_observed_tpu_leader(&self) -> Option<LeaderScheduleEntry> {
        self.latest_observed_tpu_leader
            .read()
            .ok()
            .and_then(|guard| {
                guard.as_ref().map(|state| LeaderScheduleEntry {
                    slot: state.slot,
                    leader: state.leader,
                })
            })
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

    /// Updates latest observed recent blockhash and enqueues hook when hash changed.
    pub fn on_recent_blockhash(&self, event: ObservedRecentBlockhashEvent) {
        let should_emit = self
            .latest_observed_recent_blockhash
            .write()
            .ok()
            .is_some_and(|mut guard| match guard.as_mut() {
                None => {
                    *guard = Some(ObservedRecentBlockhashState {
                        slot: event.slot,
                        recent_blockhash: event.recent_blockhash,
                    });
                    true
                }
                Some(current) if event.slot < current.slot => false,
                Some(current)
                    if event.slot == current.slot
                        && event.recent_blockhash == current.recent_blockhash =>
                {
                    false
                }
                Some(current)
                    if event.slot > current.slot
                        && event.recent_blockhash == current.recent_blockhash =>
                {
                    current.slot = event.slot;
                    false
                }
                Some(current) => {
                    current.slot = event.slot;
                    current.recent_blockhash = event.recent_blockhash;
                    true
                }
            });
        if should_emit && let Some(dispatcher) = &self.dispatcher {
            dispatcher.dispatch(
                "on_recent_blockhash",
                PluginDispatchEvent::ObservedRecentBlockhash(event),
            );
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
        if let Some(newest_entry) = event
            .added_leaders
            .iter()
            .chain(event.updated_leaders.iter())
            .chain(event.snapshot_leaders.iter())
            .copied()
            .max_by_key(|entry| entry.slot)
            && let Ok(mut guard) = self.latest_observed_tpu_leader.write()
        {
            match guard.as_mut() {
                None => {
                    *guard = Some(ObservedTpuLeaderState {
                        slot: newest_entry.slot,
                        leader: newest_entry.leader,
                    });
                }
                Some(current)
                    if newest_entry.slot > current.slot
                        || (newest_entry.slot == current.slot
                            && newest_entry.leader != current.leader) =>
                {
                    current.slot = newest_entry.slot;
                    current.leader = newest_entry.leader;
                }
                Some(_) => {}
            }
        }
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.dispatch(
                "on_leader_schedule",
                PluginDispatchEvent::LeaderSchedule(event),
            );
        }
    }
}
