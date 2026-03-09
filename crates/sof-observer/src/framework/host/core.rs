use super::dispatch::{
    ClassifiedTransactionDispatch, PluginDispatchEvent, PluginDispatcher,
    SelectedAccountTouchDispatch, TransactionDispatchPriority, TransactionPluginDispatcher,
};
use super::state::{ObservedRecentBlockhashState, ObservedTpuLeaderState};
use super::*;
use crate::framework::AccountTouchEvent;
use crate::framework::events::AccountTouchEventRef;
use crate::framework::events::TransactionEventRef;

/// Immutable plugin registry and async event dispatcher.
#[derive(Clone)]
pub struct PluginHost {
    /// Immutable plugin collection in registration order.
    pub(super) plugins: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Optional async dispatcher state (absent when no plugins are registered).
    pub(super) dispatcher: Option<PluginDispatcher>,
    /// Optional sharded accepted-transaction dispatcher.
    pub(super) transaction_dispatcher: Option<TransactionPluginDispatcher>,
    /// Hook-interest bitmap so unused hooks never enter the async queue.
    pub(super) subscriptions: PluginHookSubscriptions,
    /// Latest observed recent blockhash snapshot.
    pub(super) latest_observed_recent_blockhash: ArcShift<Option<ObservedRecentBlockhashState>>,
    /// Latest observed TPU leader snapshot.
    pub(super) latest_observed_tpu_leader: ArcShift<Option<ObservedTpuLeaderState>>,
}

