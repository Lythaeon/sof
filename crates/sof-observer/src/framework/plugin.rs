use async_trait::async_trait;
use thiserror::Error;

use crate::framework::events::{
    AccountTouchEvent, AccountTouchEventRef, ClusterTopologyEvent, DatasetEvent,
    LeaderScheduleEvent, ObservedRecentBlockhashEvent, RawPacketEvent, ReorgEvent, ShredEvent,
    SlotStatusEvent, TransactionEvent, TransactionEventRef,
};

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

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
/// Static hook subscriptions requested by one plugin during host construction.
pub struct PluginConfig {
    /// Enables `on_raw_packet`.
    pub raw_packet: bool,
    /// Enables `on_shred`.
    pub shred: bool,
    /// Enables `on_dataset`.
    pub dataset: bool,
    /// Enables `on_transaction`.
    pub transaction: bool,
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
        self.accepts_transaction(&event.to_owned())
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
        self.transaction_interest(&event.to_owned())
    }

    /// Called for each decoded transaction emitted from a dataset.
    async fn on_transaction(&self, _event: &TransactionEvent) {}

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
