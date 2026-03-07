//! Built-in tuning presets and profile-derived views.

use crate::{
    constants::{
        DEFAULT_INGEST_QUEUE_CAPACITY, DEFAULT_RECEIVER_COALESCE_WAIT_MS, DEFAULT_UDP_BATCH_SIZE,
        VPS_GOSSIP_CHANNEL_CAPACITY,
    },
    newtypes::{QueueCapacity, ReceiverCoalesceWindow, TvuReceiveSocketCount},
    types::{
        GossipChannelTuning, GossipTuningProfile, HostProfilePreset, IngestQueueMode,
        PendingGossipQueuePlan, ReceiverFanoutProfile, SofRuntimeTuning,
    },
};

impl GossipTuningProfile {
    /// Returns the built-in profile for a given preset.
    #[must_use]
    pub const fn preset(preset: HostProfilePreset) -> Self {
        match preset {
            HostProfilePreset::Home => Self {
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
            HostProfilePreset::Vps => Self {
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
            HostProfilePreset::Dedicated => Self {
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
