use async_trait::async_trait;

use crate::framework::events::{
    ClusterTopologyEvent, DatasetEvent, LeaderScheduleEvent, ObservedRecentBlockhashEvent,
    RawPacketEvent, ShredEvent, TransactionEvent,
};

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

    /// Called for every UDP packet before shred parsing.
    async fn on_raw_packet(&self, _event: RawPacketEvent) {}

    /// Called for every packet that produced a valid parsed shred header.
    async fn on_shred(&self, _event: ShredEvent) {}

    /// Called when a contiguous shred dataset is reconstructed.
    async fn on_dataset(&self, _event: DatasetEvent) {}

    /// Called for each decoded transaction emitted from a dataset.
    async fn on_transaction(&self, _event: TransactionEvent) {}

    /// Called when a newer observed recent blockhash is detected.
    async fn on_recent_blockhash(&self, _event: ObservedRecentBlockhashEvent) {}

    /// Called on low-frequency cluster topology diffs/snapshots (gossip-bootstrap mode).
    async fn on_cluster_topology(&self, _event: ClusterTopologyEvent) {}

    /// Called on event-driven leader-schedule diffs/snapshots.
    async fn on_leader_schedule(&self, _event: LeaderScheduleEvent) {}
}
