#![allow(clippy::missing_docs_in_private_items)]

use super::*;
use crate::reassembly::dataset::SharedPayloadFragment;
use crate::runtime_metrics::{
    observe_packet_worker_max_queue_depth, observe_packet_worker_queue_drops,
    set_packet_worker_queue_depth,
};
#[cfg(feature = "gossip-bootstrap")]
use crate::verify::SlotLeaderDiff;
use std::{mem, sync::atomic::AtomicBool};

#[derive(Debug)]
pub(super) struct PacketWorkerInput {
    pub(super) source: SocketAddr,
    pub(super) packet_bytes: Arc<[u8]>,
    pub(super) parsed_header: ParsedShredHeader,
    pub(super) observed_at: Instant,
}

#[derive(Debug)]
struct PacketWorkerBatch {
    worker_index: usize,
    packets: Vec<PacketWorkerInput>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum WorkerAcceptedShredKind {
    Data {
        parent_slot: Option<u64>,
        data_complete: bool,
        last_in_slot: bool,
        reference_tick: u8,
    },
    Code {
        num_data_shreds: u16,
    },
    RecoveredData {
        parent_slot: Option<u64>,
        data_complete: bool,
        last_in_slot: bool,
        reference_tick: u8,
    },
}

#[derive(Debug)]
pub(super) struct WorkerAcceptedShred {
    pub(super) source: Option<SocketAddr>,
    pub(super) observed_at: Instant,
    pub(super) slot: u64,
    pub(super) index: u32,
    pub(super) fec_set_index: u32,
    pub(super) version: u16,
    pub(super) variant: u8,
    pub(super) signature: [u8; 64],
    pub(super) kind: WorkerAcceptedShredKind,
    pub(super) payload_fragment: Option<SharedPayloadFragment>,
}

#[derive(Debug)]
pub(super) struct PacketWorkerBatchResult {
    pub(super) worker_index: usize,
    pub(super) reusable_packets: Vec<PacketWorkerInput>,
    pub(super) accepted_shreds: Vec<WorkerAcceptedShred>,
    #[cfg(feature = "gossip-bootstrap")]
    pub(super) leader_diff: SlotLeaderDiff,
    #[cfg(feature = "gossip-bootstrap")]
    pub(super) observed_slot_leaders: Vec<(u64, [u8; 32])>,
    pub(super) verify_verified_count: u64,
    pub(super) verify_unknown_leader_count: u64,
    pub(super) verify_invalid_merkle_count: u64,
    pub(super) verify_invalid_signature_count: u64,
    pub(super) verify_malformed_count: u64,
    pub(super) verify_dropped_count: u64,
}

pub(super) enum DispatchWorkerBatchOutcome {
    Enqueued,
    Dropped(Vec<PacketWorkerInput>),
    Closed(Vec<PacketWorkerInput>),
}

#[derive(Clone, Copy)]
pub(super) struct PacketWorkerPoolConfig {
    pub(super) workers: usize,
    pub(super) queue_capacity: usize,
    pub(super) verify_enabled: bool,
    pub(super) verify_recovered_shreds: bool,
    pub(super) verify_strict_unknown: bool,
    pub(super) verify_signature_cache_entries: usize,
    pub(super) verify_slot_leader_window: u64,
    pub(super) verify_unknown_retry: Duration,
    pub(super) fec_max_tracked_sets: usize,
    pub(super) fec_retained_slot_lag: u64,
}

#[derive(Clone, Copy, Default)]
struct WorkerVerifyCounters {
    verified: u64,
    unknown_leader: u64,
    invalid_merkle: u64,
    invalid_signature: u64,
    malformed: u64,
    dropped: u64,
}

impl WorkerVerifyCounters {
    const fn observe(&mut self, verify_status: VerifyStatus) {
        match verify_status {
            VerifyStatus::Verified => {
                self.verified = self.verified.saturating_add(1);
            }
            VerifyStatus::UnknownLeader => {
                self.unknown_leader = self.unknown_leader.saturating_add(1);
            }
            VerifyStatus::InvalidMerkle => {
                self.invalid_merkle = self.invalid_merkle.saturating_add(1);
            }
            VerifyStatus::InvalidSignature => {
                self.invalid_signature = self.invalid_signature.saturating_add(1);
            }
            VerifyStatus::Malformed => {
                self.malformed = self.malformed.saturating_add(1);
            }
        }
    }

    const fn merge_from(&mut self, other: &Self) {
        self.verified = self.verified.saturating_add(other.verified);
        self.unknown_leader = self.unknown_leader.saturating_add(other.unknown_leader);
        self.invalid_merkle = self.invalid_merkle.saturating_add(other.invalid_merkle);
        self.invalid_signature = self
            .invalid_signature
            .saturating_add(other.invalid_signature);
        self.malformed = self.malformed.saturating_add(other.malformed);
        self.dropped = self.dropped.saturating_add(other.dropped);
    }
}

#[derive(Clone, Default)]
#[cfg(feature = "gossip-bootstrap")]
pub(super) struct SharedKnownPubkeys {
    generation: Arc<AtomicU64>,
    pubkeys: ArcShift<Arc<Vec<[u8; 32]>>>,
}

#[cfg(feature = "gossip-bootstrap")]
impl SharedKnownPubkeys {
    #[cfg(feature = "gossip-bootstrap")]
    pub(super) fn update(&self, pubkeys: &[[u8; 32]]) {
        let current = self.pubkeys.shared_get();
        if current.as_slice() == pubkeys {
            return;
        }

        let mut normalized = pubkeys.to_vec();
        normalized.sort_unstable();
        normalized.dedup();
        if current.as_slice() == normalized.as_slice() {
            return;
        }

        let mut shared_pubkeys = self.pubkeys.clone();
        shared_pubkeys.update(Arc::new(normalized));
        self.generation.fetch_add(1, Ordering::Release);
    }

