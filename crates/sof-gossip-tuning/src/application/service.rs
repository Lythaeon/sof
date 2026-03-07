//! Application service that projects host presets into SOF-supported runtime tuning.

use crate::{
    application::ports::RuntimeTuningPort,
    domain::{
        constants::{
            DEFAULT_INGEST_QUEUE_CAPACITY, DEFAULT_RECEIVER_COALESCE_WAIT_MS,
            DEFAULT_UDP_BATCH_SIZE, VPS_GOSSIP_CHANNEL_CAPACITY,
        },
        model::{
            GossipChannelTuning, GossipTuningProfile, HostProfilePreset, IngestQueueMode,
            PendingGossipQueuePlan, ReceiverFanoutProfile, SofRuntimeTuning,
        },
        value_objects::{QueueCapacity, ReceiverCoalesceWindow, TvuReceiveSocketCount},
    },
};

/// Stateless application service for host tuning profiles.
#[derive(Debug, Clone, Copy, Default)]
pub struct GossipTuningService;

impl GossipTuningService {
    /// Projects one built-in preset into the domain aggregate root.
    #[must_use]
    pub const fn preset_profile(preset: HostProfilePreset) -> GossipTuningProfile {
        match preset {
            HostProfilePreset::Home => GossipTuningProfile {
                preset,
                runtime: SofRuntimeTuning {
                    ingest_queue_mode: IngestQueueMode::Bounded,
                    ingest_queue_capacity: QueueCapacity::fixed(65_536),
                    udp_batch_size: 64,
                    receiver_coalesce_window: ReceiverCoalesceWindow::fixed(
                        DEFAULT_RECEIVER_COALESCE_WAIT_MS,
                    ),
                    udp_receiver_core: None,
                    udp_receiver_pin_by_port: false,
                    tvu_receive_sockets: TvuReceiveSocketCount::fixed(1),
                },
                channels: GossipChannelTuning {
                    gossip_receiver_channel_capacity: QueueCapacity::fixed(8_192),
                    socket_consume_channel_capacity: QueueCapacity::fixed(8_192),
                    gossip_response_channel_capacity: QueueCapacity::fixed(8_192),
                },
                fanout: ReceiverFanoutProfile::Conservative,
            },
            HostProfilePreset::Vps => GossipTuningProfile {
                preset,
                runtime: SofRuntimeTuning {
                    ingest_queue_mode: IngestQueueMode::Lockfree,
                    ingest_queue_capacity: QueueCapacity::fixed(DEFAULT_INGEST_QUEUE_CAPACITY),
                    udp_batch_size: DEFAULT_UDP_BATCH_SIZE,
                    receiver_coalesce_window: ReceiverCoalesceWindow::fixed(
                        DEFAULT_RECEIVER_COALESCE_WAIT_MS,
                    ),
                    udp_receiver_core: None,
                    udp_receiver_pin_by_port: true,
                    tvu_receive_sockets: TvuReceiveSocketCount::fixed(2),
                },
                channels: GossipChannelTuning {
                    gossip_receiver_channel_capacity: QueueCapacity::fixed(
                        VPS_GOSSIP_CHANNEL_CAPACITY,
                    ),
                    socket_consume_channel_capacity: QueueCapacity::fixed(8_192),
                    gossip_response_channel_capacity: QueueCapacity::fixed(8_192),
                },
                fanout: ReceiverFanoutProfile::Balanced,
            },
            HostProfilePreset::Dedicated => GossipTuningProfile {
                preset,
                runtime: SofRuntimeTuning {
                    ingest_queue_mode: IngestQueueMode::Lockfree,
                    ingest_queue_capacity: QueueCapacity::fixed(DEFAULT_INGEST_QUEUE_CAPACITY),
                    udp_batch_size: 128,
                    receiver_coalesce_window: ReceiverCoalesceWindow::fixed(1),
                    udp_receiver_core: None,
                    udp_receiver_pin_by_port: true,
                    tvu_receive_sockets: TvuReceiveSocketCount::fixed(4),
                },
                channels: GossipChannelTuning {
                    gossip_receiver_channel_capacity: QueueCapacity::fixed(65_536),
                    socket_consume_channel_capacity: QueueCapacity::fixed(32_768),
                    gossip_response_channel_capacity: QueueCapacity::fixed(32_768),
                },
                fanout: ReceiverFanoutProfile::Aggressive,
            },
        }
    }

    /// Returns the SOF-supported runtime subset for one profile.
    #[must_use]
    pub const fn supported_runtime_tuning(profile: GossipTuningProfile) -> SofRuntimeTuning {
        profile.runtime
    }

    /// Returns the not-yet-wired upstream gossip queue plan for one profile.
    #[must_use]
    pub const fn pending_gossip_queue_plan(profile: GossipTuningProfile) -> PendingGossipQueuePlan {
        PendingGossipQueuePlan {
            gossip_receiver_channel_capacity: profile.channels.gossip_receiver_channel_capacity,
            socket_consume_channel_capacity: profile.channels.socket_consume_channel_capacity,
            gossip_response_channel_capacity: profile.channels.gossip_response_channel_capacity,
            fanout: profile.fanout,
        }
    }

    /// Applies the SOF-supported subset of one profile through an output port.
    pub fn apply_supported_runtime_tuning<P>(profile: GossipTuningProfile, port: &mut P)
    where
        P: RuntimeTuningPort,
    {
        let runtime = Self::supported_runtime_tuning(profile);
        Self::apply_runtime_tuning(runtime, port);
    }

    /// Applies one already-projected runtime tuning bundle through an output port.
    pub fn apply_runtime_tuning<P>(runtime: SofRuntimeTuning, port: &mut P)
    where
        P: RuntimeTuningPort,
    {
        port.set_ingest_queue_mode(runtime.ingest_queue_mode);
        port.set_ingest_queue_capacity(runtime.ingest_queue_capacity);
        port.set_udp_batch_size(runtime.udp_batch_size);
        port.set_receiver_coalesce_window(runtime.receiver_coalesce_window);
        port.set_udp_receiver_core(runtime.udp_receiver_core);
        port.set_udp_receiver_pin_by_port(runtime.udp_receiver_pin_by_port);
        port.set_tvu_receive_sockets(runtime.tvu_receive_sockets);
    }
}

impl GossipTuningProfile {
    /// Returns the built-in profile for a given preset.
    #[must_use]
    pub const fn preset(preset: HostProfilePreset) -> Self {
        GossipTuningService::preset_profile(preset)
    }

    /// Returns the subset of tuning that SOF can apply directly today.
    #[must_use]
    pub const fn supported_runtime_tuning(self) -> SofRuntimeTuning {
        GossipTuningService::supported_runtime_tuning(self)
    }

    /// Returns the not-yet-wired upstream gossip queue plan.
    #[must_use]
    pub const fn pending_gossip_queue_plan(self) -> PendingGossipQueuePlan {
        GossipTuningService::pending_gossip_queue_plan(self)
    }
}
