use crate::{
    application::{ports::RuntimeTuningPort, service::GossipTuningService},
    domain::{
        constants::{
            LEGACY_GOSSIP_CHANNEL_CAPACITY, VPS_GOSSIP_RECEIVER_CHANNEL_CAPACITY,
            VPS_GOSSIP_THREADS, VPS_SHRED_DEDUP_CAPACITY, VPS_SOCKET_CONSUME_CHANNEL_CAPACITY,
            VPS_TVU_RECEIVE_SOCKETS, VPS_UDP_BATCH_SIZE,
        },
        error::TuningValueError,
        model::{GossipTuningProfile, HostProfilePreset, IngestQueueMode},
        value_objects::{
            CpuCoreIndex, QueueCapacity, ReceiverCoalesceWindow, TvuReceiveSocketCount,
        },
    },
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
fn vps_profile_matches_validated_public_host_queue_plan() {
    let profile = GossipTuningProfile::preset(HostProfilePreset::Vps);
    assert_eq!(
        profile.channels.gossip_receiver_channel_capacity.get(),
        VPS_GOSSIP_RECEIVER_CHANNEL_CAPACITY
    );
    assert_eq!(
        profile.channels.socket_consume_channel_capacity.get(),
        VPS_SOCKET_CONSUME_CHANNEL_CAPACITY
    );
    assert_eq!(
        profile.channels.gossip_response_channel_capacity.get(),
        VPS_SOCKET_CONSUME_CHANNEL_CAPACITY
    );
    assert!(
        profile.channels.gossip_receiver_channel_capacity.get() > LEGACY_GOSSIP_CHANNEL_CAPACITY
    );
    assert!(
        profile.channels.gossip_receiver_channel_capacity.get()
            > profile.channels.socket_consume_channel_capacity.get(),
        "validated public-host profile keeps the receiver queue ahead of downstream gossip queues",
    );
}

#[test]
fn runtime_tuning_matches_supported_sof_surface() {
    let tuning = GossipTuningProfile::preset(HostProfilePreset::Vps).supported_runtime_tuning();
    assert_eq!(tuning.ingest_queue_mode, IngestQueueMode::Lockfree);
    assert_eq!(tuning.udp_batch_size, VPS_UDP_BATCH_SIZE);
    assert!(tuning.udp_receiver_pin_by_port);
    assert_eq!(tuning.tvu_receive_sockets.get(), VPS_TVU_RECEIVE_SOCKETS);
    assert_eq!(tuning.gossip_consume_threads, VPS_GOSSIP_THREADS);
    assert_eq!(tuning.gossip_listen_threads, VPS_GOSSIP_THREADS);
    assert_eq!(tuning.gossip_run_threads, VPS_GOSSIP_THREADS);
    assert_eq!(tuning.shred_dedup_capacity, VPS_SHRED_DEDUP_CAPACITY);
}

#[derive(Default)]
struct RecordingRuntimePort {
    mode: Option<IngestQueueMode>,
    capacity: Option<QueueCapacity>,
    batch_size: Option<u16>,
    coalesce_window: Option<ReceiverCoalesceWindow>,
    core: Option<Option<CpuCoreIndex>>,
    pin_by_port: Option<bool>,
    sockets: Option<TvuReceiveSocketCount>,
    gossip_channel_consume_capacity: Option<QueueCapacity>,
    gossip_consume_threads: Option<usize>,
    gossip_listen_threads: Option<usize>,
    gossip_run_threads: Option<usize>,
    shred_dedup_capacity: Option<usize>,
    gossip_channels: Option<crate::domain::model::GossipChannelTuning>,
}

impl RuntimeTuningPort for RecordingRuntimePort {
    fn set_ingest_queue_mode(&mut self, mode: IngestQueueMode) {
        self.mode = Some(mode);
    }

    fn set_ingest_queue_capacity(&mut self, capacity: QueueCapacity) {
        self.capacity = Some(capacity);
    }

    fn set_udp_batch_size(&mut self, batch_size: u16) {
        self.batch_size = Some(batch_size);
    }

    fn set_receiver_coalesce_window(&mut self, window: ReceiverCoalesceWindow) {
        self.coalesce_window = Some(window);
    }

    fn set_udp_receiver_core(&mut self, core: Option<CpuCoreIndex>) {
        self.core = Some(core);
    }

    fn set_udp_receiver_pin_by_port(&mut self, enabled: bool) {
        self.pin_by_port = Some(enabled);
    }

    fn set_tvu_receive_sockets(&mut self, sockets: TvuReceiveSocketCount) {
        self.sockets = Some(sockets);
    }

    fn set_gossip_channel_consume_capacity(&mut self, capacity: QueueCapacity) {
        self.gossip_channel_consume_capacity = Some(capacity);
    }

    fn set_gossip_consume_threads(&mut self, thread_count: usize) {
        self.gossip_consume_threads = Some(thread_count);
    }

    fn set_gossip_listen_threads(&mut self, thread_count: usize) {
        self.gossip_listen_threads = Some(thread_count);
    }

    fn set_gossip_run_threads(&mut self, thread_count: usize) {
        self.gossip_run_threads = Some(thread_count);
    }

    fn set_shred_dedup_capacity(&mut self, dedupe_capacity: usize) {
        self.shred_dedup_capacity = Some(dedupe_capacity);
    }

    fn set_gossip_channel_tuning(&mut self, tuning: crate::domain::model::GossipChannelTuning) {
        self.gossip_channels = Some(tuning);
    }
}

#[test]
fn application_service_projects_profile_through_port() {
    let mut port = RecordingRuntimePort::default();
    GossipTuningService::apply_supported_runtime_tuning(
        GossipTuningProfile::preset(HostProfilePreset::Vps),
        &mut port,
    );

    assert_eq!(port.mode, Some(IngestQueueMode::Lockfree));
    assert_eq!(port.batch_size, Some(VPS_UDP_BATCH_SIZE));
    assert_eq!(port.pin_by_port, Some(true));
    assert_eq!(
        port.sockets.map(TvuReceiveSocketCount::get),
        Some(VPS_TVU_RECEIVE_SOCKETS)
    );
    assert_eq!(
        port.gossip_channel_consume_capacity.map(QueueCapacity::get),
        Some(4_096)
    );
    assert_eq!(port.gossip_consume_threads, Some(VPS_GOSSIP_THREADS));
    assert_eq!(port.gossip_listen_threads, Some(VPS_GOSSIP_THREADS));
    assert_eq!(port.gossip_run_threads, Some(VPS_GOSSIP_THREADS));
    assert_eq!(port.shred_dedup_capacity, Some(VPS_SHRED_DEDUP_CAPACITY));
    assert_eq!(
        port.gossip_channels
            .map(|channels| channels.gossip_receiver_channel_capacity.get()),
        Some(VPS_GOSSIP_RECEIVER_CHANNEL_CAPACITY)
    );
}
