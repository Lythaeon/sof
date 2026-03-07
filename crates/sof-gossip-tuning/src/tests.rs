use crate::{
    application::{ports::RuntimeTuningPort, service::GossipTuningService},
    domain::{
        constants::{
            DEFAULT_UDP_BATCH_SIZE, LEGACY_GOSSIP_CHANNEL_CAPACITY, VPS_GOSSIP_CHANNEL_CAPACITY,
            VPS_TVU_RECEIVE_SOCKETS,
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
fn vps_profile_uses_widened_receiver_queue() {
    let profile = GossipTuningProfile::preset(HostProfilePreset::Vps);
    assert_eq!(
        profile.channels.gossip_receiver_channel_capacity.get(),
        VPS_GOSSIP_CHANNEL_CAPACITY
    );
    assert!(
        profile.channels.gossip_receiver_channel_capacity.get() > LEGACY_GOSSIP_CHANNEL_CAPACITY
    );
}

#[test]
fn runtime_tuning_matches_supported_sof_surface() {
    let tuning = GossipTuningProfile::preset(HostProfilePreset::Vps).supported_runtime_tuning();
    assert_eq!(tuning.ingest_queue_mode, IngestQueueMode::Lockfree);
    assert!(tuning.udp_receiver_pin_by_port);
    assert_eq!(tuning.tvu_receive_sockets.get(), VPS_TVU_RECEIVE_SOCKETS);
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
}

#[test]
fn application_service_projects_profile_through_port() {
    let mut port = RecordingRuntimePort::default();
    GossipTuningService::apply_supported_runtime_tuning(
        GossipTuningProfile::preset(HostProfilePreset::Vps),
        &mut port,
    );

    assert_eq!(port.mode, Some(IngestQueueMode::Lockfree));
    assert_eq!(port.batch_size, Some(DEFAULT_UDP_BATCH_SIZE));
    assert_eq!(port.pin_by_port, Some(true));
    assert_eq!(
        port.sockets.map(TvuReceiveSocketCount::get),
        Some(VPS_TVU_RECEIVE_SOCKETS)
    );
}
