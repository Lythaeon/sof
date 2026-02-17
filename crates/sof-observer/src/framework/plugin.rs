use crate::framework::events::{DatasetEvent, RawPacketEvent, ShredEvent, TransactionEvent};

/// Extension point for SOF runtime event hooks.
///
/// A plugin is executed synchronously on runtime hot paths. Keep callbacks non-blocking
/// and low-allocation. If expensive work is needed, enqueue to your own worker.
pub trait ObserverPlugin: Send + Sync + 'static {
    /// Stable plugin identifier used in startup logs and diagnostics.
    ///
    /// By default this uses [`core::any::type_name`] so simple plugins can skip boilerplate.
    fn name(&self) -> &'static str {
        core::any::type_name::<Self>()
    }

    /// Called for every UDP packet before shred parsing.
    fn on_raw_packet(&self, _event: RawPacketEvent<'_>) {}

    /// Called for every packet that produced a valid parsed shred header.
    fn on_shred(&self, _event: ShredEvent<'_>) {}

    /// Called when a contiguous shred dataset is reconstructed.
    fn on_dataset(&self, _event: DatasetEvent) {}

    /// Called for each decoded transaction emitted from a dataset.
    fn on_transaction(&self, _event: TransactionEvent<'_>) {}
}
