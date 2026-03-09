#![allow(clippy::assertions_on_constants, clippy::missing_const_for_fn)]
#![doc = "End-to-end kernel-bypass ingress test for SOF runtime."]
#![cfg(feature = "kernel-bypass")]

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use sof::{
    framework::{ObserverPlugin, PluginHost, RawPacketEvent, ShredEvent},
    ingest::{RawPacket, RawPacketBatch, RawPacketIngress},
    protocol::shred_wire::{SIZE_OF_DATA_SHRED_PAYLOAD, VARIANT_MERKLE_DATA},
    runtime,
    shred::wire::SIZE_OF_DATA_SHRED_HEADERS,
};
use tokio::sync::mpsc;

const SHRED_PAYLOAD_BYTES: usize = 128;
const TOTAL_PACKETS: usize = 600;
const BATCH_SIZE: usize = 20;
const SHRED_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
struct RawIngressSnapshot {
    packets: u64,
    bytes: u64,
    shreds: u64,
    data_shreds: u64,
    code_shreds: u64,
    source_8899_packets: u64,
    source_8900_packets: u64,
    source_other_packets: u64,
}

#[derive(Default)]
struct RawIngressCounterPlugin {
    packets: AtomicU64,
    bytes: AtomicU64,
    shreds: AtomicU64,
    data_shreds: AtomicU64,
    code_shreds: AtomicU64,
    source_8899_packets: AtomicU64,
    source_8900_packets: AtomicU64,
    source_other_packets: AtomicU64,
}

impl RawIngressCounterPlugin {
    fn snapshot(&self) -> RawIngressSnapshot {
        RawIngressSnapshot {
            packets: self.packets.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            shreds: self.shreds.load(Ordering::Relaxed),
            data_shreds: self.data_shreds.load(Ordering::Relaxed),
            code_shreds: self.code_shreds.load(Ordering::Relaxed),
            source_8899_packets: self.source_8899_packets.load(Ordering::Relaxed),
            source_8900_packets: self.source_8900_packets.load(Ordering::Relaxed),
            source_other_packets: self.source_other_packets.load(Ordering::Relaxed),
        }
    }
}

