#[cfg(feature = "gossip-bootstrap")]
use super::*;
#[cfg(feature = "gossip-bootstrap")]
use crate::repair::{GossipRepairClient, RepairPeerSnapshot, ServedRepairRequest};
#[cfg(feature = "gossip-bootstrap")]
use tokio::sync::oneshot;

#[cfg(feature = "gossip-bootstrap")]
#[derive(Debug)]
pub(crate) enum RepairCommand {
    Request {
        request: MissingShredRequest,
    },
    NoteShredSources {
        sources: Vec<(SocketAddr, u16)>,
    },
    HandleResponsePing {
        packet: Arc<[u8]>,
        from_addr: SocketAddr,
    },
    HandleServeRequest {
        packet: Arc<[u8]>,
        from_addr: SocketAddr,
    },
}

#[cfg(feature = "gossip-bootstrap")]
#[derive(Debug)]
pub(crate) enum RepairOutcome {
    RequestSent {
        peer_addr: SocketAddr,
    },
    RequestNoPeer {
        request: MissingShredRequest,
    },
    RequestError {
        request: MissingShredRequest,
        error: String,
    },
    ResponsePingHandledFrom {
        source: SocketAddr,
    },
    ResponsePingError {
        source: SocketAddr,
        error: String,
    },
    ServeRequestHandled {
        source: SocketAddr,
        request: ServedRepairRequest,
    },
    ServeRequestError {
        source: SocketAddr,
        error: String,
    },
}

#[cfg(feature = "gossip-bootstrap")]
#[derive(Debug, thiserror::Error)]
pub(crate) enum RepairDriverStartError {
    #[error("failed to spawn dedicated repair thread: {source}")]
    SpawnThread { source: std::io::Error },
}

#[cfg(feature = "gossip-bootstrap")]
type RepairDriverParts = (
    mpsc::Sender<RepairCommand>,
    mpsc::Receiver<RepairOutcome>,
    ArcShift<RepairPeerSnapshot>,
    RepairDriverHandle,
);

#[cfg(feature = "gossip-bootstrap")]
#[derive(Debug)]
pub(crate) struct RepairDriverHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(feature = "gossip-bootstrap")]
impl RepairDriverHandle {
    #[expect(
        clippy::missing_const_for_fn,
        reason = "contains non-const std thread handle storage"
    )]
    fn new(shutdown_tx: oneshot::Sender<()>, join_handle: std::thread::JoinHandle<()>) -> Self {
        Self {
            shutdown_tx: Some(shutdown_tx),
            join_handle: Some(join_handle),
        }
    }

    fn abort(mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take()
            && shutdown_tx.send(()).is_err()
        {
            // Driver thread already exited.
        }
        drop(self.join_handle.take());
    }

    async fn shutdown(mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take()
            && shutdown_tx.send(()).is_err()
        {
            // Driver thread already exited.
        }
        if let Some(join_handle) = self.join_handle.take() {
            drop(
                tokio::task::spawn_blocking(move || {
                    drop(join_handle.join());
                })
                .await,
            );
        }
    }
}

#[cfg(feature = "gossip-bootstrap")]
#[derive(Debug)]
pub(crate) struct RepairSourceHintBuffer {
    counts: HashMap<SocketAddr, u16>,
    capacity: usize,
}

