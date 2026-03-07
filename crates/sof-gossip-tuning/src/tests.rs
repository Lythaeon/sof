use crate::{
    constants::{LEGACY_GOSSIP_CHANNEL_CAPACITY, VPS_GOSSIP_CHANNEL_CAPACITY},
    error::TuningValueError,
    newtypes::{QueueCapacity, TvuReceiveSocketCount},
    types::{GossipTuningProfile, HostProfilePreset, IngestQueueMode},
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
    assert_eq!(tuning.tvu_receive_sockets.get(), 2);
}
