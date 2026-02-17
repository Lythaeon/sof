use super::*;

pub(super) struct DatasetProcessInput {
    pub(super) slot: u64,
    pub(super) start_index: u32,
    pub(super) end_index: u32,
    pub(super) last_in_slot: bool,
    pub(super) serialized_shreds: Vec<Vec<u8>>,
}

pub(super) struct DatasetProcessContext<'context> {
    pub(super) plugin_host: &'context PluginHost,
    pub(super) tx_event_tx: &'context mpsc::Sender<TxObservedEvent>,
    pub(super) tx_event_drop_count: &'context AtomicU64,
    pub(super) dataset_decode_fail_count: &'context AtomicU64,
    pub(super) dataset_tail_skip_count: &'context AtomicU64,
    pub(super) log_dataset_reconstruction: bool,
}

pub(super) fn process_completed_dataset(
    input: DatasetProcessInput,
    context: &DatasetProcessContext<'_>,
) -> DatasetProcessOutcome {
    let DatasetProcessInput {
        slot,
        start_index,
        end_index,
        last_in_slot,
        serialized_shreds,
    } = input;
    let Some((entries, payload, skipped_prefix_shreds)) =
        decode_entries_from_shreds(&serialized_shreds)
    else {
        let _ = context
            .dataset_decode_fail_count
            .fetch_add(1, Ordering::Relaxed);
        tracing::debug!(
            slot,
            start_index,
            end_index,
            shreds = serialized_shreds.len(),
            "failed to decode entries from completed data range"
        );
        return DatasetProcessOutcome::DecodeFailed;
    };
    let effective_start_index = start_index.saturating_add(skipped_prefix_shreds);
    if skipped_prefix_shreds > 0 {
        let _ = context
            .dataset_tail_skip_count
            .fetch_add(u64::from(skipped_prefix_shreds), Ordering::Relaxed);
    }
    let plugin_hooks_enabled = !context.plugin_host.is_empty();

    let mut tx_count: u64 = 0;
    for entry in entries {
        for tx in entry.transactions {
            let kind = classify_tx_kind(&tx);
            let signature = tx.signatures.first().cloned();
            tx_count = tx_count.saturating_add(1);
            if plugin_hooks_enabled {
                context.plugin_host.on_transaction(TransactionEvent {
                    slot,
                    signature,
                    tx: Arc::new(tx),
                    kind,
                });
            }
            let event = TxObservedEvent {
                slot,
                signature: signature.unwrap_or_default(),
                kind,
            };
            if context.tx_event_tx.try_send(event).is_err() {
                let _ = context.tx_event_drop_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    if plugin_hooks_enabled {
        context.plugin_host.on_dataset(DatasetEvent {
            slot,
            start_index: effective_start_index,
            end_index,
            last_in_slot,
            shreds: serialized_shreds.len(),
            payload_len: payload.len(),
            tx_count,
        });
    }

    if context.log_dataset_reconstruction {
        tracing::info!(
            slot,
            start_index = effective_start_index,
            end_index,
            last_in_slot,
            shreds = serialized_shreds.len(),
            payload_len = payload.len(),
            tx_count,
            skipped_prefix_shreds,
            "completed dataset reconstruction"
        );
    }
    DatasetProcessOutcome::Decoded
}

fn decode_entries_from_shreds(serialized_shreds: &[Vec<u8>]) -> Option<(Vec<Entry>, Vec<u8>, u32)> {
    for skipped_prefix in 0..serialized_shreds.len() {
        let Some(candidate) = serialized_shreds.get(skipped_prefix..) else {
            continue;
        };
        let Ok(payload) = deshred_entries_payload(candidate) else {
            continue;
        };
        if payload.is_empty() {
            let skipped_prefix_shreds = u32::try_from(skipped_prefix).ok()?;
            return Some((Vec::new(), payload, skipped_prefix_shreds));
        }
        let entries = <WincodeVec<Elem<Entry>, MaxDataShredsLen>>::deserialize(&payload).ok();
        if let Some(entries) = entries {
            let skipped_prefix_shreds = u32::try_from(skipped_prefix).ok()?;
            return Some((entries, payload, skipped_prefix_shreds));
        }
    }
    None
}

fn deshred_entries_payload(shreds: &[Vec<u8>]) -> Result<Vec<u8>, ()> {
    let mut payload = Vec::new();
    let mut previous_index: Option<u32> = None;
    let mut data_complete = false;

    for serialized in shreds {
        if data_complete {
            return Err(());
        }
        let ParsedShred::Data(data) = parse_shred(serialized).map_err(|_error| ())? else {
            return Err(());
        };
        if let Some(previous) = previous_index
            && previous.checked_add(1) != Some(data.common.index)
        {
            return Err(());
        }
        payload.extend_from_slice(&data.payload);
        data_complete = data.data_header.data_complete();
        previous_index = Some(data.common.index);
    }

    if !data_complete {
        return Err(());
    }
    Ok(payload)
}

fn classify_tx_kind(tx: &solana_transaction::versioned::VersionedTransaction) -> TxKind {
    let mut has_vote = false;
    let mut has_non_vote_non_budget = false;
    let keys = tx.message.static_account_keys();

    for ix in tx.message.instructions() {
        if let Some(program_id) = keys.get(usize::from(ix.program_id_index)) {
            if *program_id == vote::id() {
                has_vote = true;
                continue;
            }
            if *program_id != compute_budget::id() {
                has_non_vote_non_budget = true;
            }
        }
    }

    if has_vote && !has_non_vote_non_budget {
        TxKind::VoteOnly
    } else if has_vote {
        TxKind::Mixed
    } else {
        TxKind::NonVote
    }
}
