#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::missing_docs_in_private_items,
        clippy::panic,
        missing_docs
    )
)]

//! Typed control surface for SOF gossip and ingest tuning.
//!
//! This crate is intentionally narrow:
//! - it models the tuning knobs SOF can already apply directly,
//! - it keeps upstream gossip queue ambitions explicit without pretending they are live,
//! - it gives service builders one typed place to define host-specific tuning presets.

use std::{
    fmt,
    num::{NonZeroU32, NonZeroUsize},
    time::Duration,
};

/// Default coalesce wait used in SOF today.
pub const DEFAULT_RECEIVER_COALESCE_WAIT_MS: u64 = 1;
/// Default UDP batch size used in the current VPS profile.
pub const DEFAULT_UDP_BATCH_SIZE: u16 = 128;
/// Default SOF ingest queue capacity for the lockfree queue.
pub const DEFAULT_INGEST_QUEUE_CAPACITY: u32 = 262_144;
/// Current Agave gossip channel default observed upstream.
pub const LEGACY_GOSSIP_CHANNEL_CAPACITY: u32 = 4_096;
/// Current widened capacity that proved materially better on constrained VPS hosts.
pub const VPS_GOSSIP_CHANNEL_CAPACITY: u32 = 32_768;

/// Error returned when a tuning value violates a basic invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuningValueError {
    /// Capacity values must be non-zero.
    ZeroCapacity,
    /// Millisecond values must be positive.
    ZeroMillis,
    /// TVU socket count must be non-zero.
    ZeroSocketCount,
}

impl fmt::Display for TuningValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCapacity => f.write_str("capacity must be non-zero"),
            Self::ZeroMillis => f.write_str("duration must be positive"),
            Self::ZeroSocketCount => f.write_str("socket count must be non-zero"),
        }
    }
}

impl std::error::Error for TuningValueError {}

/// Typed wrapper for bounded channel capacities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueueCapacity(NonZeroU32);

impl QueueCapacity {
    /// Creates a validated queue capacity.
    ///
    /// # Errors
    /// Returns [`TuningValueError::ZeroCapacity`] when `value` is zero.
    pub fn new(value: u32) -> Result<Self, TuningValueError> {
        NonZeroU32::new(value)
            .map(Self)
            .ok_or(TuningValueError::ZeroCapacity)
    }

    /// Returns the raw capacity value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// Typed wrapper for a millisecond coalesce window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReceiverCoalesceWindow(Duration);

impl ReceiverCoalesceWindow {
    /// Creates a validated coalesce window from milliseconds.
    ///
    /// # Errors
    /// Returns [`TuningValueError::ZeroMillis`] when `value` is zero.
    pub const fn from_millis(value: u64) -> Result<Self, TuningValueError> {
        if value == 0 {
            return Err(TuningValueError::ZeroMillis);
        }
        Ok(Self(Duration::from_millis(value)))
    }

    /// Returns the value in milliseconds.
    #[must_use]
    pub const fn as_millis_u64(self) -> u64 {
        self.0.as_millis() as u64
    }

    /// Returns the underlying duration.
    #[must_use]
    pub const fn as_duration(self) -> Duration {
        self.0
    }
}

/// Typed wrapper for a CPU core index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CpuCoreIndex(usize);

impl CpuCoreIndex {
    /// Creates a CPU core index.
    #[must_use]
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the raw zero-based index.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Number of TVU receive sockets to create during gossip bootstrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TvuReceiveSocketCount(NonZeroUsize);

impl TvuReceiveSocketCount {
    /// Creates a validated TVU socket count.
    ///
    /// # Errors
    /// Returns [`TuningValueError::ZeroSocketCount`] when `value` is zero.
    pub fn new(value: usize) -> Result<Self, TuningValueError> {
        NonZeroUsize::new(value)
            .map(Self)
            .ok_or(TuningValueError::ZeroSocketCount)
    }

