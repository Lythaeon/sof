#![allow(clippy::missing_docs_in_private_items)]

use super::dispatch::{
    ClassifiedAccountTouchDispatch, ClassifiedTransactionBatchDispatch,
    ClassifiedTransactionDispatch, ClassifiedTransactionViewBatchDispatch, PluginDispatchEvent,
    PluginDispatcher, SelectedAccountTouchDispatch, SelectedTransactionLogDispatch,
    TransactionDispatchPriority, TransactionDispatchQueueMetrics, TransactionPluginDispatcher,
};
use super::state::{ObservedRecentBlockhashState, ObservedTpuLeaderState};

use super::*;
use crate::framework::AccountTouchEvent;
use crate::framework::PluginContext;
use crate::framework::events::AccountTouchEventRef;
use crate::framework::events::TransactionEventRef;
use crate::framework::pubkey_bytes;
use agave_transaction_view::{
    transaction_data::TransactionData, transaction_view::SanitizedTransactionView,
};
use std::time::Instant;

/// Selects which transaction subscribers should receive a callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransactionDispatchScope {
    /// Deliver transactions to both inline and deferred subscribers.
    All,
    /// Deliver only to plugins that explicitly requested inline dispatch.
    InlineOnly,
    /// Deliver only to plugins that still rely on deferred dispatch.
    DeferredOnly,
}

impl TransactionDispatchScope {
    /// Returns whether this scope includes callbacks for the requested inline mode.
    const fn includes(self, inline_requested: bool) -> bool {
        match self {
            Self::All => true,
            Self::InlineOnly => inline_requested,
            Self::DeferredOnly => !inline_requested,
        }
    }
}

/// Result of running only compiled transaction prefilters within one delivery scope.
pub(crate) struct PrefilteredTransactionDispatch {
    /// Preclassified dispatch targets from compiled prefilters.
    pub(crate) dispatch: ClassifiedTransactionDispatch,
    /// Whether full decoded classification is still required for plugins without prefilters.
    pub(crate) needs_full_classification: bool,
}

impl PrefilteredTransactionDispatch {
    const fn empty() -> Self {
        Self {
            dispatch: ClassifiedTransactionDispatch::empty(),
            needs_full_classification: false,
        }
    }
}

/// Immutable plugin registry and async event dispatcher.
///
/// Build this through [`PluginHost::builder`] and pass it into
/// [`crate::ObserverRuntime::with_plugin_host`] when embedding SOF.
///
/// # Examples
///
/// ```rust
/// use async_trait::async_trait;
/// use sof::framework::{ObserverPlugin, PluginConfig, PluginHost};
///
/// struct BlockhashPlugin;
///
/// #[async_trait]
/// impl ObserverPlugin for BlockhashPlugin {
///     fn config(&self) -> PluginConfig {
///         PluginConfig::new().with_recent_blockhash()
///     }
/// }
///
/// let host = PluginHost::builder().add_plugin(BlockhashPlugin).build();
///
/// assert_eq!(host.len(), 1);
/// assert!(host.wants_recent_blockhash());
/// ```
#[derive(Clone)]
pub struct PluginHost {
    /// Immutable plugin collection in registration order.
    pub(super) plugins: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Plugins interested in transaction callbacks.
    pub(super) transaction_plugins: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Per-transaction-plugin commitment selector in registration order.
    pub(super) transaction_plugin_commitments:
        Arc<[crate::framework::plugin::TransactionCommitmentSelector]>,
    /// Plugins interested in transaction-log callbacks.
    pub(super) transaction_log_plugins: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Per-transaction-log-plugin commitment selector in registration order.
    pub(super) transaction_log_plugin_commitments:
        Arc<[crate::framework::plugin::TransactionCommitmentSelector]>,
    /// Per-transaction-plugin inline delivery preference in registration order.
    pub(super) transaction_plugin_inline_preferences: Arc<[bool]>,
    /// Optional compiled transaction prefilters in registration order.
    pub(super) transaction_plugin_prefilters: Arc<[Option<crate::framework::TransactionPrefilter>]>,
    /// Plugins interested in transaction-batch callbacks.
    pub(super) transaction_batch_plugins: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Per-transaction-batch-plugin commitment selector in registration order.
    pub(super) transaction_batch_plugin_commitments:
        Arc<[crate::framework::plugin::TransactionCommitmentSelector]>,
    /// Per-transaction-batch-plugin inline delivery preference in registration order.
    pub(super) transaction_batch_plugin_inline_preferences: Arc<[bool]>,
    /// Plugins interested in transaction-view-batch callbacks.
    pub(super) transaction_view_batch_plugins: Arc<[Arc<dyn ObserverPlugin>]>,
    /// Per-transaction-view-batch-plugin commitment selector in registration order.
    pub(super) transaction_view_batch_plugin_commitments:
        Arc<[crate::framework::plugin::TransactionCommitmentSelector]>,
    /// Per-transaction-view-batch-plugin inline delivery preference in registration order.
    pub(super) transaction_view_batch_plugin_inline_preferences: Arc<[bool]>,
    /// Plugins interested in account-touch callbacks.
    pub(super) account_touch_plugins: Arc<[Arc<dyn ObserverPlugin>]>,
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
    /// Shared lifecycle guard for startup/shutdown hooks.
    pub(super) lifecycle: Arc<PluginHostLifecycleState>,
}

