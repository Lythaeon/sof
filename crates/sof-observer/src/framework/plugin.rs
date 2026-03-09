use async_trait::async_trait;

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

    /// Returns true when this plugin wants raw-packet callbacks.
    fn wants_raw_packet(&self) -> bool {
        false
    }

    /// Called for every UDP packet before shred parsing.
    async fn on_raw_packet(&self, _event: RawPacketEvent) {}

    /// Returns true when this plugin wants parsed shred callbacks.
    fn wants_shred(&self) -> bool {
        false
    }

    /// Called for every packet that produced a valid parsed shred header.
    async fn on_shred(&self, _event: ShredEvent) {}

    /// Returns true when this plugin wants reconstructed dataset callbacks.
    fn wants_dataset(&self) -> bool {
        false
    }

    /// Called when a contiguous shred dataset is reconstructed.
    async fn on_dataset(&self, _event: DatasetEvent) {}

    /// Returns true when this plugin wants decoded transaction callbacks.
    fn wants_transaction(&self) -> bool {
        false
    }

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

    /// Returns true when this plugin wants account-touch callbacks.
    fn wants_account_touch(&self) -> bool {
        false
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

    /// Returns true when this plugin wants slot-status callbacks.
    fn wants_slot_status(&self) -> bool {
        false
    }

    /// Called when local slot status transitions (processed/confirmed/finalized/orphaned).
    async fn on_slot_status(&self, _event: SlotStatusEvent) {}

    /// Returns true when this plugin wants reorg callbacks.
    fn wants_reorg(&self) -> bool {
        false
    }

    /// Called when local canonical tip switches to a different branch.
    async fn on_reorg(&self, _event: ReorgEvent) {}

    /// Returns true when this plugin wants recent-blockhash callbacks.
    fn wants_recent_blockhash(&self) -> bool {
        false
    }

    /// Called when a newer observed recent blockhash is detected.
    async fn on_recent_blockhash(&self, _event: ObservedRecentBlockhashEvent) {}

    /// Returns true when this plugin wants cluster-topology callbacks.
    fn wants_cluster_topology(&self) -> bool {
        false
    }

    /// Called on low-frequency cluster topology diffs/snapshots (gossip-bootstrap mode).
    async fn on_cluster_topology(&self, _event: ClusterTopologyEvent) {}

    /// Returns true when this plugin wants leader-schedule callbacks.
    fn wants_leader_schedule(&self) -> bool {
        false
    }

    /// Called on event-driven leader-schedule diffs/snapshots.
    async fn on_leader_schedule(&self, _event: LeaderScheduleEvent) {}
}
