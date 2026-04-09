#![allow(clippy::missing_docs_in_private_items)]

use agave_transaction_view::{
    transaction_data::TransactionData, transaction_view::SanitizedTransactionView,
};
use async_trait::async_trait;
use sof_types::{PubkeyBytes, SignatureBytes};
use std::{cell::RefCell, sync::Arc};
use thiserror::Error;

use crate::{
    event::{TxCommitmentStatus, TxKind},
    framework::events::{
        AccountTouchEvent, AccountTouchEventRef, AccountUpdateEvent, BlockMetaEvent,
        ClusterTopologyEvent, DatasetEvent, LeaderScheduleEvent, ObservedRecentBlockhashEvent,
        RawPacketEvent, ReorgEvent, ShredEvent, SlotStatusEvent, TransactionBatchEvent,
        TransactionEvent, TransactionEventRef, TransactionLogEvent, TransactionStatusEvent,
        TransactionViewBatchEvent,
    },
};

#[derive(Clone, Copy, Eq, PartialEq)]
struct CachedTransactionEventKey {
    slot: u64,
    signature: Option<SignatureBytes>,
    tx_ptr: *const solana_transaction::versioned::VersionedTransaction,
    kind: TxKind,
}

struct CachedTransactionEvent {
    key: CachedTransactionEventKey,
    event: TransactionEvent,
}

thread_local! {
    static CACHED_TRANSACTION_EVENT: RefCell<Option<CachedTransactionEvent>> = const { RefCell::new(None) };
}

const fn cached_transaction_event_key(
    event: &TransactionEventRef<'_>,
) -> CachedTransactionEventKey {
    CachedTransactionEventKey {
        slot: event.slot,
        signature: event.signature,
        tx_ptr: event.tx as *const _,
        kind: event.kind,
    }
}

fn with_cached_transaction_event<R>(
    event: &TransactionEventRef<'_>,
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
        if let Some(cached_event) = cached.as_ref() {
            f(&cached_event.event)
        } else {
            let owned = event.to_owned();
            f(&owned)
        }
    })
}