    fn snapshot(&self) -> (u64, Arc<Vec<[u8; 32]>>) {
        loop {
            let generation_before = self.generation.load(Ordering::Acquire);
            let pubkeys = self.pubkeys.shared_get();
            let generation_after = self.generation.load(Ordering::Acquire);
            if generation_before == generation_after {
                return (generation_after, Arc::clone(&pubkeys));
            }
        }
    }
}

#[derive(Clone)]
struct PacketWorkerTelemetry {
    tracked_fec_sets: Arc<AtomicU64>,
    queue_depth: Arc<AtomicU64>,
}

impl PacketWorkerTelemetry {
    fn new() -> Self {
        Self {
            tracked_fec_sets: Arc::new(AtomicU64::new(0)),
            queue_depth: Arc::new(AtomicU64::new(0)),
        }
    }

    fn set_tracked_fec_sets(&self, tracked_fec_sets: usize) {
        self.tracked_fec_sets.store(
            u64::try_from(tracked_fec_sets).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    fn tracked_fec_sets(&self) -> u64 {
        self.tracked_fec_sets.load(Ordering::Relaxed)
    }

    fn set_queue_depth(&self, queue_depth: u64) {
        self.queue_depth.store(queue_depth, Ordering::Relaxed);
    }

    fn queue_depth(&self) -> u64 {
        self.queue_depth.load(Ordering::Relaxed)
    }
}

pub(super) struct PacketWorkerPool {
    senders: Vec<mpsc::Sender<PacketWorkerBatch>>,
    result_rx: mpsc::Receiver<PacketWorkerBatchResult>,
    worker_handles: Vec<JoinHandle<()>>,
    #[cfg(feature = "gossip-bootstrap")]
    known_pubkeys: SharedKnownPubkeys,
    verify_strict_unknown: Arc<AtomicBool>,
    telemetry: Vec<PacketWorkerTelemetry>,
    queue_depth: Arc<AtomicU64>,
    max_queue_depth: Arc<AtomicU64>,
}

impl PacketWorkerPool {
    pub(super) fn new(config: PacketWorkerPoolConfig) -> Self {
        let PacketWorkerPoolConfig {
            workers,
            queue_capacity,
            verify_enabled,
            verify_recovered_shreds,
            verify_strict_unknown,
            verify_signature_cache_entries,
            verify_slot_leader_window,
            verify_unknown_retry,
            fec_max_tracked_sets,
            fec_retained_slot_lag,
        } = config;
        let worker_count = workers.max(1);
        let sender_capacity = queue_capacity.max(1);
        let (result_tx, result_rx) =
            mpsc::channel::<PacketWorkerBatchResult>(worker_count.saturating_mul(sender_capacity));
        #[cfg(feature = "gossip-bootstrap")]
        let known_pubkeys = SharedKnownPubkeys::default();
        let verify_strict_unknown = Arc::new(AtomicBool::new(verify_strict_unknown));
        let queue_depth = Arc::new(AtomicU64::new(0));
        let max_queue_depth = Arc::new(AtomicU64::new(0));
        let mut senders = Vec::with_capacity(worker_count);
        let mut worker_handles = Vec::with_capacity(worker_count);
        let mut telemetry = Vec::with_capacity(worker_count);

        for _worker_id in 0..worker_count {
            let (worker_tx, mut worker_rx) = mpsc::channel::<PacketWorkerBatch>(sender_capacity);
            let worker_result_tx = result_tx.clone();
            #[cfg(feature = "gossip-bootstrap")]
            let worker_known_pubkeys = known_pubkeys.clone();
            let worker_telemetry = PacketWorkerTelemetry::new();
            let worker_telemetry_state = worker_telemetry.clone();
            let worker_queue_depth = Arc::clone(&queue_depth);
            let worker_verify_strict_unknown = Arc::clone(&verify_strict_unknown);
            let worker_handle = tokio::task::spawn_blocking(move || {
                let mut shred_verifier = verify_enabled.then(|| {
                    ShredVerifier::new(
                        verify_signature_cache_entries,
                        verify_slot_leader_window,
                        verify_unknown_retry,
                    )
                });
                #[cfg(feature = "gossip-bootstrap")]
                let mut verifier_generation: u64 = u64::MAX;
                let mut fec_recoverer =
                    FecRecoverer::new(fec_max_tracked_sets, fec_retained_slot_lag);

                loop {
                    let maybe_batch = worker_rx.blocking_recv();
                    let Some(batch) = maybe_batch else {
                        break;
                    };
                    let packet_count = u64::try_from(batch.packets.len()).unwrap_or(u64::MAX);
                    let depth_after = saturating_sub_atomic(&worker_queue_depth, packet_count);
                    let worker_depth_after = worker_telemetry_state
                        .queue_depth()
                        .saturating_sub(packet_count);
                    worker_telemetry_state.set_queue_depth(worker_depth_after);
                    set_packet_worker_queue_depth(depth_after);
                    #[cfg(feature = "gossip-bootstrap")]
                    refresh_known_pubkeys(
                        &worker_known_pubkeys,
                        &mut verifier_generation,
                        shred_verifier.as_mut(),
                    );
                    let strict_unknown = worker_verify_strict_unknown.load(Ordering::Relaxed);
                    let forwarded_all_results = process_packet_batch_streaming(
                        batch,
                        shred_verifier.as_mut(),
                        verify_recovered_shreds,
                        strict_unknown,
                        &mut fec_recoverer,
                        |result| worker_result_tx.blocking_send(result).is_ok(),
                    );
                    worker_telemetry_state.set_tracked_fec_sets(fec_recoverer.tracked_sets());
                    if !forwarded_all_results {
                        break;
                    }
                }
            });
            senders.push(worker_tx);
            worker_handles.push(worker_handle);
            telemetry.push(worker_telemetry);
        }

        Self {
            senders,
            result_rx,
            worker_handles,
            #[cfg(feature = "gossip-bootstrap")]
            known_pubkeys,
            verify_strict_unknown,
            telemetry,
            queue_depth,
            max_queue_depth,
        }
    }

    pub(super) fn worker_count(&self) -> usize {
        self.senders.len().max(1)
    }

    pub(super) fn dispatch_worker_batch(
        &self,
        worker_index: usize,
        packets: Vec<PacketWorkerInput>,
    ) -> DispatchWorkerBatchOutcome {
        if packets.is_empty() {
            return DispatchWorkerBatchOutcome::Enqueued;
        }
        let Some(sender) = self.senders.get(worker_index) else {
            return DispatchWorkerBatchOutcome::Closed(packets);
        };
        let packet_count = u64::try_from(packets.len()).unwrap_or(u64::MAX);
        match sender.try_send(PacketWorkerBatch {
            worker_index,
            packets,
        }) {
            Ok(()) => {
                let worker_depth_after = sender.max_capacity().saturating_sub(sender.capacity());
                let depth_after = self
                    .queue_depth
                    .fetch_add(packet_count, Ordering::Relaxed)
                    .saturating_add(packet_count);
                if let Some(worker_telemetry) = self.telemetry.get(worker_index) {
                    let worker_queue_depth = worker_telemetry
                        .queue_depth()
                        .saturating_add(packet_count)
                        .max(u64::try_from(worker_depth_after).unwrap_or(u64::MAX));
                    worker_telemetry.set_queue_depth(worker_queue_depth);
                }
                set_packet_worker_queue_depth(depth_after);
                let mut current_max = self.max_queue_depth.load(Ordering::Relaxed);
                while depth_after > current_max {
                    match self.max_queue_depth.compare_exchange_weak(
                        current_max,
                        depth_after,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(observed) => current_max = observed,
                    }
                }
                observe_packet_worker_max_queue_depth(self.max_queue_depth.load(Ordering::Relaxed));
                DispatchWorkerBatchOutcome::Enqueued
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(batch)) => {
                observe_packet_worker_queue_drops(1, packet_count);
                DispatchWorkerBatchOutcome::Dropped(batch.packets)
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(batch)) => {
                DispatchWorkerBatchOutcome::Closed(batch.packets)
            }
        }
    }

    pub(super) async fn recv(&mut self) -> Option<PacketWorkerBatchResult> {
        self.result_rx.recv().await
    }

    #[cfg(feature = "gossip-bootstrap")]
    pub(super) fn try_recv(
        &mut self,
    ) -> Result<PacketWorkerBatchResult, tokio::sync::mpsc::error::TryRecvError> {
        self.result_rx.try_recv()
    }

    pub(super) fn close_inputs(&mut self) {
        self.senders.clear();
    }

    pub(super) fn set_verify_strict_unknown(&self, enabled: bool) {
        self.verify_strict_unknown.store(enabled, Ordering::Relaxed);
    }

    #[cfg(feature = "gossip-bootstrap")]
    pub(super) fn update_known_pubkeys(&self, pubkeys: &[[u8; 32]]) {
        self.known_pubkeys.update(pubkeys);
    }

    pub(super) fn tracked_fec_sets(&self) -> u64 {
        self.telemetry
            .iter()
            .map(PacketWorkerTelemetry::tracked_fec_sets)
            .sum()
    }

    pub(super) fn queue_depth(&self) -> u64 {
        self.queue_depth.load(Ordering::Relaxed)
    }

    pub(super) fn max_queue_depth(&self) -> u64 {
        self.max_queue_depth.load(Ordering::Relaxed)
    }

    pub(super) fn worker_queue_depths(&self) -> Vec<u64> {
        self.telemetry
            .iter()
            .map(PacketWorkerTelemetry::queue_depth)
            .collect()
    }

    /// Fills `out` with one queue-depth sample per worker without reallocating caller storage.
    pub(super) fn fill_worker_queue_depths(&self, out: &mut Vec<u64>) {
        out.clear();
        out.extend(
            self.telemetry
                .iter()
                .map(PacketWorkerTelemetry::queue_depth),
        );
    }

    pub(super) fn tracked_fec_sets_by_worker(&self) -> Vec<u64> {
        self.telemetry
            .iter()
            .map(PacketWorkerTelemetry::tracked_fec_sets)
            .collect()
    }

    /// Fills `out` with one tracked-FEC-set sample per worker without reallocating caller storage.
    pub(super) fn fill_tracked_fec_sets_by_worker(&self, out: &mut Vec<u64>) {
        out.clear();
        out.extend(
            self.telemetry
                .iter()
                .map(PacketWorkerTelemetry::tracked_fec_sets),
        );
    }

    pub(super) async fn shutdown(&mut self) {
        self.close_inputs();
        for handle in mem::take(&mut self.worker_handles) {
            if handle.await.is_err() {
                // Worker task was already cancelled during runtime teardown.
            }
        }
    }
}

#[cfg(feature = "gossip-bootstrap")]
fn refresh_known_pubkeys(
    shared_known_pubkeys: &SharedKnownPubkeys,
    verifier_generation: &mut u64,
    shred_verifier: Option<&mut ShredVerifier>,
) {
    let Some(shred_verifier) = shred_verifier else {
        return;
    };
    let (generation, pubkeys) = shared_known_pubkeys.snapshot();
    if generation == *verifier_generation {
        return;
    }
    shred_verifier.set_known_pubkeys_sorted(pubkeys.as_slice());
    *verifier_generation = generation;
}

fn process_packet_batch_streaming<F>(
    batch: PacketWorkerBatch,
    mut shred_verifier: Option<&mut ShredVerifier>,
    verify_recovered_shreds: bool,
    verify_strict_unknown: bool,
    fec_recoverer: &mut FecRecoverer,
    mut emit_result: F,
) -> bool
where
    F: FnMut(PacketWorkerBatchResult) -> bool,
{
    let PacketWorkerBatch {
        worker_index,
        mut packets,
    } = batch;
    let mut forwarded_all_results = true;
    {
        let mut drained_packets = packets.drain(..).peekable();

        let mut pending_verify_counters = WorkerVerifyCounters::default();

        while let Some(packet) = drained_packets.next() {
            let mut accepted_shreds = Vec::new();
            #[cfg(feature = "gossip-bootstrap")]
            let mut observed_slot_leaders = HashMap::<u64, [u8; 32]>::new();
            let mut verify_counters = WorkerVerifyCounters::default();
            let observed_at = packet.observed_at;
            let accepted = match verify_packet_with_counters(
                shred_verifier.as_deref_mut(),
                packet.packet_bytes.as_ref(),
                observed_at,
                verify_strict_unknown,
                &mut verify_counters,
            ) {
                WorkerVerifyDecision::Accept => true,
                WorkerVerifyDecision::Drop => false,
            };
            if accepted {
                #[cfg(feature = "gossip-bootstrap")]
                maybe_record_observed_leader(
                    shred_verifier.as_deref(),
                    parsed_header_slot(&packet.parsed_header),
                    &mut observed_slot_leaders,
                );
                let recovered_packets =
                    fec_recoverer.ingest_packet(&packet.packet_bytes, &packet.parsed_header);
                push_primary_shred(packet, &mut accepted_shreds);

                for recovered in recovered_packets {
                    if verify_recovered_shreds {
                        let recovered_accepted = match verify_packet_with_counters(
                            shred_verifier.as_deref_mut(),
                            &recovered.bytes,
                            observed_at,
                            verify_strict_unknown,
                            &mut verify_counters,
                        ) {
                            WorkerVerifyDecision::Accept => true,
                            WorkerVerifyDecision::Drop => false,
                        };
                        if !recovered_accepted {
                            continue;
                        }
                    }
                    let Some(signature) = packet_signature_bytes(&recovered.bytes) else {
                        continue;
                    };
                    let Some(variant) = packet_variant_byte(&recovered.bytes) else {
                        continue;
                    };
                    #[cfg(feature = "gossip-bootstrap")]
                    maybe_record_observed_leader(
                        shred_verifier.as_deref(),
                        recovered.parsed.common.slot,
                        &mut observed_slot_leaders,
                    );
                    let packet_bytes: Arc<[u8]> = Arc::from(recovered.bytes);
                    let Some(payload_fragment) = SharedPayloadFragment::borrowed(
                        Arc::clone(&packet_bytes),
                        recovered.parsed.payload_offset,
                        recovered.parsed.payload_len,
                    ) else {
                        continue;
                    };
                    accepted_shreds.push(WorkerAcceptedShred {
                        source: None,
                        observed_at,
                        slot: recovered.parsed.common.slot,
                        index: recovered.parsed.common.index,
                        fec_set_index: recovered.parsed.common.fec_set_index,
                        version: recovered.parsed.common.version,
                        variant,
                        signature,
                        kind: WorkerAcceptedShredKind::RecoveredData {
                            parent_slot: derive_parent_slot(
                                recovered.parsed.common.slot,
                                recovered.parsed.data_header.parent_offset,
                            ),
                            data_complete: recovered.parsed.data_header.data_complete(),
                            last_in_slot: recovered.parsed.data_header.last_in_slot(),
                            reference_tick: recovered.parsed.data_header.reference_tick(),
                        },
                        payload_fragment: Some(payload_fragment),
                    });
                }
            }

            #[cfg(feature = "gossip-bootstrap")]
            let leader_diff = shred_verifier.as_deref_mut().map_or_else(
                SlotLeaderDiff::default,
                ShredVerifier::take_slot_leader_diff,
            );
            #[cfg(not(feature = "gossip-bootstrap"))]
            let should_emit = !accepted_shreds.is_empty() || drained_packets.peek().is_none();
            #[cfg(feature = "gossip-bootstrap")]
            let should_emit = !accepted_shreds.is_empty()
                || !observed_slot_leaders.is_empty()
                || !leader_diff.added.is_empty()
                || !leader_diff.updated.is_empty()
                || !leader_diff.removed_slots.is_empty()
                || drained_packets.peek().is_none();
            if !should_emit {
                pending_verify_counters.merge_from(&verify_counters);
                continue;
            }

            verify_counters.merge_from(&mem::take(&mut pending_verify_counters));

            if !emit_result(PacketWorkerBatchResult {
                worker_index,
                reusable_packets: Vec::new(),
                accepted_shreds,
                #[cfg(feature = "gossip-bootstrap")]
                leader_diff,
                #[cfg(feature = "gossip-bootstrap")]
                observed_slot_leaders: observed_slot_leaders.into_iter().collect(),
                verify_verified_count: verify_counters.verified,
                verify_unknown_leader_count: verify_counters.unknown_leader,
                verify_invalid_merkle_count: verify_counters.invalid_merkle,
                verify_invalid_signature_count: verify_counters.invalid_signature,
                verify_malformed_count: verify_counters.malformed,
                verify_dropped_count: verify_counters.dropped,
            }) {
                forwarded_all_results = false;
                break;
            }
        }
    }
    if !forwarded_all_results {
        return false;
    }

    emit_result(PacketWorkerBatchResult {
        worker_index,
        reusable_packets: packets,
        accepted_shreds: Vec::new(),
        #[cfg(feature = "gossip-bootstrap")]
        leader_diff: SlotLeaderDiff::default(),
        #[cfg(feature = "gossip-bootstrap")]
        observed_slot_leaders: Vec::new(),
        verify_verified_count: 0,
        verify_unknown_leader_count: 0,
        verify_invalid_merkle_count: 0,
        verify_invalid_signature_count: 0,
        verify_malformed_count: 0,
        verify_dropped_count: 0,
    })
}

#[cfg(feature = "gossip-bootstrap")]
fn maybe_record_observed_leader(
    shred_verifier: Option<&ShredVerifier>,
    slot: u64,
    observed_slot_leaders: &mut HashMap<u64, [u8; 32]>,
) {
    let Some(shred_verifier) = shred_verifier else {
        return;
    };
    if let Some(leader) = shred_verifier.slot_leader_for_slot(slot) {
        observed_slot_leaders.entry(slot).or_insert(leader);
    }
}

fn push_primary_shred(packet: PacketWorkerInput, accepted_shreds: &mut Vec<WorkerAcceptedShred>) {
    let Some(signature) = packet_signature_bytes(packet.packet_bytes.as_ref()) else {
        return;
    };
    let Some(variant) = packet_variant_byte(packet.packet_bytes.as_ref()) else {
        return;
    };
    match packet.parsed_header {
        ParsedShredHeader::Data(data) => {
            let payload_fragment = SharedPayloadFragment::borrowed(
                Arc::clone(&packet.packet_bytes),
                data.payload_offset,
                data.payload_len,
            );
            accepted_shreds.push(WorkerAcceptedShred {
                source: Some(packet.source),
                observed_at: packet.observed_at,
                slot: data.common.slot,
                index: data.common.index,
                fec_set_index: data.common.fec_set_index,
                version: data.common.version,
                variant,
                signature,
                kind: WorkerAcceptedShredKind::Data {
                    parent_slot: derive_parent_slot(
                        data.common.slot,
                        data.data_header.parent_offset,
                    ),
                    data_complete: data.data_header.data_complete(),
                    last_in_slot: data.data_header.last_in_slot(),
                    reference_tick: data.data_header.reference_tick(),
                },
                payload_fragment,
            });
        }
        ParsedShredHeader::Code(code) => {
            accepted_shreds.push(WorkerAcceptedShred {
                source: Some(packet.source),
                observed_at: packet.observed_at,
                slot: code.common.slot,
                index: code.common.index,
                fec_set_index: code.common.fec_set_index,
                version: code.common.version,
                variant,
                signature,
                kind: WorkerAcceptedShredKind::Code {
                    num_data_shreds: code.coding_header.num_data_shreds,
                },
                payload_fragment: None,
            });
        }
    }
}

fn packet_signature_bytes(packet: &[u8]) -> Option<[u8; 64]> {
    packet.get(0..64)?.try_into().ok()
}

fn packet_variant_byte(packet: &[u8]) -> Option<u8> {
    packet.get(64).copied()
}

enum WorkerVerifyDecision {
    Accept,
    Drop,
}

fn verify_packet_with_counters(
    shred_verifier: Option<&mut ShredVerifier>,
    packet: &[u8],
    observed_at: Instant,
    verify_strict_unknown: bool,
    verify_counters: &mut WorkerVerifyCounters,
) -> WorkerVerifyDecision {
    let Some(shred_verifier) = shred_verifier else {
        return WorkerVerifyDecision::Accept;
    };
    let verify_status = shred_verifier.verify_packet(packet, observed_at, verify_strict_unknown);
    verify_counters.observe(verify_status);
    if verify_status.is_accepted(verify_strict_unknown) {
        WorkerVerifyDecision::Accept
    } else {
        verify_counters.dropped = verify_counters.dropped.saturating_add(1);
        WorkerVerifyDecision::Drop
    }
}

#[cfg(feature = "gossip-bootstrap")]
const fn parsed_header_slot(parsed_header: &ParsedShredHeader) -> u64 {
    match parsed_header {
        ParsedShredHeader::Data(data) => data.common.slot,
        ParsedShredHeader::Code(code) => code.common.slot,
    }
}

fn saturating_sub_atomic(target: &AtomicU64, amount: u64) -> u64 {
    let mut current = target.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_sub(amount);
        match target.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(observed) => current = observed,
        }
    }
}

const fn derive_parent_slot(slot: u64, parent_offset: u16) -> Option<u64> {
    if parent_offset == 0 {
        return None;
    }
    slot.checked_sub(parent_offset as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "gossip-bootstrap")]
    use crate::verify::ShredVerifier;
    use crate::{
        protocol::shred_wire::{
            SIZE_OF_CODING_SHRED_HEADERS, SIZE_OF_CODING_SHRED_PAYLOAD, SIZE_OF_DATA_SHRED_PAYLOAD,
            SIZE_OF_SIGNATURE, VARIANT_MERKLE_CODE, VARIANT_MERKLE_DATA,
        },
        shred::wire::{SIZE_OF_DATA_SHRED_HEADERS, parse_shred_header},
    };
    use reed_solomon_erasure::galois_8::ReedSolomon;
    use sof_support::{bench::avg_ns_per_iteration, env_support::read_positive_usize};
    #[cfg(feature = "gossip-bootstrap")]
    use solana_keypair::Keypair;
    #[cfg(feature = "gossip-bootstrap")]
    use solana_signer::Signer;

