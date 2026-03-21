#![allow(missing_docs)]
//! Criterion benches for public SOF hot paths.

use std::time::{Duration, Instant};

use criterion::{BatchSize, Criterion, Throughput, black_box, criterion_group, criterion_main};
use sof::{
    event::ForkSlotStatus,
    framework::{
        DerivedStateCheckpoint, DerivedStateConsumer, DerivedStateConsumerConfig,
        DerivedStateConsumerContext, DerivedStateConsumerFault, DerivedStateConsumerSetupError,
        DerivedStateFeedEnvelope, DerivedStateFeedEvent, DerivedStateHost, FeedWatermarks,
        SlotStatusChangedEvent,
    },
    protocol::shred_wire::VARIANT_MERKLE_DATA,
    reassembly::dataset::{DataSetReassembler, SharedPayloadFragment},
    relay::{RecentShredRingBuffer, RelayRangeLimits, RelayRangeRequest, SharedRelayCache},
    shred::wire::{SIZE_OF_DATA_SHRED_HEADERS, SIZE_OF_DATA_SHRED_PAYLOAD, parse_shred_header},
};

const RELAY_BENCH_SHREDS: usize = 512;
const RELAY_QUERY_SHREDS: usize = 1024;
const RELAY_QUERY_START_INDEX: u32 = 320;
const RELAY_QUERY_END_INDEX: u32 = 383;
const DATASET_BENCH_SHREDS: usize = 32;
const DERIVED_STATE_BATCH_EVENTS: usize = 64;