impl Default for PluginHost {
    fn default() -> Self {
        Self {
            plugins: Arc::from(Vec::<Arc<dyn ObserverPlugin>>::new()),
            transaction_plugins: Arc::from(Vec::<Arc<dyn ObserverPlugin>>::new()),
            transaction_plugin_commitments: Arc::from(Vec::<
                crate::framework::plugin::TransactionCommitmentSelector,
            >::new()),
            transaction_log_plugins: Arc::from(Vec::<Arc<dyn ObserverPlugin>>::new()),
            transaction_log_plugin_commitments: Arc::from(Vec::<
                crate::framework::plugin::TransactionCommitmentSelector,
            >::new()),
            transaction_plugin_inline_preferences: Arc::from(Vec::<bool>::new()),
            transaction_plugin_prefilters: Arc::from(Vec::<
                Option<crate::framework::TransactionPrefilter>,
            >::new()),
            transaction_batch_plugins: Arc::from(Vec::<Arc<dyn ObserverPlugin>>::new()),
            transaction_batch_plugin_commitments: Arc::from(Vec::<
                crate::framework::plugin::TransactionCommitmentSelector,
            >::new()),
            transaction_batch_plugin_inline_preferences: Arc::from(Vec::<bool>::new()),
            transaction_view_batch_plugins: Arc::from(Vec::<Arc<dyn ObserverPlugin>>::new()),
            transaction_view_batch_plugin_commitments: Arc::from(Vec::<
                crate::framework::plugin::TransactionCommitmentSelector,
            >::new()),
            transaction_view_batch_plugin_inline_preferences: Arc::from(Vec::<bool>::new()),
            account_touch_plugins: Arc::from(Vec::<Arc<dyn ObserverPlugin>>::new()),
            dispatcher: None,
            transaction_dispatcher: None,
            subscriptions: PluginHookSubscriptions::default(),
            latest_observed_recent_blockhash: ArcShift::new(None),
            latest_observed_tpu_leader: ArcShift::new(None),
            lifecycle: Arc::new(PluginHostLifecycleState::default()),
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

    /// Returns true when at least one transaction subscriber wants this commitment or stronger.
    #[must_use]
    pub(crate) fn transaction_enabled_at_commitment(
        &self,
        commitment_status: crate::event::TxCommitmentStatus,
    ) -> bool {
        self.subscriptions.transaction
            && commitment_status.satisfies_minimum(self.subscriptions.transaction_min_commitment)
    }

    /// Returns true when at least one transaction subscriber exposes a compiled prefilter.
    #[must_use]
    pub const fn has_transaction_prefilter(&self) -> bool {
        self.subscriptions.transaction_prefilter
    }

    /// Returns true when at least one in-scope transaction subscriber exposes a compiled prefilter.
    #[must_use]
    pub(crate) fn has_transaction_prefilter_at_commitment(
        &self,
        commitment_status: crate::event::TxCommitmentStatus,
    ) -> bool {
        self.transaction_plugin_prefilters
            .iter()
            .zip(self.transaction_plugin_commitments.iter().copied())
            .any(|(prefilter, selector)| prefilter.is_some() && selector.matches(commitment_status))
    }

    /// Returns true when at least one plugin wants transaction-log callbacks.
    #[must_use]
    pub const fn wants_transaction_log(&self) -> bool {
        self.subscriptions.transaction_log
    }

    /// Returns true when at least one plugin requested inline transaction dispatch.
    #[must_use]
    pub const fn wants_inline_transaction_dispatch(&self) -> bool {
        self.subscriptions.inline_transaction
    }

    /// Returns true when at least one plugin still relies on deferred transaction dispatch.
    #[must_use]
    pub fn wants_deferred_transaction_dispatch(&self) -> bool {
        self.transaction_plugin_inline_preferences
            .iter()
            .any(|inline_requested| !*inline_requested)
    }

    /// Returns true when at least one plugin wants transaction-batch callbacks.
    #[must_use]
    pub const fn wants_transaction_batch(&self) -> bool {
        self.subscriptions.transaction_batch
    }

    /// Returns true when at least one plugin requested inline transaction-batch dispatch.
    #[must_use]
    pub const fn wants_inline_transaction_batch_dispatch(&self) -> bool {
        self.subscriptions.inline_transaction_batch
    }

    /// Returns true when at least one plugin wants transaction-view-batch callbacks.
    #[must_use]
    pub const fn wants_transaction_view_batch(&self) -> bool {
        self.subscriptions.transaction_view_batch
    }

    /// Returns true when at least one plugin requested inline transaction-view-batch dispatch.
    #[must_use]
    pub const fn wants_inline_transaction_view_batch_dispatch(&self) -> bool {
        self.subscriptions.inline_transaction_view_batch
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

    /// Returns true when at least one plugin wants raw-packet callbacks.
    #[must_use]
    pub const fn wants_raw_packet(&self) -> bool {
        self.subscriptions.raw_packet
    }

    /// Returns true when at least one plugin wants parsed-shred callbacks.
    #[must_use]
    pub const fn wants_shred(&self) -> bool {
        self.subscriptions.shred
    }

    /// Returns true when at least one plugin wants cluster-topology callbacks.
    #[must_use]
    pub const fn wants_cluster_topology(&self) -> bool {
        self.subscriptions.cluster_topology
    }

    /// Returns true when at least one plugin wants leader-schedule callbacks.
    #[must_use]
    pub const fn wants_leader_schedule(&self) -> bool {
        self.subscriptions.leader_schedule
    }

    /// Returns true when at least one plugin wants slot-status callbacks.
    #[must_use]
    pub const fn wants_slot_status(&self) -> bool {
        self.subscriptions.slot_status
    }

    /// Returns true when at least one plugin wants reorg callbacks.
    #[must_use]
    pub const fn wants_reorg(&self) -> bool {
        self.subscriptions.reorg
    }

    /// Returns current queue depth for non-transaction plugin dispatch.
    #[must_use]
    pub fn general_queue_depth(&self) -> u64 {
        self.dispatcher
            .as_ref()
            .map_or(0, PluginDispatcher::queue_depth)
    }

    /// Returns maximum queue depth observed for non-transaction plugin dispatch.
    #[must_use]
    pub fn general_max_queue_depth(&self) -> u64 {
        self.dispatcher
            .as_ref()
            .map_or(0, PluginDispatcher::max_queue_depth)
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

    /// Returns aggregated transaction-dispatch queue metrics by lane.
    #[must_use]
    pub(crate) fn transaction_queue_metrics(&self) -> TransactionDispatchQueueMetrics {
        self.transaction_dispatcher
            .as_ref()
            .map_or_else(TransactionDispatchQueueMetrics::default, |dispatcher| {
                dispatcher.queue_metrics()
            })
    }

    /// Returns plugin identifiers in registration order.
    #[must_use]
    pub fn plugin_names(&self) -> Vec<&'static str> {
        self.plugins.iter().map(|plugin| plugin.name()).collect()
    }

    /// Runs plugin setup hooks once before the runtime main loop begins.
    ///
    /// # Errors
    ///
    /// Returns [`PluginHostStartupError`] when any registered plugin fails its
    /// `setup` hook. Plugins started earlier in the same setup pass are
    /// shut down best-effort before the error is returned.
    pub async fn startup(&self) -> Result<(), PluginHostStartupError> {
        if self
            .lifecycle
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        let mut started_plugins: Vec<Arc<dyn ObserverPlugin>> =
            Vec::with_capacity(self.plugins.len());
        for plugin in self.plugins.iter() {
            let plugin_name = plugin.name();
            let context = PluginContext { plugin_name };
            if let Err(error) = plugin.setup(context.clone()).await {
                for started_plugin in started_plugins.iter().rev() {
                    started_plugin
                        .shutdown(PluginContext {
                            plugin_name: started_plugin.name(),
                        })
                        .await;
                }
                self.lifecycle.started.store(false, Ordering::Release);
                return Err(PluginHostStartupError {
                    plugin: plugin_name,
                    reason: error.to_string(),
                });
            }
            started_plugins.push(Arc::clone(plugin));
        }
        Ok(())
    }

    /// Runs plugin shutdown hooks once after runtime ingest has stopped.
    pub async fn shutdown(&self) {
        if self
            .lifecycle
            .started
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        for plugin in self.plugins.iter().rev() {
            plugin
                .shutdown(PluginContext {
                    plugin_name: plugin.name(),
                })
                .await;
        }
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
                leader: pubkey_bytes(state.leader),
            })
    }

    /// Classifies one decoded transaction on borrowed data before owned event allocation.
    #[must_use]
    pub(crate) fn classify_transaction_ref(
        &self,
        event: TransactionEventRef<'_>,
    ) -> ClassifiedTransactionDispatch {
        self.classify_transaction_ref_in_scope(event, TransactionDispatchScope::All)
    }

    /// Classifies one decoded transaction within one explicit delivery scope.
    #[must_use]
    pub(crate) fn classify_transaction_ref_in_scope(
        &self,
        event: TransactionEventRef<'_>,
        scope: TransactionDispatchScope,
    ) -> ClassifiedTransactionDispatch {
        if !self.wants_transaction_dispatch_in_scope(scope) || self.transaction_dispatcher.is_none()
        {
            return ClassifiedTransactionDispatch::empty();
        }
        let mut dispatch = ClassifiedTransactionDispatch::empty();
        for (((plugin, inline_requested), prefilter), commitment_selector) in self
            .transaction_plugins
            .iter()
            .zip(self.transaction_plugin_inline_preferences.iter().copied())
            .zip(self.transaction_plugin_prefilters.iter())
            .zip(self.transaction_plugin_commitments.iter().copied())
        {
            if !scope.includes(inline_requested) {
                continue;
            }
            if !commitment_selector.matches(event.commitment_status) {
                continue;
            }
            dispatch.push(
                prefilter.as_ref().map_or_else(
                    || plugin.transaction_interest_ref(&event),
                    |filter| filter.classify_ref(&event),
                ),
                inline_requested,
                Arc::clone(plugin),
            );
        }
        dispatch
    }

    /// Runs compiled transaction prefilters against one serialized transaction view.
    #[must_use]
    pub(crate) fn classify_transaction_view_in_scope<D: TransactionData>(
        &self,
        view: &SanitizedTransactionView<D>,
        commitment_status: crate::event::TxCommitmentStatus,
        scope: TransactionDispatchScope,
    ) -> PrefilteredTransactionDispatch {
        if !self.wants_transaction_dispatch_in_scope(scope) || self.transaction_dispatcher.is_none()
        {
            return PrefilteredTransactionDispatch::empty();
        }
        let mut dispatch = ClassifiedTransactionDispatch::empty();
        let mut needs_full_classification = false;
        for (((plugin, inline_requested), prefilter), commitment_selector) in self
            .transaction_plugins
            .iter()
            .zip(self.transaction_plugin_inline_preferences.iter().copied())
            .zip(self.transaction_plugin_prefilters.iter())
            .zip(self.transaction_plugin_commitments.iter().copied())
        {
            if !scope.includes(inline_requested) {
                continue;
            }
            if !commitment_selector.matches(commitment_status) {
                continue;
            }
            if let Some(filter) = prefilter.as_ref() {
                dispatch.push(
                    filter.classify_view_ref(view),
                    inline_requested,
                    Arc::clone(plugin),
                );
            } else {
                needs_full_classification = true;
            }
        }
        PrefilteredTransactionDispatch {
            dispatch,
            needs_full_classification,
        }
    }

    /// Returns true when the requested transaction delivery scope has any listeners.
    #[must_use]
    pub(crate) fn wants_transaction_dispatch_in_scope(
        &self,
        scope: TransactionDispatchScope,
    ) -> bool {
        match scope {
            TransactionDispatchScope::All => self.subscriptions.transaction,
            TransactionDispatchScope::InlineOnly => self.subscriptions.inline_transaction,
            TransactionDispatchScope::DeferredOnly => self.wants_deferred_transaction_dispatch(),
        }
    }

    /// Enqueues one already-classified decoded transaction hook to registered plugins.
    #[expect(
        clippy::too_many_arguments,
        reason = "hot-path dispatch keeps scalar timing and dataset metadata explicit"
    )]
    pub(crate) fn on_classified_transaction(
        &self,
        dispatch: ClassifiedTransactionDispatch,
        event: TransactionEvent,
        completed_at: Instant,
        first_shred_observed_at: Instant,
        last_shred_observed_at: Instant,
        inline_source: InlineTransactionDispatchSource,
        dataset_tx_count: u32,
        dataset_tx_position: u32,
    ) {
        if !self.subscriptions.transaction {
            return;
        }
        let Some(dispatcher) = &self.transaction_dispatcher else {
            return;
        };
        let (critical_inline, critical, background) = dispatch.into_dispatches(
            event,
            completed_at,
            first_shred_observed_at,
            last_shred_observed_at,
            inline_source,
            dataset_tx_count,
            dataset_tx_position,
        );
        if let Some(dispatch_event) = critical_inline {
            dispatcher.dispatch_inline_critical(dispatch_event);
        }
        if let Some(dispatch_event) = critical {
            dispatcher.dispatch(TransactionDispatchPriority::Critical, dispatch_event);
        }
        if let Some(dispatch_event) = background {
            dispatcher.dispatch(TransactionDispatchPriority::Background, dispatch_event);
        }
    }

    /// Enqueues one completed-dataset transaction batch hook to registered plugins.
    pub(crate) fn on_transaction_batch(&self, event: TransactionBatchEvent, completed_at: Instant) {
        if !self.subscriptions.transaction_batch {
            return;
        }
        let Some(dispatcher) = &self.dispatcher else {
            return;
        };
        let dispatch = ClassifiedTransactionBatchDispatch::from_plugins(
            &self.transaction_batch_plugins,
            &self.transaction_batch_plugin_commitments,
            &self.transaction_batch_plugin_inline_preferences,
            event,
            completed_at,
        );
        if let Some(dispatch_event) = dispatch {
            dispatcher.dispatch(PluginDispatchEvent::TransactionBatch(dispatch_event));
        }
    }

    /// Enqueues one completed-dataset transaction-view batch hook to registered plugins.
    pub(crate) fn on_transaction_view_batch(
        &self,
        event: crate::framework::TransactionViewBatchEvent,
        completed_at: Instant,
    ) {
        if !self.subscriptions.transaction_view_batch {
            return;
        }
        let Some(dispatcher) = &self.dispatcher else {
            return;
        };
        let dispatch = ClassifiedTransactionViewBatchDispatch::from_plugins(
            &self.transaction_view_batch_plugins,
            &self.transaction_view_batch_plugin_commitments,
            &self.transaction_view_batch_plugin_inline_preferences,
            event,
            completed_at,
        );
        if let Some(dispatch_event) = dispatch {
            dispatcher.dispatch(PluginDispatchEvent::TransactionViewBatch(dispatch_event));
        }
    }

    /// Selects interested account-touch plugins on borrowed transaction data.
    #[must_use]
    pub(crate) fn classify_account_touch_ref(
        &self,
        event: AccountTouchEventRef<'_>,
    ) -> ClassifiedAccountTouchDispatch {
        if !self.subscriptions.account_touch || self.dispatcher.is_none() {
            return ClassifiedAccountTouchDispatch::empty();
        }
        let mut selected = ClassifiedAccountTouchDispatch::empty();
        for plugin in self.account_touch_plugins.iter() {
            if plugin.accepts_account_touch_ref(&event) {
                selected.push(Arc::clone(plugin));
            }
        }
        selected
    }

    /// Enqueues one account-touch hook to only the interested plugins.
    pub(crate) fn on_selected_account_touch(
        &self,
        dispatch: ClassifiedAccountTouchDispatch,
        event: AccountTouchEvent,
    ) {
        if !self.subscriptions.account_touch {
            return;
        }
        if let Some(dispatcher) = &self.dispatcher
            && let Some(event) = SelectedAccountTouchDispatch::from_classified(dispatch, event)
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

    /// Enqueues provider/websocket transaction-log hook to registered plugins.
    pub fn on_transaction_log(&self, event: crate::framework::TransactionLogEvent) {
        if !self.subscriptions.transaction_log {
            return;
        }
        let Some(dispatcher) = &self.dispatcher else {
            return;
        };
        if let Some(dispatch) = SelectedTransactionLogDispatch::from_plugins(
            &self.transaction_log_plugins,
            &self.transaction_log_plugin_commitments,
            event,
        ) {
            dispatcher.dispatch(PluginDispatchEvent::SelectedTransactionLog(dispatch));
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
        let now = Instant::now();
        self.on_classified_transaction(
            dispatch,
            event,
            now,
            now,
            now,
            InlineTransactionDispatchSource::CompletedDatasetFallback,
            1,
            0,
        );
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
                        leader: newest_entry.leader.to_solana(),
                    });
                    true
                }
                Some(current)
                    if newest_entry.slot > current.slot
                        || (newest_entry.slot == current.slot
                            && newest_entry.leader.to_solana() != current.leader) =>
                {
                    current.slot = newest_entry.slot;
                    current.leader = newest_entry.leader.to_solana();
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