    /// Returns the raw socket count.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

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

impl GossipTuningProfile {
    /// Returns the built-in profile for a given preset.
    #[must_use]
    pub const fn preset(preset: HostProfilePreset) -> Self {
        match preset {
            HostProfilePreset::Home => Self {
                preset,
                runtime: SofRuntimeTuning {
                    ingest_queue_mode: IngestQueueMode::Bounded,
                    ingest_queue_capacity: fixed_queue_capacity(65_536),
                    udp_batch_size: 64,
                    receiver_coalesce_window: fixed_coalesce_window(
                        DEFAULT_RECEIVER_COALESCE_WAIT_MS,
                    ),
                    udp_receiver_core: None,
                    udp_receiver_pin_by_port: false,
                    tvu_receive_sockets: fixed_socket_count(1),
                },
                channels: GossipChannelTuning {
                    gossip_receiver_channel_capacity: fixed_queue_capacity(8_192),
                    socket_consume_channel_capacity: fixed_queue_capacity(8_192),
                    gossip_response_channel_capacity: fixed_queue_capacity(8_192),
                },
                fanout: ReceiverFanoutProfile::Conservative,
            },
            HostProfilePreset::Vps => Self {
                preset,
                runtime: SofRuntimeTuning {
                    ingest_queue_mode: IngestQueueMode::Lockfree,
                    ingest_queue_capacity: fixed_queue_capacity(DEFAULT_INGEST_QUEUE_CAPACITY),
                    udp_batch_size: DEFAULT_UDP_BATCH_SIZE,
                    receiver_coalesce_window: fixed_coalesce_window(
                        DEFAULT_RECEIVER_COALESCE_WAIT_MS,
                    ),
                    udp_receiver_core: None,
                    udp_receiver_pin_by_port: true,
                    tvu_receive_sockets: fixed_socket_count(2),
                },
                channels: GossipChannelTuning {
                    gossip_receiver_channel_capacity: fixed_queue_capacity(
                        VPS_GOSSIP_CHANNEL_CAPACITY,
                    ),
                    socket_consume_channel_capacity: fixed_queue_capacity(8_192),
                    gossip_response_channel_capacity: fixed_queue_capacity(8_192),
                },
                fanout: ReceiverFanoutProfile::Balanced,
            },
            HostProfilePreset::Dedicated => Self {
                preset,
                runtime: SofRuntimeTuning {
                    ingest_queue_mode: IngestQueueMode::Lockfree,
                    ingest_queue_capacity: fixed_queue_capacity(DEFAULT_INGEST_QUEUE_CAPACITY),
                    udp_batch_size: 128,
                    receiver_coalesce_window: fixed_coalesce_window(1),
                    udp_receiver_core: None,
                    udp_receiver_pin_by_port: true,
                    tvu_receive_sockets: fixed_socket_count(4),
                },
                channels: GossipChannelTuning {
                    gossip_receiver_channel_capacity: fixed_queue_capacity(65_536),
                    socket_consume_channel_capacity: fixed_queue_capacity(32_768),
                    gossip_response_channel_capacity: fixed_queue_capacity(32_768),
                },
                fanout: ReceiverFanoutProfile::Aggressive,
            },
        }
    }

    /// Returns the subset of tuning that SOF can apply directly today.
    #[must_use]
    pub const fn supported_runtime_tuning(self) -> SofRuntimeTuning {
        self.runtime
    }

    /// Returns the not-yet-wired upstream gossip queue plan.
    ///
    /// This is intentionally advisory only until SOF can thread the settings into the actual
    /// Agave gossip bootstrap path.
    #[must_use]
    pub const fn pending_gossip_queue_plan(self) -> PendingGossipQueuePlan {
        PendingGossipQueuePlan {
            gossip_receiver_channel_capacity: self.channels.gossip_receiver_channel_capacity,
            socket_consume_channel_capacity: self.channels.socket_consume_channel_capacity,
            gossip_response_channel_capacity: self.channels.gossip_response_channel_capacity,
            fanout: self.fanout,
        }
    }
}

/// Builds one fixed queue capacity for internal presets.
const fn fixed_queue_capacity(value: u32) -> QueueCapacity {
    match NonZeroU32::new(value) {
        Some(value) => QueueCapacity(value),
        None => QueueCapacity(NonZeroU32::MIN),
    }
}

/// Builds one fixed TVU socket count for internal presets.
const fn fixed_socket_count(value: usize) -> TvuReceiveSocketCount {
    match NonZeroUsize::new(value) {
        Some(value) => TvuReceiveSocketCount(value),
        None => TvuReceiveSocketCount(NonZeroUsize::MIN),
    }
}

/// Builds one fixed coalesce window for internal presets.
const fn fixed_coalesce_window(value: u64) -> ReceiverCoalesceWindow {
    ReceiverCoalesceWindow(Duration::from_millis(if value == 0 { 1 } else { value }))
}

#[cfg(test)]
mod tests {
    use super::{
        GossipTuningProfile, HostProfilePreset, IngestQueueMode, LEGACY_GOSSIP_CHANNEL_CAPACITY,
        QueueCapacity, TuningValueError, TvuReceiveSocketCount, VPS_GOSSIP_CHANNEL_CAPACITY,
    };

    #[test]
    fn rejects_zero_capacity() {
        assert_eq!(
            QueueCapacity::new(0).expect_err("zero capacity must fail"),
            TuningValueError::ZeroCapacity
        );
    }

    #[test]
    fn rejects_zero_socket_count() {
        assert_eq!(
            TvuReceiveSocketCount::new(0).expect_err("zero socket count must fail"),
            TuningValueError::ZeroSocketCount
        );
    }

    #[test]
    fn vps_profile_uses_widened_receiver_queue() {
        let profile = GossipTuningProfile::preset(HostProfilePreset::Vps);
        assert_eq!(
            profile.channels.gossip_receiver_channel_capacity.get(),
            VPS_GOSSIP_CHANNEL_CAPACITY
        );
        assert!(
            profile.channels.gossip_receiver_channel_capacity.get()
                > LEGACY_GOSSIP_CHANNEL_CAPACITY
        );
    }

    #[test]
    fn runtime_tuning_matches_supported_sof_surface() {
        let tuning = GossipTuningProfile::preset(HostProfilePreset::Vps).supported_runtime_tuning();
        assert_eq!(tuning.ingest_queue_mode, IngestQueueMode::Lockfree);
        assert!(tuning.udp_receiver_pin_by_port);
        assert_eq!(tuning.tvu_receive_sockets.get(), 2);
    }
}
