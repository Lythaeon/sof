//! Output ports for applying SOF-supported tuning into host-specific adapters.

use crate::domain::{
    model::IngestQueueMode,
    value_objects::{CpuCoreIndex, QueueCapacity, ReceiverCoalesceWindow, TvuReceiveSocketCount},
};

/// Output port that can receive the SOF-supported subset of one gossip tuning profile.
pub trait RuntimeTuningPort {
    /// Applies the ingest queue mode.
    fn set_ingest_queue_mode(&mut self, mode: IngestQueueMode);
    /// Applies the ingest queue capacity.
    fn set_ingest_queue_capacity(&mut self, capacity: QueueCapacity);
    /// Applies the UDP batch size.
    fn set_udp_batch_size(&mut self, batch_size: u16);
    /// Applies the receiver coalesce window.
    fn set_receiver_coalesce_window(&mut self, window: ReceiverCoalesceWindow);
    /// Applies the optional fixed receiver core.
    fn set_udp_receiver_core(&mut self, core: Option<CpuCoreIndex>);
    /// Applies port-based receiver pinning.
    fn set_udp_receiver_pin_by_port(&mut self, enabled: bool);
    /// Applies the TVU receive socket count.
    fn set_tvu_receive_sockets(&mut self, sockets: TvuReceiveSocketCount);
}
