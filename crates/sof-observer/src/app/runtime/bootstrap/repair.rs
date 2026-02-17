#[cfg(feature = "gossip-bootstrap")]
use super::*;

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
        packet: Vec<u8>,
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
    mut repair_client: crate::repair::GossipRepairClient,
) -> (
    mpsc::Sender<RepairCommand>,
    mpsc::Receiver<RepairOutcome>,
    ArcShift<crate::repair::RepairPeerSnapshot>,
    JoinHandle<()>,
) {
    let (command_tx, mut command_rx) =
        mpsc::channel::<RepairCommand>(read_repair_command_queue_capacity());
    let (result_tx, result_rx) =
        mpsc::channel::<RepairOutcome>(read_repair_result_queue_capacity());
    let peer_snapshot = repair_client.peer_snapshot_handle();
    let driver_handle = tokio::spawn(async move {
        let mut slot_hint = 0_u64;
        let mut refresh_tick = interval(Duration::from_millis(read_repair_peer_refresh_ms()));
        refresh_tick.tick().await;
        let _ = repair_client.refresh_peer_snapshot(slot_hint);
        loop {
            tokio::select! {
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
                            match repair_client.maybe_handle_response_ping(&packet, from_addr).await {
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
                    }
                }
                _ = refresh_tick.tick() => {
                    let _ = repair_client.refresh_peer_snapshot(slot_hint);
                }
            }
        }
    });
    (command_tx, result_rx, peer_snapshot, driver_handle)
}

#[cfg(feature = "gossip-bootstrap")]
pub(crate) fn replace_repair_driver(
    repair_client: crate::repair::GossipRepairClient,
    repair_command_tx: &mut Option<mpsc::Sender<RepairCommand>>,
    repair_result_rx: &mut Option<mpsc::Receiver<RepairOutcome>>,
    repair_peer_snapshot: &mut Option<ArcShift<crate::repair::RepairPeerSnapshot>>,
    repair_driver_handle: &mut Option<JoinHandle<()>>,
) {
    *repair_command_tx = None;
    *repair_result_rx = None;
    *repair_peer_snapshot = None;
    if let Some(handle) = repair_driver_handle.take() {
        handle.abort();
    }
    let (command_tx, result_rx, peer_snapshot, driver_handle) = spawn_repair_driver(repair_client);
    *repair_command_tx = Some(command_tx);
    *repair_result_rx = Some(result_rx);
    *repair_peer_snapshot = Some(peer_snapshot);
    *repair_driver_handle = Some(driver_handle);
}

#[cfg(feature = "gossip-bootstrap")]
pub(crate) async fn stop_repair_driver(
    repair_command_tx: &mut Option<mpsc::Sender<RepairCommand>>,
    repair_result_rx: &mut Option<mpsc::Receiver<RepairOutcome>>,
    repair_peer_snapshot: &mut Option<ArcShift<crate::repair::RepairPeerSnapshot>>,
    repair_driver_handle: &mut Option<JoinHandle<()>>,
) {
    *repair_command_tx = None;
    *repair_result_rx = None;
    *repair_peer_snapshot = None;
    if let Some(handle) = repair_driver_handle.take() {
        handle.abort();
        if handle.await.is_err() {
            // Driver task was already aborted/cancelled.
        }
    }
}