fn bench_shared_relay_cache_insert(c: &mut Criterion) {
    let packets = relay_packets(RELAY_BENCH_SHREDS);
    let mut group = c.benchmark_group("relay_cache_insert");
    group.throughput(Throughput::Elements(RELAY_BENCH_SHREDS as u64));
    group.bench_function("shared_cache_insert_512", |b| {
        b.iter_batched(
            || SharedRelayCache::new(RecentShredRingBuffer::new(16_384, Duration::from_secs(30))),
            |cache| {
                let now = Instant::now();
                for (offset, (packet, parsed)) in packets.iter().enumerate() {
                    let observed_at =
                        now + Duration::from_micros(u64::try_from(offset).unwrap_or(u64::MAX));
                    let outcome = cache.insert(packet, parsed, observed_at);
                    black_box(outcome);
                }
                black_box(cache.len());
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_shared_relay_cache_query_range(c: &mut Criterion) {
    let cache = populated_shared_relay_cache(RELAY_QUERY_SHREDS);
    let request = RelayRangeRequest {
        slot: 42,
        start_index: RELAY_QUERY_START_INDEX,
        end_index: RELAY_QUERY_END_INDEX,
    };
    let limits = RelayRangeLimits {
        max_request_span: 256,
        max_response_shreds: 128,
        max_response_bytes: 1 << 20,
    };
    let mut group = c.benchmark_group("relay_cache_query");
    group.throughput(Throughput::Elements(u64::from(
        RELAY_QUERY_END_INDEX
            .saturating_sub(RELAY_QUERY_START_INDEX)
            .saturating_add(1),
    )));
    group.bench_function("shared_cache_query_range_64", |b| {
        b.iter(|| {
            let response = cache
                .query_range(request, limits, Instant::now())
                .expect("relay range query should succeed");
            black_box(response.len());
        });
    });
    group.finish();
}

fn bench_dataset_reassembly(c: &mut Criterion) {
    let fragments = dataset_fragments(DATASET_BENCH_SHREDS);
    let mut group = c.benchmark_group("dataset_reassembly");
    group.throughput(Throughput::Elements(DATASET_BENCH_SHREDS as u64));
    group.bench_function("contiguous_slot_32", |b| {
        b.iter_batched(
            || DataSetReassembler::new(256),
            |mut reassembler| {
                let mut completed = 0usize;
                for (index, fragment) in fragments.iter().cloned().enumerate() {
                    let datasets = reassembler.ingest_data_shred_meta(
                        11_000,
                        u32::try_from(index).unwrap_or(u32::MAX),
                        index + 1 == DATASET_BENCH_SHREDS,
                        index + 1 == DATASET_BENCH_SHREDS,
                        fragment,
                    );
                    completed = completed.saturating_add(datasets.len());
                }
                black_box(completed);
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_derived_state_dispatch(c: &mut Criterion) {
    let host = DerivedStateHost::builder()
        .add_consumer(NoopDerivedStateConsumer)
        .build();
    host.initialize();
    let event_template = derived_state_events(DERIVED_STATE_BATCH_EVENTS);
    let watermarks = FeedWatermarks {
        canonical_tip_slot: Some(22_000),
        processed_slot: Some(22_000),
        confirmed_slot: Some(21_990),
        finalized_slot: Some(21_900),
    };
    let mut group = c.benchmark_group("derived_state_dispatch");
    group.throughput(Throughput::Elements(DERIVED_STATE_BATCH_EVENTS as u64));
    group.bench_function("slot_status_batch_64", |b| {
        b.iter_batched(
            || event_template.clone(),
            |events| {
                host.on_events(watermarks, events);
                black_box(host.last_emitted_sequence());
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn relay_packets(count: usize) -> Vec<(Vec<u8>, sof::shred::wire::ParsedShredHeader)> {
    (0..count)
        .map(|index| {
            let index_u32 = u32::try_from(index).unwrap_or(u32::MAX);
            let packet = build_data_shred_packet(42, index_u32, index_u32 / 8, 1, &[7_u8; 96]);
            let header = parse_shred_header(&packet).expect("benchmark packet must parse");
            (packet, header)
        })
        .collect()
}

fn populated_shared_relay_cache(count: usize) -> SharedRelayCache {
    let cache = SharedRelayCache::new(RecentShredRingBuffer::new(16_384, Duration::from_secs(30)));
    let packets = relay_packets(count);
    let now = Instant::now();
    for (offset, (packet, parsed)) in packets.iter().enumerate() {
        let observed_at = now + Duration::from_micros(u64::try_from(offset).unwrap_or(u64::MAX));
        let outcome = cache.insert(packet, parsed, observed_at);
        debug_assert!(outcome.inserted || outcome.replaced);
    }
    cache
}

fn dataset_fragments(count: usize) -> Vec<SharedPayloadFragment> {
    (0..count)
        .map(|index| {
            SharedPayloadFragment::owned(vec![u8::try_from(index).unwrap_or(u8::MAX); 128])
        })
        .collect()
}

fn derived_state_events(count: usize) -> Vec<DerivedStateFeedEvent> {
    (0..count)
        .map(|index| {
            DerivedStateFeedEvent::SlotStatusChanged(SlotStatusChangedEvent {
                slot: 22_000 + u64::try_from(index).unwrap_or(u64::MAX),
                parent_slot: Some(21_999 + u64::try_from(index).unwrap_or(u64::MAX)),
                previous_status: Some(ForkSlotStatus::Processed),
                status: ForkSlotStatus::Confirmed,
            })
        })
        .collect()
}

fn build_data_shred_packet(
    slot: u64,
    index: u32,
    fec_set_index: u32,
    parent_offset: u16,
    payload: &[u8],
) -> Vec<u8> {
    let total = SIZE_OF_DATA_SHRED_HEADERS.saturating_add(payload.len());
    let size = u16::try_from(total).expect("benchmark packet too large");
    let mut packet = vec![0_u8; SIZE_OF_DATA_SHRED_PAYLOAD];

    packet[0..8].copy_from_slice(&slot.to_le_bytes());
    packet[8..12].copy_from_slice(&index.to_le_bytes());
    packet[12..16].copy_from_slice(&fec_set_index.to_le_bytes());
    packet[64] = VARIANT_MERKLE_DATA;
    packet[65..73].copy_from_slice(&slot.to_le_bytes());
    packet[73..77].copy_from_slice(&index.to_le_bytes());
    packet[77..79].copy_from_slice(&1_u16.to_le_bytes());
    packet[79..83].copy_from_slice(&fec_set_index.to_le_bytes());
    packet[83..85].copy_from_slice(&parent_offset.to_le_bytes());
    packet[85] = 0b0100_0000;
    packet[86..88].copy_from_slice(&size.to_le_bytes());
    let end = 88usize.saturating_add(payload.len());
    packet[88..end].copy_from_slice(payload);
    packet
}

#[derive(Debug, Default)]
struct NoopDerivedStateConsumer;

impl DerivedStateConsumer for NoopDerivedStateConsumer {
    fn name(&self) -> &'static str {
        "noop-derived-state-consumer"
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn extension_version(&self) -> &'static str {
        "bench"
    }

    fn load_checkpoint(
        &mut self,
    ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
        Ok(None)
    }

    fn config(&self) -> DerivedStateConsumerConfig {
        DerivedStateConsumerConfig::new().with_control_plane_observed()
    }

    fn setup(
        &mut self,
        _ctx: DerivedStateConsumerContext,
    ) -> Result<(), DerivedStateConsumerSetupError> {
        Ok(())
    }

    fn apply(
        &mut self,
        envelope: &DerivedStateFeedEnvelope,
    ) -> Result<(), DerivedStateConsumerFault> {
        black_box(envelope.sequence);
        Ok(())
    }

    fn flush_checkpoint(
        &mut self,
        checkpoint: DerivedStateCheckpoint,
    ) -> Result<(), DerivedStateConsumerFault> {
        black_box(checkpoint.last_applied_sequence);
        Ok(())
    }
}

criterion_group!(
    hot_paths,
    bench_shared_relay_cache_insert,
    bench_shared_relay_cache_query_range,
    bench_dataset_reassembly,
    bench_derived_state_dispatch
);
criterion_main!(hot_paths);