impl Default for PluginHost {
    fn default() -> Self {
        Self {
            plugins: Arc::from(Vec::<Arc<dyn ObserverPlugin>>::new()),
            dispatcher: None,
            transaction_dispatcher: None,
            subscriptions: PluginHookSubscriptions::default(),
            latest_observed_recent_blockhash: ArcShift::new(None),
            latest_observed_tpu_leader: ArcShift::new(None),
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

    /// Returns true when at least one plugin wants dataset callbacks.
    #[must_use]
    pub const fn wants_dataset(&self) -> bool {
        self.subscriptions.dataset
    }

    /// Returns true when at least one plugin wants transaction callbacks.
    #[must_use]
    pub const fn wants_transaction(&self) -> bool {
        self.subscriptions.transaction
    }

    /// Returns true when at least one plugin wants account-touch callbacks.
    #[must_use]
    pub const fn wants_account_touch(&self) -> bool {
        self.subscriptions.account_touch
    }

    /// Returns true when at least one plugin wants recent-blockhash callbacks.
    #[must_use]
    pub const fn wants_recent_blockhash(&self) -> bool {
        self.subscriptions.recent_blockhash
    }

    /// Returns total dropped hook events due to queue backpressure/closure.
    #[must_use]
    pub fn dropped_event_count(&self) -> u64 {
        self.general_dropped_event_count()
            .saturating_add(self.transaction_dropped_event_count())
            .saturating_add(self.background_transaction_dropped_event_count())
    }

    /// Returns dropped non-transaction hook events due to queue backpressure/closure.
    #[must_use]
    pub fn general_dropped_event_count(&self) -> u64 {
        self.dispatcher
            .as_ref()
            .map_or(0, PluginDispatcher::dropped_count)
    }

    /// Returns dropped accepted-transaction hook events due to queue backpressure/closure.
    #[must_use]
    pub fn transaction_dropped_event_count(&self) -> u64 {
        self.transaction_dispatcher
            .as_ref()
            .map_or(0, TransactionPluginDispatcher::critical_dropped_count)
    }

    /// Returns dropped background accepted-transaction hook events.
    #[must_use]
    pub fn background_transaction_dropped_event_count(&self) -> u64 {
        self.transaction_dispatcher
            .as_ref()
            .map_or(0, TransactionPluginDispatcher::background_dropped_count)
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
            .shared_get()
            .as_ref()
            .map(|state| (state.slot, state.recent_blockhash))
    }

    /// Returns latest observed TPU leader from leader-schedule events.
    #[must_use]
    pub fn latest_observed_tpu_leader(&self) -> Option<LeaderScheduleEntry> {
        self.latest_observed_tpu_leader
            .shared_get()
            .as_ref()
            .map(|state| LeaderScheduleEntry {
                slot: state.slot,
                leader: state.leader,
            })
    }

    /// Classifies one decoded transaction on borrowed data before owned event allocation.
    #[must_use]
    pub(crate) fn classify_transaction_ref(
        &self,
        event: TransactionEventRef<'_>,
    ) -> ClassifiedTransactionDispatch {
        if !self.subscriptions.transaction || self.transaction_dispatcher.is_none() {
            return ClassifiedTransactionDispatch::empty();
        }
        let mut dispatch = ClassifiedTransactionDispatch::empty();
        for plugin in self.plugins.iter() {
            if !plugin.wants_transaction() {
                continue;
            }
            dispatch.push(plugin.transaction_interest_ref(event), Arc::clone(plugin));
        }
        dispatch
    }

    /// Enqueues one already-classified decoded transaction hook to registered plugins.
    pub(crate) fn on_classified_transaction(
        &self,
        dispatch: ClassifiedTransactionDispatch,
        event: TransactionEvent,
    ) {
        if !self.subscriptions.transaction {
            return;
        }
        let Some(dispatcher) = &self.transaction_dispatcher else {
            return;
        };
        let (critical, background) = dispatch.into_dispatches(Arc::new(event));
        if let Some(dispatch_event) = critical {
            dispatcher.dispatch(TransactionDispatchPriority::Critical, dispatch_event);
        }
        if let Some(dispatch_event) = background {
            dispatcher.dispatch(TransactionDispatchPriority::Background, dispatch_event);
        }
    }

    /// Selects interested account-touch plugins on borrowed transaction data.
    #[must_use]
    pub(crate) fn classify_account_touch_ref(
        &self,
        event: AccountTouchEventRef<'_>,
    ) -> Vec<Arc<dyn ObserverPlugin>> {
        if !self.subscriptions.account_touch || self.dispatcher.is_none() {
            return Vec::new();
        }
        let mut selected = Vec::new();
        for plugin in self.plugins.iter() {
            if plugin.wants_account_touch() && plugin.accepts_account_touch_ref(event) {
                selected.push(Arc::clone(plugin));
            }
        }
        selected
    }

    /// Enqueues one account-touch hook to only the interested plugins.
    pub(crate) fn on_selected_account_touch(
        &self,
        plugins: Vec<Arc<dyn ObserverPlugin>>,
        event: AccountTouchEvent,
    ) {
        if !self.subscriptions.account_touch {
            return;
        }
        if let Some(dispatcher) = &self.dispatcher
            && let Some(event) = SelectedAccountTouchDispatch::from_plugins(plugins, event)
        {
            dispatcher.dispatch(PluginDispatchEvent::SelectedAccountTouch(event));
        }
    }

    /// Enqueues raw packet hook to registered plugins.
    pub fn on_raw_packet(&self, event: RawPacketEvent) {
        if self.subscriptions.raw_packet
            && let Some(dispatcher) = &self.dispatcher
        {
            dispatcher.dispatch(PluginDispatchEvent::RawPacket(event));
        }
    }

    /// Enqueues parsed shred hook to registered plugins.
    pub fn on_shred(&self, event: ShredEvent) {
        if self.subscriptions.shred
            && let Some(dispatcher) = &self.dispatcher
        {
            dispatcher.dispatch(PluginDispatchEvent::Shred(event));
        }
    }

    /// Enqueues reconstructed dataset hook to registered plugins.
    pub fn on_dataset(&self, event: DatasetEvent) {
        if self.subscriptions.dataset
            && let Some(dispatcher) = &self.dispatcher
        {
            dispatcher.dispatch(PluginDispatchEvent::Dataset(event));
        }
    }

    /// Enqueues reconstructed transaction hook to registered plugins.
    pub fn on_transaction(&self, event: TransactionEvent) {
        let dispatch = self.classify_transaction_ref(TransactionEventRef {
            slot: event.slot,
            commitment_status: event.commitment_status,
            confirmed_slot: event.confirmed_slot,
            finalized_slot: event.finalized_slot,
            signature: event.signature,
            tx: event.tx.as_ref(),
            kind: event.kind,
        });
        if dispatch.is_empty() {
            return;
        }
        self.on_classified_transaction(dispatch, event);
    }

    /// Enqueues account-touch hook to registered plugins.
    pub fn on_account_touch(&self, event: AccountTouchEvent) {
        if self.subscriptions.account_touch
            && let Some(dispatcher) = &self.dispatcher
        {
            dispatcher.dispatch(PluginDispatchEvent::AccountTouch(Arc::new(event)));
        }
    }

    /// Enqueues slot-status transition hook to registered plugins.
    pub fn on_slot_status(&self, event: SlotStatusEvent) {
        if self.subscriptions.slot_status
            && let Some(dispatcher) = &self.dispatcher
        {
            dispatcher.dispatch(PluginDispatchEvent::SlotStatus(event));
        }
    }

    /// Enqueues canonical reorg hook to registered plugins.
    pub fn on_reorg(&self, event: ReorgEvent) {
        if self.subscriptions.reorg
            && let Some(dispatcher) = &self.dispatcher
        {
            dispatcher.dispatch(PluginDispatchEvent::Reorg(event));
        }
    }

    /// Updates latest observed recent blockhash and enqueues hook when hash changed.
    pub fn on_recent_blockhash(&self, event: ObservedRecentBlockhashEvent) {
        let mut latest_observed_recent_blockhash = self.latest_observed_recent_blockhash.clone();
        let mut next_state = *latest_observed_recent_blockhash.shared_get();
        let (state_changed, should_emit) = match next_state.as_mut() {
            None => {
                next_state = Some(ObservedRecentBlockhashState {
                    slot: event.slot,
                    recent_blockhash: event.recent_blockhash,
                });
                (true, true)
            }
            Some(current) if event.slot < current.slot => (false, false),
            Some(current)
                if event.slot == current.slot
                    && event.recent_blockhash == current.recent_blockhash =>
            {
                (false, false)
            }
            Some(current)
                if event.slot > current.slot
                    && event.recent_blockhash == current.recent_blockhash =>
            {
                current.slot = event.slot;
                (true, false)
            }
            Some(current) => {
                current.slot = event.slot;
                current.recent_blockhash = event.recent_blockhash;
                (true, true)
            }
        };
        if state_changed {
            latest_observed_recent_blockhash.update(next_state);
        }
        if should_emit
            && self.subscriptions.recent_blockhash
            && let Some(dispatcher) = &self.dispatcher
        {
            dispatcher.dispatch(PluginDispatchEvent::ObservedRecentBlockhash(event));
        }
    }

    /// Enqueues cluster topology diff/snapshot hook to registered plugins.
    pub fn on_cluster_topology(&self, event: ClusterTopologyEvent) {
        if self.subscriptions.cluster_topology
            && let Some(dispatcher) = &self.dispatcher
        {
            dispatcher.dispatch(PluginDispatchEvent::ClusterTopology(event));
        }
    }

    /// Enqueues leader-schedule diff/snapshot hook to registered plugins.
    pub fn on_leader_schedule(&self, event: LeaderScheduleEvent) {
        let mut latest_observed_tpu_leader = self.latest_observed_tpu_leader.clone();
        if let Some(newest_entry) = event
            .added_leaders
            .iter()
            .chain(event.updated_leaders.iter())
            .chain(event.snapshot_leaders.iter())
            .copied()
            .max_by_key(|entry| entry.slot)
        {
            let mut next_state = *latest_observed_tpu_leader.shared_get();
            let state_changed = match next_state.as_mut() {
                None => {
                    next_state = Some(ObservedTpuLeaderState {
                        slot: newest_entry.slot,
                        leader: newest_entry.leader,
                    });
                    true
                }
                Some(current)
                    if newest_entry.slot > current.slot
                        || (newest_entry.slot == current.slot
                            && newest_entry.leader != current.leader) =>
                {
                    current.slot = newest_entry.slot;
                    current.leader = newest_entry.leader;
                    true
                }
                Some(_) => false,
            };
            if state_changed {
                latest_observed_tpu_leader.update(next_state);
            }
        }
        if self.subscriptions.leader_schedule
            && let Some(dispatcher) = &self.dispatcher
        {
            dispatcher.dispatch(PluginDispatchEvent::LeaderSchedule(event));
        }
    }
}
