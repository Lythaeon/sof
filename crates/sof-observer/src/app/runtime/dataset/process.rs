use super::*;
use crate::framework::AccountTouchEvent;
use solana_pubkey::Pubkey;
use solana_transaction::versioned::VersionedTransaction;

pub(super) struct DatasetProcessInput {
    pub(super) slot: u64,
    pub(super) start_index: u32,
    pub(super) end_index: u32,
    pub(super) last_in_slot: bool,
    pub(super) serialized_shreds: Vec<Vec<u8>>,
}

pub(super) struct DatasetProcessContext<'context> {
    pub(super) derived_state_host: &'context DerivedStateHost,
    pub(super) plugin_host: &'context PluginHost,
    pub(super) tx_event_tx: &'context mpsc::Sender<TxObservedEvent>,
    pub(super) tx_commitment_tracker: &'context CommitmentSlotTracker,
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
        crate::runtime_metrics::observe_decode_failed_dataset();
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
    let plugin_transaction_enabled = context.plugin_host.wants_transaction();
    let plugin_account_touch_enabled = context.plugin_host.wants_account_touch();
    let plugin_dataset_enabled = context.plugin_host.wants_dataset();
    let plugin_recent_blockhash_enabled = context.plugin_host.wants_recent_blockhash();
    let derived_state_transaction_enabled = context.derived_state_host.wants_transaction_applied();
    let derived_state_account_touch_enabled =
        context.derived_state_host.wants_account_touch_observed();
    let derived_state_recent_blockhash_enabled = !context.derived_state_host.is_empty();
    let account_touch_enabled = plugin_account_touch_enabled || derived_state_account_touch_enabled;
    let transaction_enabled = plugin_transaction_enabled || derived_state_transaction_enabled;
    let account_touch_needs_key_partitions = plugin_account_touch_enabled
        || context
            .derived_state_host
            .wants_account_touch_key_partitions();
    let commitment_snapshot = context.tx_commitment_tracker.snapshot();
    let commitment_status = TxCommitmentStatus::from_slot(
        slot,
        commitment_snapshot.confirmed_slot,
        commitment_snapshot.finalized_slot,
    );

    let mut tx_count: u64 = 0;
    let mut observed_recent_blockhash: Option<[u8; 32]> = None;
    for entry in entries {
        for tx in entry.transactions {
            if observed_recent_blockhash.is_none() {
                observed_recent_blockhash = Some(tx.message.recent_blockhash().to_bytes());
            }
            let kind = classify_tx_kind(&tx);
            let signature = tx.signatures.first().copied();
            let static_account_keys = tx.message.static_account_keys();
            let account_touch_event = account_touch_enabled.then(|| {
                let static_account_keys = Arc::new(static_account_keys.to_vec());
                let (writable_account_keys, readonly_account_keys) =
                    if account_touch_needs_key_partitions {
                        partition_static_account_keys(&tx)
                    } else {
                        (empty_pubkey_vec(), empty_pubkey_vec())
                    };
                AccountTouchEvent {
                    slot,
                    commitment_status,
                    confirmed_slot: commitment_snapshot.confirmed_slot,
                    finalized_slot: commitment_snapshot.finalized_slot,
                    signature,
                    account_keys: static_account_keys,
                    writable_account_keys,
                    readonly_account_keys,
                    lookup_table_account_keys: Arc::new(
                        tx.message
                            .address_table_lookups()
                            .unwrap_or(&[])
                            .iter()
                            .map(|lookup| lookup.account_key)
                            .collect::<Vec<_>>(),
                    ),
                }
            });
            let tx_index =
                if derived_state_transaction_enabled || derived_state_account_touch_enabled {
                    context.derived_state_host.next_slot_tx_index(slot)
                } else {
                    0
                };
            let transaction_event = transaction_enabled.then(|| TransactionEvent {
                slot,
                commitment_status,
                confirmed_slot: commitment_snapshot.confirmed_slot,
                finalized_slot: commitment_snapshot.finalized_slot,
                signature,
                tx: Arc::new(tx),
                kind,
            });
            tx_count = tx_count.saturating_add(1);
            if derived_state_account_touch_enabled && let Some(event) = account_touch_event.clone()
            {
                context.derived_state_host.on_account_touch(tx_index, event);
            }
            if derived_state_transaction_enabled && let Some(event) = transaction_event.clone() {
                context.derived_state_host.on_transaction(tx_index, event);
            }
            if plugin_account_touch_enabled && let Some(event) = account_touch_event {
                context.plugin_host.on_account_touch(event);
            }
            if plugin_transaction_enabled && let Some(event) = transaction_event {
                context.plugin_host.on_transaction(event);
            }
            let event = TxObservedEvent {
                slot,
                signature: signature.unwrap_or_default(),
                kind,
                commitment_status,
            };
            if context.tx_event_tx.try_send(event).is_err() {
                let _ = context.tx_event_drop_count.fetch_add(1, Ordering::Relaxed);
                crate::runtime_metrics::observe_tx_event_drops(1);
            }
        }
    }

    if let Some(recent_blockhash) = observed_recent_blockhash {
        let event = ObservedRecentBlockhashEvent {
            slot,
            recent_blockhash,
            dataset_tx_count: tx_count,
        };
        if derived_state_recent_blockhash_enabled {
            context
                .derived_state_host
                .on_recent_blockhash(event.clone());
        }
        if plugin_recent_blockhash_enabled {
            context.plugin_host.on_recent_blockhash(event);
        }
    }

    if plugin_dataset_enabled {
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
    crate::runtime_metrics::observe_decoded_dataset(tx_count);
    DatasetProcessOutcome::Decoded
}

fn partition_static_account_keys(
    tx: &VersionedTransaction,
) -> (Arc<Vec<Pubkey>>, Arc<Vec<Pubkey>>) {
    let static_account_keys = tx.message.static_account_keys();
    let mut writable_account_keys = Vec::with_capacity(static_account_keys.len());
    let mut readonly_account_keys = Vec::with_capacity(static_account_keys.len());
    for (index, key) in static_account_keys.iter().enumerate() {
        if tx.message.is_maybe_writable(index, None) {
            writable_account_keys.push(*key);
        } else {
            readonly_account_keys.push(*key);
        }
    }
    (
        Arc::new(writable_account_keys),
        Arc::new(readonly_account_keys),
    )
}

fn empty_pubkey_vec() -> Arc<Vec<Pubkey>> {
    static EMPTY: std::sync::OnceLock<Arc<Vec<Pubkey>>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| Arc::new(Vec::new())).clone()
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
