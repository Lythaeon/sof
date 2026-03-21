//! Criterion benches for public SOF hot paths.

use std::ops::Range;
use std::time::{Duration, Instant};

use criterion::{BatchSize, Criterion, Throughput, black_box};
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

/// Relay-cache insert workload size.
const RELAY_BENCH_SHREDS: usize = 512;
/// Relay-cache lookup fixture size.
const RELAY_QUERY_SHREDS: usize = 1024;
/// Relay range query start index.
const RELAY_QUERY_START_INDEX: u32 = 320;
/// Relay range query end index.
const RELAY_QUERY_END_INDEX: u32 = 383;
/// Dataset reassembly contiguous-shred workload size.
const DATASET_BENCH_SHREDS: usize = 32;
/// Derived-state dispatch batch size.
const DERIVED_STATE_BATCH_EVENTS: usize = 64;

/// Bench relay-cache insert throughput on a bounded shared cache.
fn bench_shared_relay_cache_insert(c: &mut Criterion) {
    let Some(packets) = relay_packets(RELAY_BENCH_SHREDS) else {
        return;
    };
    let mut group = c.benchmark_group("relay_cache_insert");
    group.throughput(Throughput::Elements(RELAY_BENCH_SHREDS as u64));
    group.bench_function("shared_cache_insert_512", |b| {
        b.iter_batched(
            || SharedRelayCache::new(RecentShredRingBuffer::new(16_384, Duration::from_secs(30))),
            |cache| {
                let now = Instant::now();
                for (offset, (packet, parsed)) in packets.iter().enumerate() {
                    let observed_at = observed_at(now, offset);
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

/// Bench bounded relay-cache range-query throughput.
fn bench_shared_relay_cache_query_range(c: &mut Criterion) {
    let Some(cache) = populated_shared_relay_cache(RELAY_QUERY_SHREDS) else {
        return;
    };
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
            let response_len = cache
                .query_range(request, limits, Instant::now())
                .map_or(0, |response| response.len());
            black_box(response_len);
        });
    });
    group.finish();
}

/// Bench contiguous dataset reassembly with bounded in-slot fragments.
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
                    let is_last = index.saturating_add(1) == DATASET_BENCH_SHREDS;
                    let datasets = reassembler.ingest_data_shred_meta(
                        11_000,
                        u32::try_from(index).unwrap_or(u32::MAX),
                        is_last,
                        is_last,
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

/// Bench the full derived-state host dispatch path for one control-plane batch.
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

/// Build parsed benchmark shred packets for relay-cache benches.
fn relay_packets(count: usize) -> Option<Vec<(Vec<u8>, sof::shred::wire::ParsedShredHeader)>> {
    (0..count)
        .map(|index| {
            let index_u32 = u32::try_from(index).unwrap_or(u32::MAX);
            let packet = build_data_shred_packet(42, index_u32, index_u32 / 8, 1, &[7_u8; 96])?;
            let header = parse_shred_header(&packet).ok()?;
            Some((packet, header))
        })
        .collect()
}

/// Build and prefill a bounded relay cache for lookup benches.
fn populated_shared_relay_cache(count: usize) -> Option<SharedRelayCache> {
    let cache = SharedRelayCache::new(RecentShredRingBuffer::new(16_384, Duration::from_secs(30)));
    let packets = relay_packets(count)?;
    let now = Instant::now();
    for (offset, (packet, parsed)) in packets.iter().enumerate() {
        let observed_at = observed_at(now, offset);
        let outcome = cache.insert(packet, parsed, observed_at);
        debug_assert!(outcome.inserted || outcome.replaced);
    }
    Some(cache)
}

/// Build owned payload fragments for dataset-reassembly benches.
fn dataset_fragments(count: usize) -> Vec<SharedPayloadFragment> {
    (0..count)
        .map(|index| {
            SharedPayloadFragment::owned(vec![u8::try_from(index).unwrap_or(u8::MAX); 128])
        })
        .collect()
}

/// Build slot-status events for derived-state dispatch benches.
fn derived_state_events(count: usize) -> Vec<DerivedStateFeedEvent> {
    (0..count)
        .map(|index| {
            DerivedStateFeedEvent::SlotStatusChanged(SlotStatusChangedEvent {
                slot: bench_slot(22_000, index),
                parent_slot: Some(bench_slot(21_999, index)),
                previous_status: Some(ForkSlotStatus::Processed),
                status: ForkSlotStatus::Confirmed,
            })
        })
        .collect()
}

/// Build one minimal parseable data shred packet for relay-cache benches.
fn build_data_shred_packet(
    slot: u64,
    index: u32,
    fec_set_index: u32,
    parent_offset: u16,
    payload: &[u8],
) -> Option<Vec<u8>> {
    let total = SIZE_OF_DATA_SHRED_HEADERS.saturating_add(payload.len());
    let size = u16::try_from(total).ok()?;
    let mut packet = vec![0_u8; SIZE_OF_DATA_SHRED_PAYLOAD];

    copy_into(&mut packet, 0..8, &slot.to_le_bytes())?;
    copy_into(&mut packet, 8..12, &index.to_le_bytes())?;
    copy_into(&mut packet, 12..16, &fec_set_index.to_le_bytes())?;
    set_byte(&mut packet, 64, VARIANT_MERKLE_DATA)?;
    copy_into(&mut packet, 65..73, &slot.to_le_bytes())?;
    copy_into(&mut packet, 73..77, &index.to_le_bytes())?;
    copy_into(&mut packet, 77..79, &1_u16.to_le_bytes())?;
    copy_into(&mut packet, 79..83, &fec_set_index.to_le_bytes())?;
    copy_into(&mut packet, 83..85, &parent_offset.to_le_bytes())?;
    set_byte(&mut packet, 85, 0b0100_0000)?;
    copy_into(&mut packet, 86..88, &size.to_le_bytes())?;
    let end = 88usize.saturating_add(payload.len());
    copy_into(&mut packet, 88..end, payload)?;
    Some(packet)
}

/// Convert an event index into a saturating benchmark slot.
fn bench_slot(base: u64, index: usize) -> u64 {
    base.saturating_add(u64::try_from(index).unwrap_or(u64::MAX))
}

/// Convert an offset into a saturating observed-at timestamp.
fn observed_at(now: Instant, offset: usize) -> Instant {
    let micros = u64::try_from(offset).unwrap_or(u64::MAX);
    now.checked_add(Duration::from_micros(micros))
        .unwrap_or(now)
}

/// Copy bytes into a checked range inside the packet fixture.
fn copy_into(packet: &mut [u8], range: Range<usize>, bytes: &[u8]) -> Option<()> {
    packet.get_mut(range).map(|dst| {
        dst.copy_from_slice(bytes);
    })
}

/// Set one checked byte inside the packet fixture.
fn set_byte(packet: &mut [u8], index: usize, value: u8) -> Option<()> {
    packet.get_mut(index).map(|slot| {
        *slot = value;
    })
}

/// No-op derived-state consumer used to benchmark host dispatch overhead.
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

/// Run the SOF hot-path Criterion benchmark suite.
fn main() {
    let mut criterion = Criterion::default().configure_from_args();
    bench_shared_relay_cache_insert(&mut criterion);
    bench_shared_relay_cache_query_range(&mut criterion);
    bench_dataset_reassembly(&mut criterion);
    bench_derived_state_dispatch(&mut criterion);
    criterion.final_summary();
}
