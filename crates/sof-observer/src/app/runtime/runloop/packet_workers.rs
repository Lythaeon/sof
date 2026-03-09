use super::*;

#[derive(Debug)]
pub(super) struct PacketWorkerInput {
    pub(super) source: SocketAddr,
    pub(super) packet_bytes: Arc<[u8]>,
    pub(super) parsed_header: ParsedShredHeader,
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
    pub(super) slot: u64,
    pub(super) index: u32,
    pub(super) fec_set_index: u32,
    pub(super) kind: WorkerAcceptedShredKind,
    pub(super) payload_fragment: Option<crate::reassembly::dataset::SharedPayloadFragment>,
}

#[derive(Debug)]
pub(super) struct PacketWorkerBatchResult {
    pub(super) worker_index: usize,
    pub(super) reusable_packets: Vec<PacketWorkerInput>,
    pub(super) accepted_shreds: Vec<WorkerAcceptedShred>,
    #[cfg(feature = "gossip-bootstrap")]
    pub(super) leader_diff: crate::verify::SlotLeaderDiff,
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

#[derive(Default)]
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
    pub(super) fn update(&self, pubkeys: Vec<[u8; 32]>) {
        let mut shared_pubkeys = self.pubkeys.clone();
        shared_pubkeys.update(Arc::new(pubkeys));
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> (u64, Arc<Vec<[u8; 32]>>) {
        let generation = self.generation.load(Ordering::Relaxed);
        let pubkeys = self.pubkeys.shared_get();
        let pubkeys = Arc::clone(&pubkeys);
        (generation, pubkeys)
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
        let runtime_handle = tokio::runtime::Handle::current();
        #[cfg(feature = "gossip-bootstrap")]
        let known_pubkeys = SharedKnownPubkeys::default();
        let queue_depth = Arc::new(AtomicU64::new(0));
        let max_queue_depth = Arc::new(AtomicU64::new(0));
        let mut senders = Vec::with_capacity(worker_count);
        let mut worker_handles = Vec::with_capacity(worker_count);
        let mut telemetry = Vec::with_capacity(worker_count);

        for _worker_id in 0..worker_count {
            let (worker_tx, mut worker_rx) = mpsc::channel::<PacketWorkerBatch>(sender_capacity);
            let worker_result_tx = result_tx.clone();
            let worker_runtime_handle = runtime_handle.clone();
            #[cfg(feature = "gossip-bootstrap")]
            let worker_known_pubkeys = known_pubkeys.clone();
            let worker_telemetry = PacketWorkerTelemetry::new();
            let worker_telemetry_state = worker_telemetry.clone();
            let worker_queue_depth = Arc::clone(&queue_depth);
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
                    let maybe_batch = worker_runtime_handle.block_on(worker_rx.recv());
                    let Some(batch) = maybe_batch else {
                        break;
                    };
                    let packet_count = u64::try_from(batch.packets.len()).unwrap_or(u64::MAX);
                    let depth_after = saturating_sub_atomic(&worker_queue_depth, packet_count);
                    let worker_depth_after = worker_telemetry_state
                        .queue_depth()
                        .saturating_sub(packet_count);
                    worker_telemetry_state.set_queue_depth(worker_depth_after);
                    crate::runtime_metrics::set_packet_worker_queue_depth(depth_after);
                    #[cfg(feature = "gossip-bootstrap")]
                    refresh_known_pubkeys(
                        &worker_known_pubkeys,
                        &mut verifier_generation,
                        shred_verifier.as_mut(),
                    );
                    let result = process_packet_batch(
                        batch,
                        shred_verifier.as_mut(),
                        verify_recovered_shreds,
                        verify_strict_unknown,
                        &mut fec_recoverer,
                    );
                    worker_telemetry_state.set_tracked_fec_sets(fec_recoverer.tracked_sets());
                    if worker_result_tx.blocking_send(result).is_err() {
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
                crate::runtime_metrics::set_packet_worker_queue_depth(depth_after);
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
                crate::runtime_metrics::observe_packet_worker_max_queue_depth(
                    self.max_queue_depth.load(Ordering::Relaxed),
                );
                DispatchWorkerBatchOutcome::Enqueued
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(batch)) => {
                crate::runtime_metrics::observe_packet_worker_queue_drops(1, packet_count);
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

    pub(super) fn close_inputs(&mut self) {
        self.senders.clear();
    }

    #[cfg(feature = "gossip-bootstrap")]
    pub(super) fn update_known_pubkeys(&self, pubkeys: Vec<[u8; 32]>) {
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
        for handle in std::mem::take(&mut self.worker_handles) {
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
    shred_verifier.set_known_pubkeys(pubkeys.as_ref().clone());
    *verifier_generation = generation;
}

fn process_packet_batch(
    batch: PacketWorkerBatch,
    mut shred_verifier: Option<&mut ShredVerifier>,
    verify_recovered_shreds: bool,
    verify_strict_unknown: bool,
    fec_recoverer: &mut FecRecoverer,
) -> PacketWorkerBatchResult {
    let PacketWorkerBatch {
        worker_index,
        mut packets,
    } = batch;
    let mut accepted_shreds = Vec::new();
    #[cfg(feature = "gossip-bootstrap")]
    let mut observed_slot_leaders = HashMap::<u64, [u8; 32]>::new();
    let mut verify_counters = WorkerVerifyCounters::default();

    for packet in packets.drain(..) {
        let observed_at = Instant::now();
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
        if !accepted {
            continue;
        }
        #[cfg(feature = "gossip-bootstrap")]
        maybe_record_observed_leader(
            shred_verifier.as_deref(),
            parsed_header_slot(&packet.parsed_header),
            &mut observed_slot_leaders,
        );
        let recovered_packets = fec_recoverer.ingest_packet(packet.packet_bytes.as_ref());
        push_primary_shred(packet, &mut accepted_shreds);

        for recovered in recovered_packets {
            let parsed_recovered = match parse_shred(&recovered) {
                Ok(parsed) => parsed,
                Err(_) => continue,
            };
            if verify_recovered_shreds {
                let recovered_accepted = match verify_packet_with_counters(
                    shred_verifier.as_deref_mut(),
                    &recovered,
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
            if let ParsedShred::Data(data) = parsed_recovered {
                #[cfg(feature = "gossip-bootstrap")]
                maybe_record_observed_leader(
                    shred_verifier.as_deref(),
                    data.common.slot,
                    &mut observed_slot_leaders,
                );
                accepted_shreds.push(WorkerAcceptedShred {
                    source: None,
                    slot: data.common.slot,
                    index: data.common.index,
                    fec_set_index: data.common.fec_set_index,
                    kind: WorkerAcceptedShredKind::RecoveredData {
                        parent_slot: derive_parent_slot(
                            data.common.slot,
                            data.data_header.parent_offset,
                        ),
                        data_complete: data.data_header.data_complete(),
                        last_in_slot: data.data_header.last_in_slot(),
                        reference_tick: data.data_header.reference_tick(),
                    },
                    payload_fragment: Some(
                        crate::reassembly::dataset::SharedPayloadFragment::owned(data.payload),
                    ),
                });
            }
        }
    }

    #[cfg(feature = "gossip-bootstrap")]
    let leader_diff = shred_verifier
        .map_or_else(crate::verify::SlotLeaderDiff::default, |verifier| {
            verifier.take_slot_leader_diff()
        });

    PacketWorkerBatchResult {
        worker_index,
        reusable_packets: packets,
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
    }
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
    match packet.parsed_header {
        ParsedShredHeader::Data(data) => {
            let payload_fragment = crate::reassembly::dataset::SharedPayloadFragment::borrowed(
                Arc::clone(&packet.packet_bytes),
                data.payload_offset,
                data.payload_len,
            );
            accepted_shreds.push(WorkerAcceptedShred {
                source: Some(packet.source),
                slot: data.common.slot,
                index: data.common.index,
                fec_set_index: data.common.fec_set_index,
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
                slot: code.common.slot,
                index: code.common.index,
                fec_set_index: code.common.fec_set_index,
                kind: WorkerAcceptedShredKind::Code {
                    num_data_shreds: code.coding_header.num_data_shreds,
                },
                payload_fragment: None,
            });
        }
    }
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
    let verify_status = shred_verifier.verify_packet(packet, observed_at);
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
    use crate::{
        protocol::shred_wire::{SIZE_OF_DATA_SHRED_PAYLOAD, VARIANT_MERKLE_DATA},
        shred::wire::{SIZE_OF_DATA_SHRED_HEADERS, parse_shred_header},
    };

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

        let drained = tokio::time::timeout(Duration::from_secs(1), pool.recv())
            .await
            .expect("worker shutdown should complete before timeout");
        assert!(drained.is_none());

        pool.shutdown().await;
    }
}
