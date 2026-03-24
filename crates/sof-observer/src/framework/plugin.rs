use async_trait::async_trait;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use std::{cell::RefCell, sync::Arc};
use thiserror::Error;

use crate::framework::events::{
    AccountTouchEvent, AccountTouchEventRef, ClusterTopologyEvent, DatasetEvent,
    LeaderScheduleEvent, ObservedRecentBlockhashEvent, RawPacketEvent, ReorgEvent, ShredEvent,
    SlotStatusEvent, TransactionBatchEvent, TransactionEvent, TransactionEventRef,
    TransactionViewBatchEvent,
};

#[derive(Clone, Copy, Eq, PartialEq)]
struct CachedTransactionEventKey {
    slot: u64,
    signature: Option<Signature>,
    tx_ptr: *const solana_transaction::versioned::VersionedTransaction,
    kind: crate::event::TxKind,
}

struct CachedTransactionEvent {
    key: CachedTransactionEventKey,
    event: TransactionEvent,
}

thread_local! {
    static CACHED_TRANSACTION_EVENT: RefCell<Option<CachedTransactionEvent>> = const { RefCell::new(None) };
}

fn cached_transaction_event_key(event: TransactionEventRef<'_>) -> CachedTransactionEventKey {
    CachedTransactionEventKey {
        slot: event.slot,
        signature: event.signature,
        tx_ptr: event.tx as *const _,
        kind: event.kind,
    }
}

fn with_cached_transaction_event<R>(
    event: TransactionEventRef<'_>,
    f: impl FnOnce(&TransactionEvent) -> R,
) -> R {
    let key = cached_transaction_event_key(event);
    CACHED_TRANSACTION_EVENT.with(|cached| {
        let mut cached = cached.borrow_mut();
        if !cached
            .as_ref()
            .is_some_and(|cached_event| cached_event.key == key)
        {
            *cached = Some(CachedTransactionEvent {
                key,
                event: event.to_owned(),
            });
        }
        let owned = &cached
            .as_ref()
            .expect("cached transaction event just inserted")
            .event;
        f(owned)
    })
}