#[async_trait]
impl ObserverPlugin for RawIngressCounterPlugin {
    fn name(&self) -> &'static str {
        "kernel-bypass-ingress-counter-plugin"
    }

    fn wants_raw_packet(&self) -> bool {
        true
    }

    fn wants_shred(&self) -> bool {
        true
    }

    async fn on_raw_packet(&self, event: RawPacketEvent) {
        self.packets.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(
            u64::try_from(event.bytes.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        match event.source.port() {
            8_899 => {
                self.source_8899_packets.fetch_add(1, Ordering::Relaxed);
            }
            8_900 => {
                self.source_8900_packets.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                self.source_other_packets.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    async fn on_shred(&self, event: ShredEvent) {
        self.shreds.fetch_add(1, Ordering::Relaxed);
        match event.parsed.as_ref() {
            sof::shred::wire::ParsedShredHeader::Data(_) => {
                self.data_shreds.fetch_add(1, Ordering::Relaxed);
            }
            sof::shred::wire::ParsedShredHeader::Code(_) => {
                self.code_shreds.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn build_raw_packet(sequence: u64, source_port: u16) -> RawPacket {
    let slot = (sequence / 128).saturating_add(1);
    let index = u32::try_from(sequence % 128).unwrap_or(0);
    let fec_set_index = index;
    let declared_size =
        u16::try_from(SIZE_OF_DATA_SHRED_HEADERS.saturating_add(SHRED_PAYLOAD_BYTES))
            .unwrap_or(u16::MAX);
    let mut bytes = vec![0_u8; SIZE_OF_DATA_SHRED_PAYLOAD];

    // Distinct signature prefix to avoid dedupe collisions in synthetic traffic.
    write_bytes(&mut bytes, 0, &slot.to_le_bytes());
    write_bytes(&mut bytes, 8, &index.to_le_bytes());
    write_bytes(&mut bytes, 12, &fec_set_index.to_le_bytes());

    write_byte(&mut bytes, 64, VARIANT_MERKLE_DATA);
    write_bytes(&mut bytes, 65, &slot.to_le_bytes());
    write_bytes(&mut bytes, 73, &index.to_le_bytes());
    write_bytes(&mut bytes, 77, &SHRED_VERSION.to_le_bytes());
    write_bytes(&mut bytes, 79, &fec_set_index.to_le_bytes());
    write_bytes(&mut bytes, 83, &0_u16.to_le_bytes());
    write_byte(&mut bytes, 85, 0); // no DATA_COMPLETE/LAST_IN_SLOT: avoid dataset decode work in this e2e.
    write_bytes(&mut bytes, 86, &declared_size.to_le_bytes());
    let payload_end = SIZE_OF_DATA_SHRED_HEADERS.saturating_add(SHRED_PAYLOAD_BYTES);
    fill_bytes(&mut bytes, 88, payload_end, 0xAB);

    RawPacket {
        source: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), source_port),
        ingress: RawPacketIngress::Udp,
        bytes: bytes.into(),
    }
}

fn write_bytes(buffer: &mut [u8], offset: usize, value: &[u8]) {
    let (_, tail) = buffer.split_at_mut(offset);
    let (slot, _) = tail.split_at_mut(value.len());
    slot.copy_from_slice(value);
}

fn write_byte(buffer: &mut [u8], offset: usize, value: u8) {
    let (_, tail) = buffer.split_at_mut(offset);
    let (slot, _) = tail.split_at_mut(1);
    if let Some(first) = slot.first_mut() {
        *first = value;
    }
}

fn fill_bytes(buffer: &mut [u8], start: usize, end: usize, value: u8) {
    let (_, tail) = buffer.split_at_mut(start);
    let len = end.saturating_sub(start);
    let (slot, _) = tail.split_at_mut(len);
    slot.fill(value);
}

async fn wait_for_packets(
    plugin: &RawIngressCounterPlugin,
    expected_packets: u64,
    timeout: Duration,
) -> RawIngressSnapshot {
    let started_at = Instant::now();
    loop {
        let snapshot = plugin.snapshot();
        if snapshot.packets >= expected_packets {
            return snapshot;
        }
        if started_at.elapsed() >= timeout {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kernel_bypass_ingress_e2e_delivers_packets_to_plugins() {
    // Keep test deterministic even when caller shell exports gossip bootstrap env.
    // SAFETY: the test clears process env before spawning worker tasks that consult these vars.
    unsafe {
        std::env::remove_var("SOF_GOSSIP_ENTRYPOINT");
        std::env::remove_var("SOF_GOSSIP_VALIDATORS");
    }
    let plugin = Arc::new(RawIngressCounterPlugin::default());
    let plugin_host = PluginHost::builder()
        .add_shared_plugin(plugin.clone())
        .build();
    let runtime_plugin_host = plugin_host.clone();
    let (tx, rx) = mpsc::channel::<RawPacketBatch>(128);

    let runtime_task = tokio::spawn(async move {
        runtime::run_async_with_plugin_host_and_kernel_bypass_ingress(runtime_plugin_host, rx).await
    });

    let mut sequence = 0_u64;
    let batch_count = TOTAL_PACKETS / BATCH_SIZE;
    for _ in 0..batch_count {
        let mut batch = Vec::with_capacity(BATCH_SIZE);
        for _ in 0..BATCH_SIZE {
            let source_port = match sequence % 3 {
                0 => 8_899,
                1 => 8_900,
                _ => 9_001,
            };
            batch.push(build_raw_packet(sequence, source_port));
            sequence = sequence.saturating_add(1);
        }
        let send_result = tx.send(batch).await;
        assert!(
            send_result.is_ok(),
            "kernel-bypass ingress channel closed during test send: {:?}",
            send_result.err()
        );
    }
    drop(tx);

    let runtime_result = tokio::time::timeout(Duration::from_secs(15), runtime_task).await;
    assert!(
        runtime_result.is_ok(),
        "runtime task timed out: {:?}",
        runtime_result.err()
    );
    let join_result = match runtime_result {
        Ok(join_result) => join_result,
        Err(error) => {
            assert!(
                false,
                "runtime timeout unexpectedly absent from prior assertion: {error}"
            );
            return;
        }
    };
    assert!(
        join_result.is_ok(),
        "runtime task join failed: {:?}",
        join_result.err()
    );
    let run_result = match join_result {
        Ok(run_result) => run_result,
        Err(error) => {
            assert!(
                false,
                "runtime join result unexpectedly absent from prior assertion: {error}"
            );
            return;
        }
    };
    assert!(
        run_result.is_ok(),
        "runtime returned error: {:?}",
        run_result.err()
    );

    let snapshot = wait_for_packets(
        plugin.as_ref(),
        TOTAL_PACKETS as u64,
        Duration::from_secs(5),
    )
    .await;
    let expected_packets = u64::try_from(TOTAL_PACKETS).unwrap_or(u64::MAX);
    let expected_bytes =
        u64::try_from(SIZE_OF_DATA_SHRED_PAYLOAD * TOTAL_PACKETS).unwrap_or(u64::MAX);
    let expected_per_port = expected_packets / 3;

    assert_eq!(
        snapshot.packets, expected_packets,
        "plugin did not observe all kernel-bypass ingress packets"
    );
    assert_eq!(
        snapshot.bytes, expected_bytes,
        "plugin observed unexpected aggregate ingress payload bytes"
    );
    assert_eq!(
        snapshot.shreds, expected_packets,
        "plugin did not observe all parsed shreds from kernel-bypass ingress"
    );
    assert_eq!(
        snapshot.data_shreds, expected_packets,
        "expected all synthetic packets to parse as data shreds"
    );
    assert_eq!(snapshot.code_shreds, 0, "did not expect code shreds");
    assert_eq!(
        snapshot.source_8899_packets, expected_per_port,
        "unexpected 8899 source packet count"
    );
    assert_eq!(
        snapshot.source_8900_packets, expected_per_port,
        "unexpected 8900 source packet count"
    );
    assert_eq!(
        snapshot.source_other_packets, expected_per_port,
        "unexpected non-8899/8900 source packet count"
    );
    assert_eq!(
        plugin_host.dropped_event_count(),
        0,
        "plugin host dropped events during kernel-bypass ingress e2e"
    );
}