#[cfg(feature = "gossip-bootstrap")]
impl RepairSourceHintBuffer {
    pub(crate) fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            counts: HashMap::with_capacity(capacity),
            capacity,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_baseline(capacity: usize) -> Self {
        Self {
            counts: HashMap::new(),
            capacity: capacity.max(1),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.counts.len()
    }

    pub(crate) fn record(&mut self, source_addr: SocketAddr) -> Result<(), ()> {
        if let Some(entry) = self.counts.get_mut(&source_addr) {
            *entry = entry.saturating_add(1);
            return Ok(());
        }
        if self.counts.len() >= self.capacity {
            return Err(());
        }
        let _ = self.counts.insert(source_addr, 1);
        Ok(())
    }

    fn drain_batch(&mut self, max_batch_size: usize) -> Vec<(SocketAddr, u16)> {
        if self.counts.is_empty() {
            return Vec::new();
        }
        let mut entries: Vec<_> = self.counts.drain().collect();
        entries.sort_unstable_by(|(_, left_hits), (_, right_hits)| right_hits.cmp(left_hits));
        let max_batch_size = max_batch_size.max(1);
        if entries.len() <= max_batch_size {
            return entries;
        }
        let remaining = entries.split_off(max_batch_size);
        for (addr, hits) in remaining {
            let _ = self.counts.insert(addr, hits);
        }
        entries
    }
}

#[cfg(feature = "gossip-bootstrap")]
pub(crate) fn flush_repair_source_hints(
    source_hints: &mut RepairSourceHintBuffer,
    command_tx: Option<&mpsc::Sender<RepairCommand>>,
    batch_size: usize,
    source_hint_drops: &mut u64,
    source_hint_enqueued: &mut u64,
) {
    if source_hints.len() == 0 {
        return;
    }
    let batch = source_hints.drain_batch(batch_size);
    if batch.is_empty() {
        return;
    }
    if let Some(command_tx) = command_tx {
        let batch_len = u64::try_from(batch.len()).unwrap_or(u64::MAX);
        match command_tx.try_send(RepairCommand::NoteShredSources { sources: batch }) {
            Ok(()) => {
                *source_hint_enqueued = source_hint_enqueued.saturating_add(batch_len);
            }
            Err(_) => {
                *source_hint_drops = source_hint_drops.saturating_add(batch_len);
            }
        }
    } else {
        let batch_len = u64::try_from(batch.len()).unwrap_or(u64::MAX);
        *source_hint_drops = source_hint_drops.saturating_add(batch_len);
    }
}

#[cfg(feature = "gossip-bootstrap")]
pub(crate) fn spawn_repair_driver(
    mut repair_client: GossipRepairClient,
    relay_cache: Option<SharedRelayCache>,
) -> Result<RepairDriverParts, RepairDriverStartError> {
    let (command_tx, mut command_rx) =
        mpsc::channel::<RepairCommand>(read_repair_command_queue_capacity());
    let (result_tx, result_rx) =
        mpsc::channel::<RepairOutcome>(read_repair_result_queue_capacity());
    let peer_snapshot = repair_client.peer_snapshot_handle();
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let driver_thread = std::thread::Builder::new()
        .name(String::from("sof-repair-driver"))
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(source) => {
                    tracing::error!(
                        ?source,
                        "failed to build dedicated repair runtime"
                    );
                    return;
                }
            };
            runtime.block_on(async move {
                let mut slot_hint = 0_u64;
                let mut refresh_tick = interval(Duration::from_millis(read_repair_peer_refresh_ms()));
                refresh_tick.tick().await;
                let _ = repair_client.refresh_peer_snapshot(slot_hint);
                loop {
                    tokio::select! {
                        _ = &mut shutdown_rx => {
                            break;
                        }
                        maybe_command = command_rx.recv() => {
                            let Some(command) = maybe_command else {
                                break;
                            };
                            match command {
                                RepairCommand::Request { request } => {
                                    slot_hint = slot_hint.max(request.slot);
                                    let outcome = match repair_client
                                        .request_missing_shred(request.slot, request.index, request.kind)
                                        .await
                                    {
                                        Ok(Some(peer_addr)) => RepairOutcome::RequestSent { peer_addr },
                                        Ok(None) => RepairOutcome::RequestNoPeer { request },
                                        Err(error) => RepairOutcome::RequestError {
                                            request,
                                            error: error.to_string(),
                                        },
                                    };
                                    if result_tx.try_send(outcome).is_err() {
                                        // Receiver dropped; drop outcome.
                                    }
                                }
                                RepairCommand::NoteShredSources { sources } => {
                                    let _ = repair_client.note_shred_sources(&sources);
                                }
                                RepairCommand::HandleResponsePing { packet, from_addr } => {
                                    match repair_client
                                        .maybe_handle_response_ping(packet.as_ref(), from_addr)
                                        .await
                                    {
                                        Ok(true) => {
                                            if result_tx
                                                .try_send(RepairOutcome::ResponsePingHandledFrom {
                                                    source: from_addr,
                                                })
                                                .is_err()
                                            {
                                                // Receiver dropped; drop outcome.
                                            }
                                        }
                                        Ok(false) => {}
                                        Err(error) => {
                                            if result_tx
                                                .try_send(RepairOutcome::ResponsePingError {
                                                    source: from_addr,
                                                    error: error.to_string(),
                                                })
                                                .is_err()
                                            {
                                                // Receiver dropped; drop outcome.
                                            }
                                        }
                                    }
                                }
                                RepairCommand::HandleServeRequest { packet, from_addr } => {
                                    match repair_client
                                        .maybe_serve_repair_request(
                                            packet.as_ref(),
                                            from_addr,
                                            relay_cache.as_ref(),
                                        )
                                        .await
                                    {
                                        Ok(Some(request)) => {
                                            if result_tx
                                                .try_send(RepairOutcome::ServeRequestHandled {
                                                    source: from_addr,
                                                    request,
                                                })
                                                .is_err()
                                            {
                                                // Receiver dropped; drop outcome.
                                            }
                                        }
                                        Ok(None) => {}
                                        Err(error) => {
                                            if result_tx
                                                .try_send(RepairOutcome::ServeRequestError {
                                                    source: from_addr,
                                                    error: error.to_string(),
                                                })
                                                .is_err()
                                            {
                                                // Receiver dropped; drop outcome.
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ = refresh_tick.tick() => {
                            let _ = repair_client.refresh_peer_snapshot(slot_hint);
                        }
                    }
                }
            });
        })
        .map_err(|source| RepairDriverStartError::SpawnThread { source })?;
    let driver_handle = RepairDriverHandle::new(shutdown_tx, driver_thread);
    Ok((command_tx, result_rx, peer_snapshot, driver_handle))
}

#[cfg(all(test, feature = "gossip-bootstrap"))]
mod tests {
    use std::{
        env,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::Instant,
    };

    use super::RepairSourceHintBuffer;

    #[test]
    #[ignore = "profiling fixture for repair source hint buffer allocation"]
    fn repair_source_hint_buffer_profile_fixture() {
        let iterations = env::var("SOF_REPAIR_SOURCE_HINT_PROFILE_ITERS")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(20_000);
        let capacity = env::var("SOF_REPAIR_SOURCE_HINT_PROFILE_CAPACITY")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(256);
        let batch_size = env::var("SOF_REPAIR_SOURCE_HINT_PROFILE_BATCH")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(capacity / 2);
        let addresses = (0..capacity)
            .map(|index| {
                SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::new(
                        127,
                        0,
                        u8::try_from((index / 255) % 255).unwrap_or(0),
                        u8::try_from((index % 255) + 1).unwrap_or(u8::MAX),
                    )),
                    u16::try_from((10_000 + index) % usize::from(u16::MAX)).unwrap_or(u16::MAX),
                )
            })
            .collect::<Vec<_>>();
        assert!(!addresses.is_empty());

        let baseline_started_at = Instant::now();
        for _ in 0..iterations {
            let mut buffer = RepairSourceHintBuffer::new_baseline(capacity);
            for addr in addresses.iter().copied() {
                assert!(buffer.record(addr).is_ok());
            }
            let drained = buffer.drain_batch(batch_size);
            assert!(!drained.is_empty());
        }
        let baseline_elapsed = baseline_started_at.elapsed();

        let optimized_started_at = Instant::now();
        for _ in 0..iterations {
            let mut buffer = RepairSourceHintBuffer::new(capacity);
            for addr in addresses.iter().copied() {
                assert!(buffer.record(addr).is_ok());
            }
            let drained = buffer.drain_batch(batch_size);
            assert!(!drained.is_empty());
        }
        let optimized_elapsed = optimized_started_at.elapsed();

        let baseline_avg_ns =
            baseline_elapsed.as_nanos() / u128::try_from(iterations).unwrap_or(u128::MAX);
        let optimized_avg_ns =
            optimized_elapsed.as_nanos() / u128::try_from(iterations).unwrap_or(u128::MAX);
        let baseline_avg_us = baseline_avg_ns as f64 / 1_000.0;
        let optimized_avg_us = optimized_avg_ns as f64 / 1_000.0;
        println!(
            "repair_source_hint_buffer_profile_fixture iterations={} baseline_us={} optimized_us={} baseline_avg_ns_per_iteration={} optimized_avg_ns_per_iteration={} baseline_avg_us_per_iteration={:.6} optimized_avg_us_per_iteration={:.6}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
            baseline_avg_ns,
            optimized_avg_ns,
            baseline_avg_us,
            optimized_avg_us
        );
    }
}

#[cfg(feature = "gossip-bootstrap")]
pub(crate) fn replace_repair_driver(
    repair_client: crate::repair::GossipRepairClient,
    relay_cache: Option<SharedRelayCache>,
    repair_command_tx: &mut Option<mpsc::Sender<RepairCommand>>,
    repair_result_rx: &mut Option<mpsc::Receiver<RepairOutcome>>,
    repair_peer_snapshot: &mut Option<ArcShift<crate::repair::RepairPeerSnapshot>>,
    repair_driver_handle: &mut Option<RepairDriverHandle>,
) {
    *repair_command_tx = None;
    *repair_result_rx = None;
    *repair_peer_snapshot = None;
    if let Some(handle) = repair_driver_handle.take() {
        handle.abort();
    }
    match spawn_repair_driver(repair_client, relay_cache) {
        Ok((command_tx, result_rx, peer_snapshot, driver_handle)) => {
            *repair_command_tx = Some(command_tx);
            *repair_result_rx = Some(result_rx);
            *repair_peer_snapshot = Some(peer_snapshot);
            *repair_driver_handle = Some(driver_handle);
        }
        Err(error) => {
            tracing::error!(?error, "failed to replace repair driver");
        }
    }
}

#[cfg(feature = "gossip-bootstrap")]
pub(crate) async fn stop_repair_driver(
    repair_command_tx: &mut Option<mpsc::Sender<RepairCommand>>,
    repair_result_rx: &mut Option<mpsc::Receiver<RepairOutcome>>,
    repair_peer_snapshot: &mut Option<ArcShift<crate::repair::RepairPeerSnapshot>>,
    repair_driver_handle: &mut Option<RepairDriverHandle>,
) {
    *repair_command_tx = None;
    *repair_result_rx = None;
    *repair_peer_snapshot = None;
    if let Some(handle) = repair_driver_handle.take() {
        handle.shutdown().await;
    }
}