    fn build_data_shred_packet(
        slot: u64,
        index: u32,
        fec_set_index: u32,
        parent_offset: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let total = SIZE_OF_DATA_SHRED_HEADERS.saturating_add(payload.len());
        let size = u16::try_from(total).expect("test packet too large");
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

    fn build_coding_shard_packet(
        slot: u64,
        index: u32,
        fec_set_index: u32,
        num_data_shreds: u16,
        num_coding_shreds: u16,
        position: u16,
        parity_shard: &[u8],
    ) -> Vec<u8> {
        let mut packet = vec![0_u8; SIZE_OF_CODING_SHRED_PAYLOAD];
        packet[64] = VARIANT_MERKLE_CODE;
        packet[65..73].copy_from_slice(&slot.to_le_bytes());
        packet[73..77].copy_from_slice(&index.to_le_bytes());
        packet[77..79].copy_from_slice(&1_u16.to_le_bytes());
        packet[79..83].copy_from_slice(&fec_set_index.to_le_bytes());
        packet[83..85].copy_from_slice(&num_data_shreds.to_le_bytes());
        packet[85..87].copy_from_slice(&num_coding_shreds.to_le_bytes());
        packet[87..89].copy_from_slice(&position.to_le_bytes());
        let shard_end = SIZE_OF_CODING_SHRED_HEADERS.saturating_add(parity_shard.len());
        packet[SIZE_OF_CODING_SHRED_HEADERS..shard_end].copy_from_slice(parity_shard);
        packet
    }

    fn erasure_shard_len_for_test() -> usize {
        SIZE_OF_CODING_SHRED_PAYLOAD.saturating_sub(SIZE_OF_CODING_SHRED_HEADERS + 32)
    }

    fn data_erasure_shard_for_test(packet: &[u8]) -> Vec<u8> {
        let shard_len = erasure_shard_len_for_test();
        packet[SIZE_OF_SIGNATURE..SIZE_OF_SIGNATURE + shard_len].to_vec()
    }

    #[cfg(feature = "gossip-bootstrap")]
    #[test]
    fn shared_known_pubkeys_skips_equivalent_updates() {
        let shared = SharedKnownPubkeys::default();
        let initial = [[2_u8; 32], [1_u8; 32], [2_u8; 32]];

        shared.update(&initial);
        let (first_generation, first_pubkeys) = shared.snapshot();
        assert_eq!(first_generation, 1);
        assert_eq!(first_pubkeys.as_slice(), &[[1_u8; 32], [2_u8; 32]]);

        let reordered = [[1_u8; 32], [2_u8; 32]];
        shared.update(&reordered);
        let (second_generation, second_pubkeys) = shared.snapshot();
        assert_eq!(second_generation, 1);
        assert_eq!(second_pubkeys.as_slice(), &[[1_u8; 32], [2_u8; 32]]);
    }

    #[cfg(feature = "gossip-bootstrap")]
    #[test]
    #[ignore = "profiling fixture for equivalent known-pubkey refresh churn"]
    fn shared_known_pubkeys_equivalent_refresh_profile_fixture() {
        let iterations =
            read_positive_usize("SOF_PACKET_WORKER_KNOWN_PUBKEY_PROFILE_ITERS", 200_000);
        let key_count = read_positive_usize("SOF_PACKET_WORKER_KNOWN_PUBKEY_COUNT", 64);
        let mut canonical = (0..key_count)
            .map(|_| Keypair::new().pubkey().to_bytes())
            .collect::<Vec<_>>();
        canonical.sort_unstable();

        let shared = SharedKnownPubkeys::default();
        let mut verifier = ShredVerifier::new(1024, 256, Duration::from_secs(5));
        let mut verifier_generation = 0_u64;

        shared.update(&canonical);
        refresh_known_pubkeys(&shared, &mut verifier_generation, Some(&mut verifier));

        let started_at = Instant::now();
        for _ in 0..iterations {
            shared.update(&canonical);
            refresh_known_pubkeys(&shared, &mut verifier_generation, Some(&mut verifier));
        }
        let elapsed = started_at.elapsed();
        let avg_ns = avg_ns_per_iteration(elapsed, iterations);
        println!(
            "shared_known_pubkeys_equivalent_refresh_profile_fixture iterations={} key_count={} final_generation={} elapsed_ms={} avg_ns_per_iteration={} avg_us_per_iteration={:.3}",
            iterations,
            key_count,
            verifier_generation,
            elapsed.as_millis(),
            avg_ns,
            avg_ns as f64 / 1_000.0
        );
    }

    fn build_recoverable_fec_pair(slot: u64) -> [(Arc<[u8]>, ParsedShredHeader); 2] {
        let data0 = build_data_shred_packet(slot, 0, 0, 1, &[1, 2, 3, 4]);
        let data1 = build_data_shred_packet(slot, 1, 0, 1, &[5, 6, 7, 8]);
        let mut shards = vec![
            data_erasure_shard_for_test(&data0),
            data_erasure_shard_for_test(&data1),
            vec![0_u8; erasure_shard_len_for_test()],
        ];
        ReedSolomon::new(2, 1)
            .expect("reed solomon config")
            .encode(&mut shards)
            .expect("encode parity shard");
        let code0 = build_coding_shard_packet(slot, 2, 0, 2, 1, 0, &shards[2]);
        [
            (
                Arc::<[u8]>::from(data0.clone()),
                parse_shred_header(&data0).expect("valid data shred"),
            ),
            (
                Arc::<[u8]>::from(code0.clone()),
                parse_shred_header(&code0).expect("valid coding shred"),
            ),
        ]
    }

    #[test]
    #[ignore = "profiling fixture for packet worker primary FEC ingest"]
    fn packet_worker_primary_fec_profile_fixture() {
        let iterations = read_positive_usize("SOF_PACKET_WORKER_FEC_PROFILE_ITERS", 50_000);
        let packets = (0..iterations)
            .map(|iteration| {
                let slot =
                    9_000_000_u64.saturating_add(u64::try_from(iteration).unwrap_or(u64::MAX));
                let index = u32::try_from(iteration % 32).unwrap_or(u32::MAX);
                let fec_set_index = index;
                let payload = [
                    u8::try_from(iteration & 0xff).unwrap_or_default(),
                    u8::try_from((iteration >> 8) & 0xff).unwrap_or_default(),
                    u8::try_from((iteration >> 16) & 0xff).unwrap_or_default(),
                    u8::try_from((iteration >> 24) & 0xff).unwrap_or_default(),
                ];
                let packet_bytes = build_data_shred_packet(slot, index, fec_set_index, 1, &payload);
                let parsed_header = parse_shred_header(&packet_bytes).expect("valid test shred");
                (Arc::<[u8]>::from(packet_bytes), parsed_header)
            })
            .collect::<Vec<_>>();
        let mut fec_recoverer = FecRecoverer::new(128, 1);
        let started_at = Instant::now();
        let mut emitted = 0_u64;
        for (packet_bytes, parsed_header) in packets {
            let forwarded = process_packet_batch_streaming(
                PacketWorkerBatch {
                    worker_index: 0,
                    packets: vec![PacketWorkerInput {
                        source: SocketAddr::from(([127, 0, 0, 1], 8_899)),
                        packet_bytes,
                        parsed_header,
                        observed_at: Instant::now(),
                    }],
                },
                None,
                false,
                false,
                &mut fec_recoverer,
                |_| {
                    emitted = emitted.saturating_add(1);
                    true
                },
            );
            assert!(forwarded);
        }
        let elapsed = started_at.elapsed();
        let avg_ns = avg_ns_per_iteration(elapsed, iterations);
        println!(
            "packet_worker_primary_fec_profile_fixture iterations={} emitted={} elapsed_ms={} avg_ns_per_iteration={} avg_us_per_iteration={:.3}",
            iterations,
            emitted,
            elapsed.as_millis(),
            avg_ns,
            avg_ns as f64 / 1_000.0
        );
    }

    #[test]
    #[ignore = "profiling fixture for packet worker FEC recovery"]
    fn packet_worker_recovery_fec_profile_fixture() {
        let iterations =
            read_positive_usize("SOF_PACKET_WORKER_FEC_RECOVERY_PROFILE_ITERS", 20_000);
        let batches = (0..iterations)
            .map(|iteration| {
                let slot =
                    10_000_000_u64.saturating_add(u64::try_from(iteration).unwrap_or(u64::MAX));
                build_recoverable_fec_pair(slot)
            })
            .collect::<Vec<_>>();
        let mut fec_recoverer = FecRecoverer::new(128, 1);
        let started_at = Instant::now();
        let mut emitted = 0_u64;
        let mut recovered = 0_u64;
        for [data_packet, code_packet] in batches {
            let forwarded = process_packet_batch_streaming(
                PacketWorkerBatch {
                    worker_index: 0,
                    packets: vec![
                        PacketWorkerInput {
                            source: SocketAddr::from(([127, 0, 0, 1], 8_899)),
                            packet_bytes: data_packet.0,
                            parsed_header: data_packet.1,
                            observed_at: Instant::now(),
                        },
                        PacketWorkerInput {
                            source: SocketAddr::from(([127, 0, 0, 1], 8_900)),
                            packet_bytes: code_packet.0,
                            parsed_header: code_packet.1,
                            observed_at: Instant::now(),
                        },
                    ],
                },
                None,
                false,
                false,
                &mut fec_recoverer,
                |result| {
                    emitted = emitted.saturating_add(1);
                    recovered = recovered.saturating_add(
                        result
                            .accepted_shreds
                            .iter()
                            .filter(|shred| {
                                matches!(shred.kind, WorkerAcceptedShredKind::RecoveredData { .. })
                            })
                            .count() as u64,
                    );
                    true
                },
            );
            assert!(forwarded);
        }
        let elapsed = started_at.elapsed();
        let avg_ns = avg_ns_per_iteration(elapsed, iterations);
        println!(
            "packet_worker_recovery_fec_profile_fixture iterations={} emitted={} recovered={} elapsed_ms={} avg_ns_per_iteration={} avg_us_per_iteration={:.3}",
            iterations,
            emitted,
            recovered,
            elapsed.as_millis(),
            avg_ns,
            avg_ns as f64 / 1_000.0
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn closing_inputs_still_drains_enqueued_batches() {
        let packet_bytes = build_data_shred_packet(42, 7, 7, 1, &[1, 2, 3, 4]);
        let parsed_header = parse_shred_header(&packet_bytes).expect("valid test shred");
        let mut pool = PacketWorkerPool::new(PacketWorkerPoolConfig {
            workers: 1,
            queue_capacity: 4,
            verify_enabled: false,
            verify_recovered_shreds: false,
            verify_strict_unknown: false,
            verify_signature_cache_entries: 64,
            verify_slot_leader_window: 64,
            verify_unknown_retry: Duration::from_millis(100),
            fec_max_tracked_sets: 8,
            fec_retained_slot_lag: 16,
        });

        assert!(matches!(
            pool.dispatch_worker_batch(
                0,
                vec![PacketWorkerInput {
                    source: SocketAddr::from(([127, 0, 0, 1], 8_899)),
                    packet_bytes: Arc::from(packet_bytes),
                    parsed_header,
                    observed_at: Instant::now(),
                }],
            ),
            DispatchWorkerBatchOutcome::Enqueued
        ));

        pool.close_inputs();

        let worker_result = tokio::time::timeout(Duration::from_secs(1), pool.recv())
            .await
            .expect("worker result should arrive before timeout")
            .expect("worker result should be present");
        assert_eq!(worker_result.accepted_shreds.len(), 1);
        assert_eq!(worker_result.accepted_shreds[0].slot, 42);
        assert_eq!(worker_result.accepted_shreds[0].index, 7);

        let recycle_result = tokio::time::timeout(Duration::from_secs(1), pool.recv())
            .await
            .expect("recycle result should arrive before timeout")
            .expect("recycle result should be present");
        assert!(recycle_result.accepted_shreds.is_empty());

        let drained = tokio::time::timeout(Duration::from_secs(1), pool.recv())
            .await
            .expect("worker shutdown should complete before timeout");
        assert!(drained.is_none());

        pool.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn packet_worker_emits_results_before_batch_tail_finishes() {
        let packet0 = build_data_shred_packet(42, 7, 7, 1, &[1, 2, 3, 4]);
        let packet1 = build_data_shred_packet(42, 8, 8, 1, &[5, 6, 7, 8]);
        let parsed_header0 = parse_shred_header(&packet0).expect("valid first test shred");
        let parsed_header1 = parse_shred_header(&packet1).expect("valid second test shred");
        let mut pool = PacketWorkerPool::new(PacketWorkerPoolConfig {
            workers: 1,
            queue_capacity: 4,
            verify_enabled: false,
            verify_recovered_shreds: false,
            verify_strict_unknown: false,
            verify_signature_cache_entries: 64,
            verify_slot_leader_window: 64,
            verify_unknown_retry: Duration::from_millis(100),
            fec_max_tracked_sets: 8,
            fec_retained_slot_lag: 16,
        });

        assert!(matches!(
            pool.dispatch_worker_batch(
                0,
                vec![
                    PacketWorkerInput {
                        source: SocketAddr::from(([127, 0, 0, 1], 8_899)),
                        packet_bytes: Arc::from(packet0),
                        parsed_header: parsed_header0,
                        observed_at: Instant::now(),
                    },
                    PacketWorkerInput {
                        source: SocketAddr::from(([127, 0, 0, 1], 8_900)),
                        packet_bytes: Arc::from(packet1),
                        parsed_header: parsed_header1,
                        observed_at: Instant::now(),
                    },
                ],
            ),
            DispatchWorkerBatchOutcome::Enqueued
        ));

        pool.close_inputs();

        let first_result = tokio::time::timeout(Duration::from_secs(1), pool.recv())
            .await
            .expect("first worker result should arrive before timeout")
            .expect("first worker result should be present");
        assert_eq!(first_result.accepted_shreds.len(), 1);
        assert_eq!(first_result.accepted_shreds[0].slot, 42);
        assert_eq!(first_result.accepted_shreds[0].index, 7);

        let second_result = tokio::time::timeout(Duration::from_secs(1), pool.recv())
            .await
            .expect("second worker result should arrive before timeout")
            .expect("second worker result should be present");
        assert_eq!(second_result.accepted_shreds.len(), 1);
        assert_eq!(second_result.accepted_shreds[0].slot, 42);
        assert_eq!(second_result.accepted_shreds[0].index, 8);

        let recycle_result = tokio::time::timeout(Duration::from_secs(1), pool.recv())
            .await
            .expect("recycle result should arrive before timeout")
            .expect("recycle result should be present");
        assert!(recycle_result.accepted_shreds.is_empty());

        let drained = tokio::time::timeout(Duration::from_secs(1), pool.recv())
            .await
            .expect("worker shutdown should complete before timeout");
        assert!(drained.is_none());

        pool.shutdown().await;
    }
}