pub(crate) fn clone_cached_transaction_event(
    event: TransactionEventRef<'_>,
) -> Option<TransactionEvent> {
    let key = cached_transaction_event_key(event);
    CACHED_TRANSACTION_EVENT.with(|cached| {
        cached
            .borrow()
            .as_ref()
            .and_then(|cached_event| (cached_event.key == key).then(|| cached_event.event.clone()))
    })
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
/// Priority class for accepted transaction callbacks.
pub enum TransactionInterest {
    /// Ignore the transaction entirely.
    Ignore,
    /// Lower-priority transaction visibility that may be isolated from HFT-critical traffic.
    Background,
    /// HFT-critical transaction visibility that should stay on the fast lane.
    Critical,
}

#[derive(Debug, Clone, Eq, PartialEq)]
/// Compiled transaction classifier for common signature/account-key matching cases.
///
/// This lets SOF classify transactions on the hot path without calling the
/// plugin's custom matcher for simple "does this transaction mention these
/// keys/signature?" cases.
///
/// Matching is performed against static message account keys and referenced
/// address-table account keys. Loaded lookup addresses are not available on
/// SOF's external shred path, so they are intentionally not part of this
/// matcher.
///
/// # Examples
///
/// ```rust
/// use sof::framework::{TransactionInterest, TransactionPrefilter};
/// use solana_pubkey::Pubkey;
///
/// let pool = Pubkey::new_unique();
/// let program = Pubkey::new_unique();
///
/// let filter = TransactionPrefilter::new(TransactionInterest::Critical)
///     .with_account_required([pool, program]);
///
/// assert_eq!(filter.matched_interest(), TransactionInterest::Critical);
/// ```
pub struct TransactionPrefilter {
    matched_interest: TransactionInterest,
    signature: Option<Signature>,
    account_include: Arc<[Pubkey]>,
    account_exclude: Arc<[Pubkey]>,
    account_required: Arc<[Pubkey]>,
}

impl TransactionPrefilter {
    /// Creates an empty prefilter that returns the provided interest on match.
    #[must_use]
    pub fn new(matched_interest: TransactionInterest) -> Self {
        Self {
            matched_interest,
            signature: None,
            account_include: Arc::new([]),
            account_exclude: Arc::new([]),
            account_required: Arc::new([]),
        }
    }

    /// Returns the interest emitted when this filter matches.
    #[must_use]
    pub const fn matched_interest(&self) -> TransactionInterest {
        self.matched_interest
    }

    /// Requires one exact transaction signature.
    #[must_use]
    pub fn with_signature(mut self, signature: Signature) -> Self {
        self.signature = Some(signature);
        self
    }

    /// Requires at least one listed account key to be present.
    #[must_use]
    pub fn with_account_include<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = Pubkey>,
    {
        self.account_include = Arc::from(keys.into_iter().collect::<Vec<_>>());
        self
    }

    /// Rejects transactions that mention any listed account key.
    #[must_use]
    pub fn with_account_exclude<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = Pubkey>,
    {
        self.account_exclude = Arc::from(keys.into_iter().collect::<Vec<_>>());
        self
    }

    /// Requires all listed account keys to be present.
    #[must_use]
    pub fn with_account_required<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = Pubkey>,
    {
        self.account_required = Arc::from(keys.into_iter().collect::<Vec<_>>());
        self
    }

    /// Returns the matched interest or [`TransactionInterest::Ignore`].
    #[must_use]
    pub fn classify_ref(&self, event: TransactionEventRef<'_>) -> TransactionInterest {
        if let Some(signature) = self.signature
            && event.signature != Some(signature)
        {
            return TransactionInterest::Ignore;
        }
        if !self.account_include.is_empty()
            && !self
                .account_include
                .iter()
                .copied()
                .any(|key| transaction_mentions_account_key(event.tx, key))
        {
            return TransactionInterest::Ignore;
        }
        if self
            .account_exclude
            .iter()
            .copied()
            .any(|key| transaction_mentions_account_key(event.tx, key))
        {
            return TransactionInterest::Ignore;
        }
        if !self
            .account_required
            .iter()
            .copied()
            .all(|key| transaction_mentions_account_key(event.tx, key))
        {
            return TransactionInterest::Ignore;
        }
        self.matched_interest
    }
}

fn transaction_mentions_account_key(
    tx: &solana_transaction::versioned::VersionedTransaction,
    key: Pubkey,
) -> bool {
    tx.message.static_account_keys().contains(&key)
        || tx
            .message
            .address_table_lookups()
            .is_some_and(|lookups| lookups.iter().any(|lookup| lookup.account_key == key))
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
/// Delivery path for transaction callbacks.
///
/// # Examples
///
/// ```rust
/// use sof::framework::{PluginConfig, TransactionDispatchMode};
///
/// let config = PluginConfig::new().with_transaction_mode(TransactionDispatchMode::Inline);
///
/// assert!(config.transaction);
/// assert_eq!(config.transaction_dispatch_mode, TransactionDispatchMode::Inline);
/// ```
pub enum TransactionDispatchMode {
    /// Use the standard dataset-worker transaction path.
    #[default]
    Standard,
    /// Dispatch inline from the earliest reconstructable contiguous transaction path.
    ///
    /// SOF emits `on_transaction` as soon as one full serialized transaction is
    /// reconstructable on the inline path and falls back to the completed-dataset
    /// boundary only when early reconstruction is not yet possible.
    Inline,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
/// Static hook subscriptions requested by one plugin during host construction.
///
/// # Examples
///
/// ```rust
/// use sof::framework::{PluginConfig, TransactionDispatchMode};
///
/// let config = PluginConfig::new()
///     .with_transaction_mode(TransactionDispatchMode::Inline)
///     .with_recent_blockhash()
///     .with_leader_schedule();
///
/// assert!(config.transaction);
/// assert!(config.recent_blockhash);
/// assert!(config.leader_schedule);
/// ```
pub struct PluginConfig {
    /// Enables `on_raw_packet`.
    pub raw_packet: bool,
    /// Enables `on_shred`.
    pub shred: bool,
    /// Enables `on_dataset`.
    pub dataset: bool,
    /// Enables `on_transaction`.
    pub transaction: bool,
    /// Requested delivery path for `on_transaction`.
    pub transaction_dispatch_mode: TransactionDispatchMode,
    /// Enables `on_transaction_batch`.
    pub transaction_batch: bool,
    /// Requested delivery path for `on_transaction_batch`.
    pub transaction_batch_dispatch_mode: TransactionDispatchMode,
    /// Enables `on_transaction_view_batch`.
    pub transaction_view_batch: bool,
    /// Requested delivery path for `on_transaction_view_batch`.
    pub transaction_view_batch_dispatch_mode: TransactionDispatchMode,
    /// Enables `on_account_touch`.
    pub account_touch: bool,
    /// Enables `on_slot_status`.
    pub slot_status: bool,
    /// Enables `on_reorg`.
    pub reorg: bool,
    /// Enables `on_recent_blockhash`.
    pub recent_blockhash: bool,
    /// Enables `on_cluster_topology`.
    pub cluster_topology: bool,
    /// Enables `on_leader_schedule`.
    pub leader_schedule: bool,
}

impl PluginConfig {
    /// Creates an empty plugin config with all hooks disabled.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sof::framework::PluginConfig;
    ///
    /// let config = PluginConfig::new();
    /// assert!(!config.transaction);
    /// assert!(!config.dataset);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables `on_raw_packet`.
    #[must_use]
    pub const fn with_raw_packet(mut self) -> Self {
        self.raw_packet = true;
        self
    }

    /// Enables `on_shred`.
    #[must_use]
    pub const fn with_shred(mut self) -> Self {
        self.shred = true;
        self
    }

    /// Enables `on_dataset`.
    #[must_use]
    pub const fn with_dataset(mut self) -> Self {
        self.dataset = true;
        self
    }

    /// Enables `on_transaction`.
    #[must_use]
    pub const fn with_transaction(mut self) -> Self {
        self.transaction = true;
        self
    }

    /// Enables `on_transaction` and requests inline low-jitter transaction delivery.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sof::framework::{PluginConfig, TransactionDispatchMode};
    ///
    /// let config = PluginConfig::new().with_inline_transaction();
    ///
    /// assert!(config.transaction);
    /// assert_eq!(config.transaction_dispatch_mode, TransactionDispatchMode::Inline);
    /// ```
    #[must_use]
    pub const fn with_inline_transaction(mut self) -> Self {
        self.transaction = true;
        self.transaction_dispatch_mode = TransactionDispatchMode::Inline;
        self
    }

    /// Enables `on_transaction` and chooses one explicit transaction delivery mode.
    #[must_use]
    pub const fn with_transaction_mode(mut self, mode: TransactionDispatchMode) -> Self {
        self.transaction = true;
        self.transaction_dispatch_mode = mode;
        self
    }

    /// Enables `on_transaction_batch`.
    #[must_use]
    pub const fn with_transaction_batch(mut self) -> Self {
        self.transaction_batch = true;
        self
    }

    /// Enables `on_transaction_batch` and requests inline completed-dataset delivery.
    #[must_use]
    pub const fn with_inline_transaction_batch(mut self) -> Self {
        self.transaction_batch = true;
        self.transaction_batch_dispatch_mode = TransactionDispatchMode::Inline;
        self
    }

    /// Enables `on_transaction_batch` and chooses one explicit batch delivery mode.
    #[must_use]
    pub const fn with_transaction_batch_mode(mut self, mode: TransactionDispatchMode) -> Self {
        self.transaction_batch = true;
        self.transaction_batch_dispatch_mode = mode;
        self
    }

    /// Enables `on_transaction_view_batch`.
    #[must_use]
    pub const fn with_transaction_view_batch(mut self) -> Self {
        self.transaction_view_batch = true;
        self
    }

    /// Enables `on_transaction_view_batch` and requests inline completed-dataset delivery.
    #[must_use]
    pub const fn with_inline_transaction_view_batch(mut self) -> Self {
        self.transaction_view_batch = true;
        self.transaction_view_batch_dispatch_mode = TransactionDispatchMode::Inline;
        self
    }

    /// Enables `on_transaction_view_batch` and chooses one explicit view-batch delivery mode.
    #[must_use]
    pub const fn with_transaction_view_batch_mode(mut self, mode: TransactionDispatchMode) -> Self {
        self.transaction_view_batch = true;
        self.transaction_view_batch_dispatch_mode = mode;
        self
    }

    /// Enables `on_account_touch`.
    #[must_use]
    pub const fn with_account_touch(mut self) -> Self {
        self.account_touch = true;
        self
    }

    /// Enables `on_slot_status`.
    #[must_use]
    pub const fn with_slot_status(mut self) -> Self {
        self.slot_status = true;
        self
    }

    /// Enables `on_reorg`.
    #[must_use]
    pub const fn with_reorg(mut self) -> Self {
        self.reorg = true;
        self
    }

    /// Enables `on_recent_blockhash`.
    #[must_use]
    pub const fn with_recent_blockhash(mut self) -> Self {
        self.recent_blockhash = true;
        self
    }

    /// Enables `on_cluster_topology`.
    #[must_use]
    pub const fn with_cluster_topology(mut self) -> Self {
        self.cluster_topology = true;
        self
    }

    /// Enables `on_leader_schedule`.
    #[must_use]
    pub const fn with_leader_schedule(mut self) -> Self {
        self.leader_schedule = true;
        self
    }
}

#[derive(Debug, Clone)]
/// Context passed to plugin lifecycle hooks.
pub struct PluginContext {
    /// Plugin identifier.
    pub plugin_name: &'static str,
}

#[derive(Debug, Clone, Error, Eq, PartialEq)]
#[error("{reason}")]
/// Plugin setup failure reported by one plugin implementation.
pub struct PluginSetupError {
    /// Human-readable setup failure reason.
    reason: String,
}

impl PluginSetupError {
    /// Creates a setup error with a human-readable reason.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// Extension point for SOF runtime event hooks.
///
/// Plugins are executed asynchronously by the plugin host worker, decoupled from ingest hot paths.
/// Keep callbacks lightweight and use bounded work queues for any expensive downstream processing.
///
/// # Examples
///
/// ```rust
/// use async_trait::async_trait;
/// use sof::framework::{ObserverPlugin, PluginConfig, TransactionEvent};
///
/// struct CriticalFlowPlugin;
///
/// #[async_trait]
/// impl ObserverPlugin for CriticalFlowPlugin {
///     fn config(&self) -> PluginConfig {
///         PluginConfig::new().with_inline_transaction()
///     }
///
///     async fn on_transaction(&self, event: &TransactionEvent) {
///         let _signature = event.signature;
///     }
/// }
/// ```
#[async_trait]
pub trait ObserverPlugin: Send + Sync + 'static {
    /// Stable plugin identifier used in startup logs and diagnostics.
    ///
    /// By default this uses [`core::any::type_name`] so simple plugins can skip boilerplate.
    fn name(&self) -> &'static str {
        core::any::type_name::<Self>()
    }

    /// Returns static hook subscriptions requested by this plugin.
    ///
    /// The host evaluates this once during construction and precomputes dispatch
    /// targets so the runtime does not need per-hook subscription lookups later.
    fn config(&self) -> PluginConfig {
        PluginConfig::default()
    }

    /// Called once before the runtime enters its main event loop.
    async fn setup(&self, _ctx: PluginContext) -> Result<(), PluginSetupError> {
        Ok(())
    }

    /// Called for every UDP packet before shred parsing.
    async fn on_raw_packet(&self, _event: RawPacketEvent) {}

    /// Called for every packet that produced a valid parsed shred header.
    async fn on_shred(&self, _event: ShredEvent) {}

    /// Called when a contiguous shred dataset is reconstructed.
    async fn on_dataset(&self, _event: DatasetEvent) {}

    /// Returns true when this plugin wants a specific decoded transaction callback.
    ///
    /// This synchronous prefilter runs on the hot path before queueing the
    /// transaction hook. Use it to reject irrelevant transactions cheaply.
    fn accepts_transaction(&self, _event: &TransactionEvent) -> bool {
        true
    }

    /// Borrowed transaction prefilter used on the dataset hot path.
    ///
    /// Override this to avoid constructing an owned [`TransactionEvent`] for
    /// transactions that will be ignored anyway.
    ///
    /// Plugins that only need borrowed fields should prefer this hook over
    /// [`Self::accepts_transaction`].
    fn accepts_transaction_ref(&self, event: TransactionEventRef<'_>) -> bool {
        with_cached_transaction_event(event, |owned| self.accepts_transaction(owned))
    }

    /// Returns transaction-interest priority for one decoded transaction callback.
    ///
    /// The default preserves the historical API: accepted transactions are treated
    /// as critical and rejected transactions are ignored.
    fn transaction_interest(&self, event: &TransactionEvent) -> TransactionInterest {
        if self.accepts_transaction(event) {
            TransactionInterest::Critical
        } else {
            TransactionInterest::Ignore
        }
    }

    /// Borrowed transaction-interest classifier used on the dataset hot path.
    ///
    /// Override this when classification can run directly on borrowed message
    /// data without first allocating an owned [`TransactionEvent`].
    ///
    /// Priority-sensitive plugins should implement this hook directly so the
    /// dataset hot path can classify traffic without allocating.
    fn transaction_interest_ref(&self, event: TransactionEventRef<'_>) -> TransactionInterest {
        with_cached_transaction_event(event, |owned| self.transaction_interest(owned))
    }

    /// Returns one compiled hot-path transaction matcher when the plugin uses
    /// only common signature/account-key classification rules.
    ///
    /// When this is present, SOF can classify transactions without calling the
    /// plugin's custom borrowed classifier on the hot path.
    fn transaction_prefilter(&self) -> Option<&TransactionPrefilter> {
        None
    }

    /// Called for each decoded transaction emitted from a completed contiguous data range.
    ///
    /// By default this happens on the dataset-worker path. Plugins that request inline
    /// transaction delivery receive this hook from the completed-dataset boundary even when
    /// other dataset consumers still continue on the dataset-worker path.
    async fn on_transaction(&self, _event: &TransactionEvent) {}

    /// Called once per completed dataset with all decoded transactions in dataset order.
    ///
    /// This hook is non-speculative. SOF invokes it only after a contiguous dataset is fully
    /// reconstructed and decoded, and before the runtime walks each transaction through the
    /// per-transaction plugin path.
    async fn on_transaction_batch(&self, _event: &TransactionBatchEvent) {}

    /// Called once per completed dataset with authoritative serialized transaction views.
    ///
    /// This hook is non-speculative. SOF invokes it after the completed dataset payload is
    /// assembled and the transaction byte ranges are validated, but before full owned
    /// `VersionedTransaction` materialization. It is intended for low-latency completed-dataset
    /// consumers that can work directly on sanitized transaction views.
    async fn on_transaction_view_batch(&self, _event: &TransactionViewBatchEvent) {}

    /// Called for each accepted decoded transaction with the already-computed routing lane.
    ///
    /// Implement this when the plugin wants to avoid recomputing the same synchronous
    /// routing/classification work inside [`Self::on_transaction`].
    async fn on_transaction_with_interest(
        &self,
        event: &TransactionEvent,
        _interest: TransactionInterest,
    ) {
        self.on_transaction(event).await;
    }

    /// Borrowed account-touch prefilter used on the dataset hot path.
    ///
    /// Override this to reject irrelevant account-touch callbacks before the
    /// runtime allocates owned account-key vectors.
    fn accepts_account_touch_ref(&self, _event: AccountTouchEventRef<'_>) -> bool {
        true
    }

    /// Called for each decoded transaction's static touched-account set.
    async fn on_account_touch(&self, _event: &AccountTouchEvent) {}

    /// Called when local slot status transitions (processed/confirmed/finalized/orphaned).
    async fn on_slot_status(&self, _event: SlotStatusEvent) {}

    /// Called when local canonical tip switches to a different branch.
    async fn on_reorg(&self, _event: ReorgEvent) {}

    /// Called when a newer observed recent blockhash is detected.
    async fn on_recent_blockhash(&self, _event: ObservedRecentBlockhashEvent) {}

    /// Called on low-frequency cluster topology diffs/snapshots (gossip-bootstrap mode).
    async fn on_cluster_topology(&self, _event: ClusterTopologyEvent) {}

    /// Called on event-driven leader-schedule diffs/snapshots.
    async fn on_leader_schedule(&self, _event: LeaderScheduleEvent) {}

    /// Called during runtime shutdown after ingest has stopped.
    async fn shutdown(&self, _ctx: PluginContext) {}
}
