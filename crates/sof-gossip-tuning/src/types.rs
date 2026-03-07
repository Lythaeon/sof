//! Public tuning enums and structs that define the SOF-supported control surface.

use crate::newtypes::{CpuCoreIndex, QueueCapacity, ReceiverCoalesceWindow, TvuReceiveSocketCount};

/// Receiver thread pinning policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverPinningPolicy {
    /// Leave receiver threads to the scheduler.
    Inherit,
    /// Pin all receiver threads to a single core.
    FixedCore(CpuCoreIndex),
    /// Let SOF pin receivers deterministically by port.
    PinByPort,
}

/// High-level receiver fanout profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverFanoutProfile {
    /// Keep receiver fanout minimal to reduce background overhead.
    Conservative,
    /// Balanced settings for a small VPS.
    Balanced,
    /// Aggressive fanout for dedicated hosts.
    Aggressive,
}

/// Built-in host profile preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostProfilePreset {
    /// Home or low-end host with constrained ingress.
    Home,
    /// VPS or small dedicated instance.
    Vps,
    /// Dedicated high-throughput host.
    Dedicated,
}

/// Current ingest queue mode supported by SOF runtime env.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestQueueMode {
    /// Bounded channel mode.
    Bounded,
    /// Unbounded channel mode.
    Unbounded,
    /// Lock-free ring mode.
    Lockfree,
}

impl IngestQueueMode {
    /// Returns the SOF env string form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bounded => "bounded",
            Self::Unbounded => "unbounded",
            Self::Lockfree => "lockfree",
        }
    }
}

/// SOF-supported receiver/runtime knobs that can be translated directly into runtime setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SofRuntimeTuning {
    /// Ingest queue mode.
    pub ingest_queue_mode: IngestQueueMode,
    /// SOF ingest queue capacity.
    pub ingest_queue_capacity: QueueCapacity,
    /// UDP batch size.
    pub udp_batch_size: u16,
    /// Receiver coalesce window.
    pub receiver_coalesce_window: ReceiverCoalesceWindow,
    /// Optional fixed receiver core.
    pub udp_receiver_core: Option<CpuCoreIndex>,
    /// Whether SOF should pin receivers by port.
    pub udp_receiver_pin_by_port: bool,
    /// TVU receive socket count used by gossip bootstrap.
    pub tvu_receive_sockets: TvuReceiveSocketCount,
}

/// Agave gossip-side queue knobs that are not yet wired through SOF bootstrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingGossipQueuePlan {
    /// Target gossip receiver queue capacity.
    pub gossip_receiver_channel_capacity: QueueCapacity,
    /// Target socket consume queue capacity.
    pub socket_consume_channel_capacity: QueueCapacity,
    /// Target gossip response queue capacity.
    pub gossip_response_channel_capacity: QueueCapacity,
    /// Desired receiver fanout profile for future integration.
    pub fanout: ReceiverFanoutProfile,
}

/// Agave gossip-side queue knobs that are currently upstream-internal in practice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GossipChannelTuning {
    /// Receiver socket -> gossip request channel capacity.
    pub gossip_receiver_channel_capacity: QueueCapacity,
    /// Socket consume channel capacity.
    pub socket_consume_channel_capacity: QueueCapacity,
    /// Gossip response channel capacity.
    pub gossip_response_channel_capacity: QueueCapacity,
}

/// Full scaffold for a host tuning profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GossipTuningProfile {
    /// Host profile label.
    pub preset: HostProfilePreset,
    /// SOF-supported runtime tuning.
    pub runtime: SofRuntimeTuning,
    /// Gossip internal queue tuning.
    pub channels: GossipChannelTuning,
    /// Desired receiver fanout posture for future integration.
    pub fanout: ReceiverFanoutProfile,
}
