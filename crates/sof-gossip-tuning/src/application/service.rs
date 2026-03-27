//! Application service that projects host presets into SOF-supported runtime tuning.

use crate::{
    application::ports::RuntimeTuningPort,
    domain::{
        constants::{
            DEDICATED_GOSSIP_CHANNEL_CAPACITY, DEDICATED_GOSSIP_CHANNEL_CONSUME_CAPACITY,
            DEDICATED_GOSSIP_THREADS, DEDICATED_SHRED_DEDUP_CAPACITY,
            DEDICATED_SOCKET_CONSUME_CHANNEL_CAPACITY, DEDICATED_TVU_RECEIVE_SOCKETS,
            DEFAULT_INGEST_QUEUE_CAPACITY, DEFAULT_RECEIVER_COALESCE_WAIT_MS,
            DEFAULT_UDP_BATCH_SIZE, HOME_GOSSIP_CHANNEL_CAPACITY,
            HOME_GOSSIP_CHANNEL_CONSUME_CAPACITY, HOME_GOSSIP_THREADS, HOME_INGEST_QUEUE_CAPACITY,
            HOME_SHRED_DEDUP_CAPACITY, HOME_TVU_RECEIVE_SOCKETS, HOME_UDP_BATCH_SIZE,
            VPS_GOSSIP_CHANNEL_CONSUME_CAPACITY, VPS_GOSSIP_RECEIVER_CHANNEL_CAPACITY,
            VPS_GOSSIP_THREADS, VPS_SHRED_DEDUP_CAPACITY, VPS_SOCKET_CONSUME_CHANNEL_CAPACITY,
            VPS_TVU_RECEIVE_SOCKETS, VPS_UDP_BATCH_SIZE,
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
                    ingest_queue_capacity: QueueCapacity::fixed(HOME_INGEST_QUEUE_CAPACITY),
                    udp_batch_size: HOME_UDP_BATCH_SIZE,
                    receiver_coalesce_window: ReceiverCoalesceWindow::fixed(
                        DEFAULT_RECEIVER_COALESCE_WAIT_MS,
                    ),
                    udp_receiver_core: None,
                    udp_receiver_pin_by_port: false,
                    tvu_receive_sockets: TvuReceiveSocketCount::fixed(HOME_TVU_RECEIVE_SOCKETS),
                    gossip_channel_consume_capacity: QueueCapacity::fixed(
                        HOME_GOSSIP_CHANNEL_CONSUME_CAPACITY,
                    ),
                    gossip_consume_threads: HOME_GOSSIP_THREADS,
                    gossip_listen_threads: HOME_GOSSIP_THREADS,
                    gossip_run_threads: HOME_GOSSIP_THREADS,
                    shred_dedup_capacity: HOME_SHRED_DEDUP_CAPACITY,
                },
                channels: GossipChannelTuning {
                    gossip_receiver_channel_capacity: QueueCapacity::fixed(
                        HOME_GOSSIP_CHANNEL_CAPACITY,
                    ),
                    socket_consume_channel_capacity: QueueCapacity::fixed(
                        HOME_GOSSIP_CHANNEL_CAPACITY,
                    ),
                    gossip_response_channel_capacity: QueueCapacity::fixed(
                        HOME_GOSSIP_CHANNEL_CAPACITY,
                    ),
                },
                fanout: ReceiverFanoutProfile::Conservative,
            },
            HostProfilePreset::Vps => GossipTuningProfile {
                preset,
                runtime: SofRuntimeTuning {
                    ingest_queue_mode: IngestQueueMode::Lockfree,
                    ingest_queue_capacity: QueueCapacity::fixed(DEFAULT_INGEST_QUEUE_CAPACITY),
                    udp_batch_size: VPS_UDP_BATCH_SIZE,
                    receiver_coalesce_window: ReceiverCoalesceWindow::fixed(
                        DEFAULT_RECEIVER_COALESCE_WAIT_MS,
                    ),
                    udp_receiver_core: None,
                    udp_receiver_pin_by_port: false,
                    tvu_receive_sockets: TvuReceiveSocketCount::fixed(VPS_TVU_RECEIVE_SOCKETS),
                    gossip_channel_consume_capacity: QueueCapacity::fixed(
                        VPS_GOSSIP_CHANNEL_CONSUME_CAPACITY,
                    ),
                    gossip_consume_threads: VPS_GOSSIP_THREADS,
                    gossip_listen_threads: VPS_GOSSIP_THREADS,
                    gossip_run_threads: VPS_GOSSIP_THREADS,
                    shred_dedup_capacity: VPS_SHRED_DEDUP_CAPACITY,
                },
                channels: GossipChannelTuning {
                    gossip_receiver_channel_capacity: QueueCapacity::fixed(
                        VPS_GOSSIP_RECEIVER_CHANNEL_CAPACITY,
                    ),
                    socket_consume_channel_capacity: QueueCapacity::fixed(
                        VPS_SOCKET_CONSUME_CHANNEL_CAPACITY,
                    ),
                    gossip_response_channel_capacity: QueueCapacity::fixed(
                        VPS_SOCKET_CONSUME_CHANNEL_CAPACITY,
                    ),
                },
                fanout: ReceiverFanoutProfile::Balanced,
            },
            HostProfilePreset::Dedicated => GossipTuningProfile {
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
                    tvu_receive_sockets: TvuReceiveSocketCount::fixed(
                        DEDICATED_TVU_RECEIVE_SOCKETS,
                    ),
                    gossip_channel_consume_capacity: QueueCapacity::fixed(
                        DEDICATED_GOSSIP_CHANNEL_CONSUME_CAPACITY,
                    ),
                    gossip_consume_threads: DEDICATED_GOSSIP_THREADS,
                    gossip_listen_threads: DEDICATED_GOSSIP_THREADS,
                    gossip_run_threads: DEDICATED_GOSSIP_THREADS,
                    shred_dedup_capacity: DEDICATED_SHRED_DEDUP_CAPACITY,
                },
                channels: GossipChannelTuning {
                    gossip_receiver_channel_capacity: QueueCapacity::fixed(
                        DEDICATED_GOSSIP_CHANNEL_CAPACITY,
                    ),
                    socket_consume_channel_capacity: QueueCapacity::fixed(
                        DEDICATED_SOCKET_CONSUME_CHANNEL_CAPACITY,
                    ),
                    gossip_response_channel_capacity: QueueCapacity::fixed(
                        DEDICATED_SOCKET_CONSUME_CHANNEL_CAPACITY,
                    ),
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

    /// Returns the gossip queue plan for one profile.
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
        port.set_gossip_channel_tuning(profile.channels);
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
        port.set_gossip_channel_consume_capacity(runtime.gossip_channel_consume_capacity);
        port.set_gossip_consume_threads(runtime.gossip_consume_threads);
        port.set_gossip_listen_threads(runtime.gossip_listen_threads);
        port.set_gossip_run_threads(runtime.gossip_run_threads);
        port.set_shred_dedup_capacity(runtime.shred_dedup_capacity);
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

    /// Returns the gossip queue plan.
    #[must_use]
    pub const fn pending_gossip_queue_plan(self) -> PendingGossipQueuePlan {
        GossipTuningService::pending_gossip_queue_plan(self)
    }
}