pub(crate) fn clone_cached_transaction_event(
    event: &TransactionEventRef<'_>,
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
/// On the inline path, this is also the preferred way to reject irrelevant
/// traffic. When every in-scope inline transaction plugin uses
/// [`ObserverPlugin::transaction_prefilter`] and all matching prefilters return
/// [`TransactionInterest::Ignore`], SOF can skip full owned
/// `VersionedTransaction` decode for that transaction entirely.
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
/// use sof::PubkeyBytes;
///
/// let pool = PubkeyBytes::new([1_u8; 32]);
/// let program = PubkeyBytes::new([2_u8; 32]);
///
/// let filter = TransactionPrefilter::new(TransactionInterest::Critical)
///     .with_account_required([pool, program]);
///
/// assert_eq!(filter.matched_interest(), TransactionInterest::Critical);
/// ```
pub struct TransactionPrefilter {
    matched_interest: TransactionInterest,
    signature: Option<SignatureBytes>,
    signature_solana: Option<solana_signature::Signature>,
    account_include: CompiledAccountMatcher,
    account_exclude: CompiledAccountMatcher,
    account_required: CompiledAccountMatcher,
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum CompiledAccountMatcher {
    Empty,
    One(solana_pubkey::Pubkey),
    Two(solana_pubkey::Pubkey, solana_pubkey::Pubkey),
    Many(Arc<[solana_pubkey::Pubkey]>),
}

impl TransactionPrefilter {
    /// Creates an empty prefilter that returns the provided interest on match.
    #[must_use]
    pub const fn new(matched_interest: TransactionInterest) -> Self {
        Self {
            matched_interest,
            signature: None,
            signature_solana: None,
            account_include: CompiledAccountMatcher::Empty,
            account_exclude: CompiledAccountMatcher::Empty,
            account_required: CompiledAccountMatcher::Empty,
        }
    }

    /// Returns the interest emitted when this filter matches.
    #[must_use]
    pub const fn matched_interest(&self) -> TransactionInterest {
        self.matched_interest
    }

    /// Requires one exact transaction signature.
    #[must_use]
    pub fn with_signature<S>(mut self, signature: S) -> Self
    where
        S: Into<SignatureBytes>,
    {
        let signature = signature.into();
        self.signature = Some(signature);
        self.signature_solana = Some(signature.to_solana());
        self
    }

    /// Requires at least one listed account key to be present.
    #[must_use]
    pub fn with_account_include<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<PubkeyBytes>,
    {
        self.account_include = compile_prefilter_keys(keys);
        self
    }

    /// Rejects transactions that mention any listed account key.
    #[must_use]
    pub fn with_account_exclude<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<PubkeyBytes>,
    {
        self.account_exclude = compile_prefilter_keys(keys);
        self
    }

    /// Requires all listed account keys to be present.
    #[must_use]
    pub fn with_account_required<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<PubkeyBytes>,
    {
        self.account_required = compile_prefilter_keys(keys);
        self
    }

    /// Returns the matched interest or [`TransactionInterest::Ignore`].
    #[must_use]
    pub fn classify_ref(&self, event: &TransactionEventRef<'_>) -> TransactionInterest {
        if let Some(signature) = self.signature
            && event.signature != Some(signature)
        {
            return TransactionInterest::Ignore;
        }
        if !matches!(self.account_include, CompiledAccountMatcher::Empty)
            && !transaction_matches_any_keys(event.tx, &self.account_include)
        {
            return TransactionInterest::Ignore;
        }
        if transaction_matches_any_keys(event.tx, &self.account_exclude) {
            return TransactionInterest::Ignore;
        }
        if !transaction_matches_all_keys(event.tx, &self.account_required) {
            return TransactionInterest::Ignore;
        }
        self.matched_interest
    }

    /// Returns the matched interest or [`TransactionInterest::Ignore`] from one transaction view.
    #[must_use]
    pub(crate) fn classify_view_ref<D: TransactionData>(
        &self,
        view: &SanitizedTransactionView<D>,
    ) -> TransactionInterest {
        if let Some(signature) = self.signature_solana
            && view.signatures().first().copied() != Some(signature)
        {
            return TransactionInterest::Ignore;
        }
        if !matches!(self.account_include, CompiledAccountMatcher::Empty)
            && !transaction_view_matches_any_keys(view, &self.account_include)
        {
            return TransactionInterest::Ignore;
        }
        if transaction_view_matches_any_keys(view, &self.account_exclude) {
            return TransactionInterest::Ignore;
        }
        if !transaction_view_matches_all_keys(view, &self.account_required) {
            return TransactionInterest::Ignore;
        }
        self.matched_interest
    }
}

fn compile_prefilter_keys<I>(keys: I) -> CompiledAccountMatcher
where
    I: IntoIterator,
    I::Item: Into<PubkeyBytes>,
{
    let keys = keys
        .into_iter()
        .map(Into::into)
        .map(PubkeyBytes::to_solana)
        .collect::<Vec<_>>();
    match keys.as_slice() {
        [] => CompiledAccountMatcher::Empty,
        [key] => CompiledAccountMatcher::One(*key),
        [first, second] => CompiledAccountMatcher::Two(*first, *second),
        _ => CompiledAccountMatcher::Many(Arc::from(keys)),
    }
}

fn transaction_mentions_account_key(
    tx: &solana_transaction::versioned::VersionedTransaction,
    key: &solana_pubkey::Pubkey,
) -> bool {
    tx.message.static_account_keys().contains(key)
        || tx
            .message
            .address_table_lookups()
            .is_some_and(|lookups| lookups.iter().any(|lookup| lookup.account_key == *key))
}

fn transaction_matches_any_keys(
    tx: &solana_transaction::versioned::VersionedTransaction,
    matcher: &CompiledAccountMatcher,
) -> bool {
    match matcher {
        CompiledAccountMatcher::Empty => false,
        CompiledAccountMatcher::One(key) => transaction_mentions_account_key(tx, key),
        CompiledAccountMatcher::Two(first, second) => {
            transaction_mentions_account_key(tx, first)
                || transaction_mentions_account_key(tx, second)
        }
        CompiledAccountMatcher::Many(keys) => keys
            .iter()
            .any(|key| transaction_mentions_account_key(tx, key)),
    }
}

fn transaction_matches_all_keys(
    tx: &solana_transaction::versioned::VersionedTransaction,
    matcher: &CompiledAccountMatcher,
) -> bool {
    match matcher {
        CompiledAccountMatcher::Empty => true,
        CompiledAccountMatcher::One(key) => transaction_mentions_account_key(tx, key),
        CompiledAccountMatcher::Two(first, second) => {
            transaction_mentions_account_key(tx, first)
                && transaction_mentions_account_key(tx, second)
        }
        CompiledAccountMatcher::Many(keys) => keys
            .iter()
            .all(|key| transaction_mentions_account_key(tx, key)),
    }
}

fn transaction_view_mentions_account_key<D: TransactionData>(
    view: &SanitizedTransactionView<D>,
    key: &solana_pubkey::Pubkey,
) -> bool {
    view.static_account_keys().contains(key)
        || view
            .address_table_lookup_iter()
            .any(|lookup| lookup.account_key == key)
}

fn transaction_view_matches_any_keys<D: TransactionData>(
    view: &SanitizedTransactionView<D>,
    matcher: &CompiledAccountMatcher,
) -> bool {
    match matcher {
        CompiledAccountMatcher::Empty => false,
        CompiledAccountMatcher::One(key) => transaction_view_mentions_account_key(view, key),
        CompiledAccountMatcher::Two(first, second) => {
            transaction_view_mentions_account_key(view, first)
                || transaction_view_mentions_account_key(view, second)
        }
        CompiledAccountMatcher::Many(keys) => keys
            .iter()
            .any(|key| transaction_view_mentions_account_key(view, key)),
    }
}

fn transaction_view_matches_all_keys<D: TransactionData>(
    view: &SanitizedTransactionView<D>,
    matcher: &CompiledAccountMatcher,
) -> bool {
    match matcher {
        CompiledAccountMatcher::Empty => true,
        CompiledAccountMatcher::One(key) => transaction_view_mentions_account_key(view, key),
        CompiledAccountMatcher::Two(first, second) => {
            transaction_view_mentions_account_key(view, first)
                && transaction_view_mentions_account_key(view, second)
        }
        CompiledAccountMatcher::Many(keys) => keys
            .iter()
            .all(|key| transaction_view_mentions_account_key(view, key)),
    }
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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
/// Commitment selector applied to transaction-family plugin hooks.
pub enum TransactionCommitmentSelector {
    /// Deliver transactions at or above one minimum commitment.
    AtLeast(TxCommitmentStatus),
    /// Deliver transactions only at one exact commitment.
    Only(TxCommitmentStatus),
}

impl Default for TransactionCommitmentSelector {
    fn default() -> Self {
        Self::AtLeast(TxCommitmentStatus::Processed)
    }
}

impl TransactionCommitmentSelector {
    /// Returns true when one transaction event should be delivered under this selector.
    #[must_use]
    pub fn matches(self, commitment_status: TxCommitmentStatus) -> bool {
        match self {
            Self::AtLeast(minimum) => commitment_status.satisfies_minimum(minimum),
            Self::Only(expected) => commitment_status == expected,
        }
    }

    /// Returns the lowest commitment this selector can ever accept.
    #[must_use]
    pub const fn minimum_required(self) -> TxCommitmentStatus {
        match self {
            Self::AtLeast(minimum) | Self::Only(minimum) => minimum,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
/// Static hook subscriptions requested by one plugin during host construction.
///
/// # Examples
///
/// ```rust
/// use sof::framework::{PluginConfig, TransactionDispatchMode, TxCommitmentStatus};
///
/// let config = PluginConfig::new()
///     .with_transaction_mode(TransactionDispatchMode::Inline)
///     .at_commitment(TxCommitmentStatus::Confirmed)
///     .with_recent_blockhash()
///     .with_leader_schedule();
///
/// assert!(config.transaction);
/// assert_eq!(
///     config.transaction_commitment,
///     sof::framework::TransactionCommitmentSelector::AtLeast(TxCommitmentStatus::Confirmed)
/// );
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
    /// Enables `on_transaction_log`.
    pub transaction_log: bool,
    /// Enables `on_transaction_status`.
    pub transaction_status: bool,
    /// Commitment selector applied to transaction-family hooks.
    pub transaction_commitment: TransactionCommitmentSelector,
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
    /// Enables `on_account_update`.
    pub account_update: bool,
    /// Enables `on_block_meta`.
    pub block_meta: bool,
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

    /// Enables `on_transaction_status`.
    #[must_use]
    pub const fn with_transaction_status(mut self) -> Self {
        self.transaction_status = true;
        self
    }

    /// Sets the minimum commitment required before transaction-family hooks are delivered.
    ///
    /// This applies uniformly to:
    /// - `on_transaction`
    /// - `on_transaction_log`
    /// - `on_transaction_status`
    /// - `on_transaction_batch`
    /// - `on_transaction_view_batch`
    ///
    /// If you do not call [`Self::at_commitment`] or
    /// [`Self::only_at_commitment`], the default is
    /// `AtLeast(TxCommitmentStatus::Processed)`.
    #[must_use]
    pub const fn at_commitment(mut self, commitment: TxCommitmentStatus) -> Self {
        self.transaction_commitment = TransactionCommitmentSelector::AtLeast(commitment);
        self
    }

    /// Delivers transaction-family hooks only at one exact commitment.
    #[must_use]
    pub const fn only_at_commitment(mut self, commitment: TxCommitmentStatus) -> Self {
        self.transaction_commitment = TransactionCommitmentSelector::Only(commitment);
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

    /// Enables `on_account_update`.
    #[must_use]
    pub const fn with_account_update(mut self) -> Self {
        self.account_update = true;
        self
    }

    /// Enables `on_block_meta`.
    #[must_use]
    pub const fn with_block_meta(mut self) -> Self {
        self.block_meta = true;
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
    fn accepts_transaction_ref(&self, event: &TransactionEventRef<'_>) -> bool {
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
    fn transaction_interest_ref(&self, event: &TransactionEventRef<'_>) -> TransactionInterest {
        with_cached_transaction_event(event, |owned| self.transaction_interest(owned))
    }

    /// Returns one compiled hot-path transaction matcher when the plugin uses
    /// only common signature/account-key classification rules.
    ///
    /// When this is present, SOF can classify transactions without calling the
    /// plugin's custom borrowed classifier on the hot path.
    ///
    /// Prefer this over [`Self::transaction_interest_ref`] when the plugin only
    /// needs exact signature matching or static/account-lookup key presence
    /// checks. On the inline path, a prefilter-backed miss can let SOF skip full
    /// owned transaction decode for that tx.
    fn transaction_prefilter(&self) -> Option<&TransactionPrefilter> {
        None
    }

    /// Called for each decoded transaction emitted from a completed contiguous data range.
    ///
    /// By default this happens on the dataset-worker path. Plugins that request inline
    /// transaction delivery receive this hook from the completed-dataset boundary even when
    /// other dataset consumers still continue on the dataset-worker path.
    async fn on_transaction(&self, _event: &TransactionEvent) {}

    /// Called for one websocket/provider log notification that does not carry a
    /// full decoded transaction object.
    async fn on_transaction_log(&self, _event: &TransactionLogEvent) {}

    /// Returns true when this plugin wants a specific transaction-log callback.
    fn accepts_transaction_log(&self, _event: &TransactionLogEvent) -> bool {
        true
    }

    /// Called for one provider transaction-status update that does not carry a
    /// full decoded transaction object.
    async fn on_transaction_status(&self, _event: &TransactionStatusEvent) {}

    /// Returns true when this plugin wants a specific transaction-status callback.
    fn accepts_transaction_status(&self, _event: &TransactionStatusEvent) -> bool {
        true
    }

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
    fn accepts_account_touch_ref(&self, _event: &AccountTouchEventRef<'_>) -> bool {
        true
    }

    /// Called for each decoded transaction's static touched-account set.
    async fn on_account_touch(&self, _event: &AccountTouchEvent) {}

    /// Called for one upstream account update.
    async fn on_account_update(&self, _event: &AccountUpdateEvent) {}

    /// Returns true when this plugin wants a specific block-meta callback.
    fn accepts_block_meta(&self, _event: &BlockMetaEvent) -> bool {
        true
    }

    /// Called for one upstream block-meta update.
    async fn on_block_meta(&self, _event: &BlockMetaEvent) {}

    /// Returns true when this plugin wants a specific account-update callback.
    fn accepts_account_update(&self, _event: &AccountUpdateEvent) -> bool {
        true
    }

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
