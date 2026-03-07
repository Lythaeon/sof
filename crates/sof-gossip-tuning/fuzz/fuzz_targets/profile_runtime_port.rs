#![no_main]

use libfuzzer_sys::fuzz_target;
use sof_gossip_tuning::{
    GossipTuningProfile, GossipTuningService, HostProfilePreset, IngestQueueMode,
    QueueCapacity, ReceiverCoalesceWindow, RuntimeTuningPort, TvuReceiveSocketCount,
};

#[derive(Default)]
struct RecordingPort {
    mode: Option<IngestQueueMode>,
    capacity: Option<QueueCapacity>,
    batch_size: Option<u16>,
    window: Option<ReceiverCoalesceWindow>,
    pin_by_port: Option<bool>,
    sockets: Option<TvuReceiveSocketCount>,
}

impl RuntimeTuningPort for RecordingPort {
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
        self.window = Some(window);
    }

    fn set_udp_receiver_core(&mut self, _core: Option<sof_gossip_tuning::CpuCoreIndex>) {}

    fn set_udp_receiver_pin_by_port(&mut self, enabled: bool) {
        self.pin_by_port = Some(enabled);
    }

    fn set_tvu_receive_sockets(&mut self, sockets: TvuReceiveSocketCount) {
        self.sockets = Some(sockets);
    }
}

fn preset_from_byte(byte: u8) -> HostProfilePreset {
    match byte % 3 {
        0 => HostProfilePreset::Home,
        1 => HostProfilePreset::Vps,
        _ => HostProfilePreset::Dedicated,
    }
}

fuzz_target!(|bytes: &[u8]| {
    let preset = preset_from_byte(*bytes.first().unwrap_or(&0));
    let profile = GossipTuningProfile::preset(preset);
    let mut port = RecordingPort::default();
    GossipTuningService::apply_supported_runtime_tuning(profile, &mut port);

    let runtime = GossipTuningService::supported_runtime_tuning(profile);
    assert_eq!(port.mode, Some(runtime.ingest_queue_mode));
    assert_eq!(port.capacity, Some(runtime.ingest_queue_capacity));
    assert_eq!(port.batch_size, Some(runtime.udp_batch_size));
    assert_eq!(port.window, Some(runtime.receiver_coalesce_window));
    assert_eq!(port.pin_by_port, Some(runtime.udp_receiver_pin_by_port));
    assert_eq!(port.sockets, Some(runtime.tvu_receive_sockets));
    assert!(port.capacity.map(QueueCapacity::get).unwrap_or(0) > 0);
    assert!(port.sockets.map(TvuReceiveSocketCount::get).unwrap_or(0) > 0);
});
