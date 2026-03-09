use super::*;
use crate::framework::AccountTouchEvent;
use crate::framework::AccountTouchEventRef;
use crate::framework::DerivedStateFeedEvent;
use crate::framework::TransactionEvent;
use crate::framework::events::TransactionEventRef;
use solana_pubkey::Pubkey;
use solana_transaction::versioned::VersionedTransaction;

pub(super) struct DatasetProcessInput {
    pub(super) slot: u64,
    pub(super) start_index: u32,
    pub(super) end_index: u32,
    pub(super) last_in_slot: bool,
    pub(super) payload_fragments: crate::reassembly::dataset::PayloadFragmentBatch,
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
    pub(super) log_all_txs: bool,
    pub(super) log_non_vote_txs: bool,
    pub(super) skip_vote_only_tx_detail_path: bool,
}

#[derive(Default)]
pub(super) struct DatasetWorkerScratch {
    payload: Vec<u8>,
    derived_state_events: Vec<DerivedStateFeedEvent>,
}

pub(super) fn process_completed_dataset(
    input: DatasetProcessInput,
    context: &DatasetProcessContext<'_>,
    scratch: &mut DatasetWorkerScratch,
) -> DatasetProcessOutcome {
    let DatasetProcessInput {
        slot,
        start_index,
        end_index,
        last_in_slot,
        payload_fragments,
    } = input;
    let Some((entries, payload_len, skipped_prefix_shreds)) =
        decode_entries_from_payload_fragments(&payload_fragments, &mut scratch.payload)
    else {
        let _ = context
            .dataset_decode_fail_count
            .fetch_add(1, Ordering::Relaxed);
        crate::runtime_metrics::observe_decode_failed_dataset();
        tracing::debug!(
            slot,
            start_index,
            end_index,
            shreds = payload_fragments.len(),
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
    let derived_state_recent_blockhash_enabled =
        context.derived_state_host.wants_control_plane_observed();
    let account_touch_needs_key_partitions = context
        .derived_state_host
        .wants_account_touch_key_partitions();
    let commitment_snapshot = context.tx_commitment_tracker.snapshot();
    let commitment_status = TxCommitmentStatus::from_slot(
        slot,
        commitment_snapshot.confirmed_slot,
        commitment_snapshot.finalized_slot,
    );
    let derived_state_watermarks = FeedWatermarks {
        canonical_tip_slot: None,
        processed_slot: Some(slot),
        confirmed_slot: commitment_snapshot.confirmed_slot,
        finalized_slot: commitment_snapshot.finalized_slot,
    };
    let derived_state_events_enabled = derived_state_transaction_enabled
        || derived_state_account_touch_enabled
        || derived_state_recent_blockhash_enabled;
    let derived_state_events = &mut scratch.derived_state_events;

    let dataset_tx_count: u32 = entries
        .iter()
        .map(|entry| u32::try_from(entry.transactions.len()).unwrap_or(u32::MAX))
        .fold(0_u32, u32::saturating_add);
    if derived_state_events_enabled {
        derived_state_events.clear();
        let dataset_event_capacity = usize::try_from(dataset_tx_count)
            .unwrap_or(usize::MAX / 2)
            .saturating_mul(2)
            .saturating_add(1);
        if derived_state_events.capacity() < dataset_event_capacity {
            derived_state_events
                .reserve(dataset_event_capacity.saturating_sub(derived_state_events.capacity()));
        }
    }
    let dataset_tx_index_base =
        if derived_state_transaction_enabled || derived_state_account_touch_enabled {
            context
                .derived_state_host
                .reserve_slot_tx_indexes(slot, dataset_tx_count)
        } else {
            0
        };

    let mut tx_count: u64 = 0;
    let mut vote_only_count: u64 = 0;
    let mut mixed_count: u64 = 0;
    let mut non_vote_count: u64 = 0;
    let mut observed_recent_blockhash: Option<[u8; 32]> = None;
    let emit_detailed_tx_events = context.log_all_txs || context.log_non_vote_txs;
    let mut dataset_tx_offset = 0_u32;
    for entry in entries {
        for tx in entry.transactions {
            let tx_ref = &tx;
            if observed_recent_blockhash.is_none() {
                observed_recent_blockhash = Some(tx_ref.message.recent_blockhash().to_bytes());
            }
            let kind = classify_tx_kind(tx_ref);
            tx_count = tx_count.saturating_add(1);
            match kind {
                TxKind::VoteOnly => {
                    vote_only_count = vote_only_count.saturating_add(1);
                }
                TxKind::Mixed => {
                    mixed_count = mixed_count.saturating_add(1);
                }
                TxKind::NonVote => {
                    non_vote_count = non_vote_count.saturating_add(1);
                }
            }
            if context.skip_vote_only_tx_detail_path && kind == TxKind::VoteOnly {
                if emit_detailed_tx_events {
                    let event = TxObservedEvent::Detailed {
                        slot,
                        signature: tx_ref.signatures.first().copied().unwrap_or_default(),
                        kind,
                        commitment_status,
                    };
                    if context.tx_event_tx.try_send(event).is_err() {
                        let _ = context.tx_event_drop_count.fetch_add(1, Ordering::Relaxed);
                        crate::runtime_metrics::observe_tx_event_drops(1);
                    }
                }
                continue;
            }
            let signature = tx_ref.signatures.first().copied();
            let static_account_keys = tx_ref.message.static_account_keys();
            let account_touch_event_ref = AccountTouchEventRef {
                slot,
                commitment_status,
                confirmed_slot: commitment_snapshot.confirmed_slot,
                finalized_slot: commitment_snapshot.finalized_slot,
                signature,
                account_keys: static_account_keys,
                lookup_table_account_key_count: lookup_table_account_key_count(tx_ref),
            };
            let plugin_account_touch_plugins = plugin_account_touch_enabled.then(|| {
                context
                    .plugin_host
                    .classify_account_touch_ref(account_touch_event_ref)
            });
            let account_touch_event = (derived_state_account_touch_enabled
                || plugin_account_touch_plugins
                    .as_ref()
                    .is_some_and(|plugins| !plugins.is_empty()))
            .then(|| {
                let static_account_keys = Arc::new(static_account_keys.to_vec());
                let (writable_account_keys, readonly_account_keys) =
                    if account_touch_needs_key_partitions {
                        partition_static_account_keys(tx_ref)
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
                    lookup_table_account_key_count: lookup_table_account_key_count(tx_ref),
                }
            });
            let tx_index = dataset_tx_index_base.saturating_add(dataset_tx_offset);
            dataset_tx_offset = dataset_tx_offset.saturating_add(1);
            let transaction_event_ref = TransactionEventRef {
                slot,
                commitment_status,
                confirmed_slot: commitment_snapshot.confirmed_slot,
                finalized_slot: commitment_snapshot.finalized_slot,
                signature,
                tx: tx_ref,
                kind,
            };
            let plugin_transaction_dispatch = plugin_transaction_enabled.then(|| {
                context
                    .plugin_host
                    .classify_transaction_ref(transaction_event_ref)
            });
            let transaction_event = if derived_state_transaction_enabled
                || plugin_transaction_dispatch
                    .as_ref()
                    .is_some_and(|dispatch| !dispatch.is_empty())
            {
                let tx = Arc::new(tx);
                Some(TransactionEvent {
                    slot,
                    commitment_status,
                    confirmed_slot: commitment_snapshot.confirmed_slot,
                    finalized_slot: commitment_snapshot.finalized_slot,
                    signature,
                    tx,
                    kind,
                })
            } else {
                None
            };
            let plugin_account_touch_needed = plugin_account_touch_plugins
                .as_ref()
                .is_some_and(|plugins| !plugins.is_empty());
            let plugin_transaction_needed = plugin_transaction_dispatch
                .as_ref()
                .is_some_and(|dispatch| !dispatch.is_empty());
            let mut account_touch_event = account_touch_event;
            let mut transaction_event = transaction_event;
            if derived_state_account_touch_enabled {
                if plugin_account_touch_needed {
                    if let Some(event) = account_touch_event.clone() {
                        derived_state_events.push(DerivedStateFeedEvent::AccountTouchObserved(
                            (tx_index, event).into(),
                        ));
                    }
                } else if let Some(event) = account_touch_event.take() {
                    derived_state_events.push(DerivedStateFeedEvent::AccountTouchObserved(
                        (tx_index, event).into(),
                    ));
                }
            }
            if derived_state_transaction_enabled {
                if plugin_transaction_needed {
                    if let Some(event) = transaction_event.clone() {
                        derived_state_events.push(DerivedStateFeedEvent::TransactionApplied(
                            (tx_index, event).into(),
                        ));
                    }
                } else if let Some(event) = transaction_event.take() {
                    derived_state_events.push(DerivedStateFeedEvent::TransactionApplied(
                        (tx_index, event).into(),
                    ));
                }
            }
            if plugin_account_touch_enabled
                && let Some(plugins) = plugin_account_touch_plugins
                && !plugins.is_empty()
                && let Some(event) = account_touch_event
            {
                context
                    .plugin_host
                    .on_selected_account_touch(plugins, event);
            }
            if plugin_transaction_enabled
                && let Some(dispatch) = plugin_transaction_dispatch
                && !dispatch.is_empty()
                && let Some(event) = transaction_event
            {
                context
                    .plugin_host
                    .on_classified_transaction(dispatch, event);
            }
            if emit_detailed_tx_events {
                let event = TxObservedEvent::Detailed {
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
    }

    if !emit_detailed_tx_events && tx_count > 0 {
        let event = TxObservedEvent::Summary {
            slot,
            vote_only: vote_only_count,
            mixed: mixed_count,
            non_vote: non_vote_count,
        };
        if context.tx_event_tx.try_send(event).is_err() {
            let _ = context.tx_event_drop_count.fetch_add(1, Ordering::Relaxed);
            crate::runtime_metrics::observe_tx_event_drops(1);
        }
    }

    if let Some(recent_blockhash) = observed_recent_blockhash {
        let event = ObservedRecentBlockhashEvent {
            slot,
            recent_blockhash,
            dataset_tx_count: tx_count,
        };
        if derived_state_recent_blockhash_enabled {
            derived_state_events.push(DerivedStateFeedEvent::RecentBlockhashObserved(
                event.clone(),
            ));
        }
        if plugin_recent_blockhash_enabled {
            context.plugin_host.on_recent_blockhash(event);
        }
    }

    if derived_state_events_enabled && !derived_state_events.is_empty() {
        context
            .derived_state_host
            .on_events_drain(derived_state_watermarks, derived_state_events);
    }

    if plugin_dataset_enabled {
        context.plugin_host.on_dataset(DatasetEvent {
            slot,
            start_index: effective_start_index,
            end_index,
            last_in_slot,
            shreds: payload_fragments.len(),
            payload_len,
            tx_count,
        });
    }

    if context.log_dataset_reconstruction {
        tracing::info!(
            slot,
            start_index = effective_start_index,
            end_index,
            last_in_slot,
            shreds = payload_fragments.len(),
            payload_len,
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

fn lookup_table_account_key_count(tx: &VersionedTransaction) -> usize {
    tx.message
        .address_table_lookups()
        .map_or(0, |lookups| lookups.len())
}

fn decode_entries_from_payload_fragments(
    payload_fragments: &crate::reassembly::dataset::PayloadFragmentBatch,
    scratch_payload: &mut Vec<u8>,
) -> Option<(Vec<Entry>, usize, u32)> {
    let total_payload_len = payload_fragments.total_len();
    if total_payload_len == 0 {
        return Some((Vec::new(), 0, 0));
    }
    if payload_fragments.len() > 1 {
        let all_fragments = payload_fragments.slice_from(0)?;
        join_payload_fragments_into(scratch_payload, all_fragments, total_payload_len);
    }
    for skipped_prefix in 0..payload_fragments.len() {
        let payload_len = payload_fragments.total_len_from(skipped_prefix)?;
        if let Some(fragment) = payload_fragments.single_fragment_from(skipped_prefix) {
            let payload = fragment.as_slice();
            if payload.is_empty() {
                let skipped_prefix_shreds = u32::try_from(skipped_prefix).ok()?;
                return Some((Vec::new(), 0, skipped_prefix_shreds));
            }
            let entries = <WincodeVec<Elem<Entry>, MaxDataShredsLen>>::deserialize(payload).ok();
            if let Some(entries) = entries {
                let skipped_prefix_shreds = u32::try_from(skipped_prefix).ok()?;
                return Some((entries, payload_len, skipped_prefix_shreds));
            }
            continue;
        }
        let Some(candidate) = payload_fragments.slice_from(skipped_prefix) else {
            continue;
        };
        let payload = if skipped_prefix == 0 {
            scratch_payload.as_slice()
        } else {
            let skipped_bytes = total_payload_len.saturating_sub(payload_len);
            scratch_payload.get(skipped_bytes..).unwrap_or_default()
        };
        if payload.is_empty() {
            let skipped_prefix_shreds = u32::try_from(skipped_prefix).ok()?;
            return Some((Vec::new(), 0, skipped_prefix_shreds));
        }
        debug_assert_eq!(
            candidate.len(),
            payload_fragments.len().saturating_sub(skipped_prefix)
        );
        let entries = <WincodeVec<Elem<Entry>, MaxDataShredsLen>>::deserialize(payload).ok();
        if let Some(entries) = entries {
            let skipped_prefix_shreds = u32::try_from(skipped_prefix).ok()?;
            return Some((entries, payload_len, skipped_prefix_shreds));
        }
    }
    None
}

fn join_payload_fragments_into(
    buffer: &mut Vec<u8>,
    fragments: &[crate::reassembly::dataset::SharedPayloadFragment],
    payload_len: usize,
) {
    buffer.clear();
    if buffer.capacity() < payload_len {
        buffer.reserve(payload_len.saturating_sub(buffer.capacity()));
    }
    for fragment in fragments {
        buffer.extend_from_slice(fragment.as_slice());
    }
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
