use super::*;
use crate::framework::AccountTouchEvent;
use crate::framework::AccountTouchEventRef;
use crate::framework::DerivedStateFeedEvent;
use crate::framework::SerializedTransactionRange;
use crate::framework::TransactionBatchEvent;
use crate::framework::TransactionEvent;
use crate::framework::TransactionViewBatchEvent;
use crate::framework::events::TransactionEventRef;
use crate::framework::host::TransactionDispatchScope;
use agave_transaction_view::transaction_view::SanitizedTransactionView;
use core::mem::size_of;
use solana_hash::Hash;
use solana_packet::PACKET_DATA_SIZE;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;

const MESSAGE_VERSION_PREFIX: u8 = 0x80;
const MAX_INSTRUCTION_TRACE_LENGTH: usize = 64;
const MAX_SIGNATURES_PER_PACKET: usize =
    PACKET_DATA_SIZE / (size_of::<Signature>() + size_of::<Pubkey>());
const MAX_STATIC_ACCOUNTS_PER_PACKET: usize = PACKET_DATA_SIZE / size_of::<Pubkey>();

pub(in crate::app::runtime) struct DatasetProcessInput {
    pub(in crate::app::runtime) slot: u64,
    pub(in crate::app::runtime) start_index: u32,
    pub(in crate::app::runtime) end_index: u32,
    pub(in crate::app::runtime) last_in_slot: bool,
    pub(in crate::app::runtime) completed_at: std::time::Instant,
    pub(in crate::app::runtime) first_shred_observed_at: std::time::Instant,
    pub(in crate::app::runtime) last_shred_observed_at: std::time::Instant,
    pub(in crate::app::runtime) payload_fragments: crate::reassembly::dataset::PayloadFragmentBatch,
}

pub(in crate::app::runtime) struct DatasetProcessContext<'context> {
    pub(in crate::app::runtime) derived_state_host: &'context DerivedStateHost,
    pub(in crate::app::runtime) plugin_host: &'context PluginHost,
    pub(in crate::app::runtime) transaction_dispatch_scope: TransactionDispatchScope,
    pub(in crate::app::runtime) tx_event_tx: &'context mpsc::Sender<TxObservedEvent>,
    pub(in crate::app::runtime) tx_commitment_tracker: &'context CommitmentSlotTracker,
    pub(in crate::app::runtime) tx_event_drop_count: &'context AtomicU64,
    pub(in crate::app::runtime) dataset_decode_fail_count: &'context AtomicU64,
    pub(in crate::app::runtime) dataset_tail_skip_count: &'context AtomicU64,
    pub(in crate::app::runtime) log_dataset_reconstruction: bool,
    pub(in crate::app::runtime) log_all_txs: bool,
    pub(in crate::app::runtime) log_non_vote_txs: bool,
    pub(in crate::app::runtime) skip_vote_only_tx_detail_path: bool,
}

#[derive(Default)]
pub(in crate::app::runtime) struct DatasetWorkerScratch {
    payload: Vec<u8>,
    derived_state_events: Vec<DerivedStateFeedEvent>,
}

pub(in crate::app::runtime) fn process_completed_dataset(
    input: DatasetProcessInput,
    context: &DatasetProcessContext<'_>,
    scratch: &mut DatasetWorkerScratch,
) -> DatasetProcessOutcome {
    let DatasetProcessInput {
        slot,
        start_index,
        end_index,
        last_in_slot,
        completed_at,
        first_shred_observed_at,
        last_shred_observed_at,
        payload_fragments,
    } = input;
    let commitment_snapshot = context.tx_commitment_tracker.snapshot();
    let commitment_status = TxCommitmentStatus::from_slot(
        slot,
        commitment_snapshot.confirmed_slot,
        commitment_snapshot.finalized_slot,
    );
    let plugin_transaction_enabled = context
        .plugin_host
        .wants_transaction_dispatch_in_scope(context.transaction_dispatch_scope);
    let plugin_transaction_batch_enabled = context.plugin_host.wants_transaction_batch();
    let plugin_transaction_view_batch_enabled = context.plugin_host.wants_transaction_view_batch();
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
    let emit_detailed_tx_events = context.log_all_txs || context.log_non_vote_txs;
    let requires_owned_decode = plugin_transaction_enabled
        || plugin_transaction_batch_enabled
        || plugin_account_touch_enabled
        || derived_state_transaction_enabled
        || derived_state_account_touch_enabled;

    let joined_payload_for_view_extract = (plugin_transaction_view_batch_enabled
        || !requires_owned_decode)
        && payload_fragments.len() > 1;
    if let Some(view_batch) = (plugin_transaction_view_batch_enabled || !requires_owned_decode)
        .then(|| {
            extract_transaction_view_batch_from_payload_fragments(
                &payload_fragments,
                &mut scratch.payload,
            )
        })
        .flatten()
    {
        let effective_start_index = start_index.saturating_add(view_batch.skipped_prefix_shreds);
        if !view_batch.transactions.is_empty() && plugin_transaction_view_batch_enabled {
            context.plugin_host.on_transaction_view_batch(
                TransactionViewBatchEvent {
                    slot,
                    start_index: effective_start_index,
                    end_index,
                    last_in_slot,
                    shreds: payload_fragments.len(),
                    payload_len: view_batch.payload_len,
                    commitment_status,
                    confirmed_slot: commitment_snapshot.confirmed_slot,
                    finalized_slot: commitment_snapshot.finalized_slot,
                    payload: Arc::from(view_batch.payload),
                    transactions: view_batch.transactions.clone(),
                },
                completed_at,
            );
        }
        if !requires_owned_decode {
            return process_completed_dataset_from_views(
                ViewOnlyDatasetProcessInput {
                    slot,
                    start_index: effective_start_index,
                    end_index,
                    last_in_slot,
                    completed_at,
                    shred_count: payload_fragments.len(),
                    confirmed_slot: commitment_snapshot.confirmed_slot,
                    finalized_slot: commitment_snapshot.finalized_slot,
                    commitment_status,
                    emit_detailed_tx_events,
                },
                &view_batch,
                context,
            );
        }
    }

    let Some((entries, payload_len, skipped_prefix_shreds)) = decode_entries_from_payload_fragments(
        &payload_fragments,
        &mut scratch.payload,
        joined_payload_for_view_extract,
    ) else {
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

    let dataset_tx_count = u32::try_from(entries.iter().fold(0_usize, |count, entry| {
        count.saturating_add(entry.transactions.len())
    }))
    .unwrap_or(u32::MAX);
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
    let process_config = ProcessDecodedTransactionConfig {
        slot,
        completed_at,
        dataset_tx_count,
        dataset_tx_index_base,
        commitment_status,
        confirmed_slot: commitment_snapshot.confirmed_slot,
        finalized_slot: commitment_snapshot.finalized_slot,
        emit_detailed_tx_events,
        emit_tx_observed_events: true,
        first_shred_observed_at,
        last_shred_observed_at,
        account_touch_needs_key_partitions,
        derived_state_account_touch_enabled,
        derived_state_transaction_enabled,
        plugin_account_touch_enabled,
        plugin_transaction_enabled,
        transaction_dispatch_scope: context.transaction_dispatch_scope,
        context,
    };
    let mut process_state = ProcessDecodedTransactionState {
        derived_state_events,
        tx_count: &mut tx_count,
        vote_only_count: &mut vote_only_count,
        mixed_count: &mut mixed_count,
        non_vote_count: &mut non_vote_count,
        observed_recent_blockhash: &mut observed_recent_blockhash,
    };

    if plugin_transaction_batch_enabled {
        let (flat_transactions, _) = flatten_entry_transactions(entries);
        if !flat_transactions.is_empty() {
            context.plugin_host.on_transaction_batch(
                TransactionBatchEvent {
                    slot,
                    start_index: effective_start_index,
                    end_index,
                    last_in_slot,
                    shreds: payload_fragments.len(),
                    payload_len,
                    commitment_status,
                    confirmed_slot: commitment_snapshot.confirmed_slot,
                    finalized_slot: commitment_snapshot.finalized_slot,
                    transactions: flat_transactions.clone(),
                },
                completed_at,
            );
        }
        for (dataset_tx_offset, tx) in flat_transactions.iter().enumerate() {
            let dataset_tx_offset = u32::try_from(dataset_tx_offset).unwrap_or(u32::MAX);
            process_decoded_transaction(
                std::borrow::Cow::Borrowed(tx),
                dataset_tx_offset,
                &process_config,
                &mut process_state,
            );
        }
    } else {
        for (dataset_tx_offset, tx) in entries
            .into_iter()
            .flat_map(|entry| entry.transactions.into_iter())
            .enumerate()
        {
            let dataset_tx_offset = u32::try_from(dataset_tx_offset).unwrap_or(u32::MAX);
            process_decoded_transaction(
                std::borrow::Cow::Owned(tx),
                dataset_tx_offset,
                &process_config,
                &mut process_state,
            );
        }
    }

    if process_config.emit_tx_observed_events && !emit_detailed_tx_events && tx_count > 0 {
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

#[expect(
    clippy::too_many_arguments,
    reason = "inline completed-dataset dispatch keeps timing and dataset metadata explicit"
)]
pub(in crate::app::runtime) fn process_completed_dataset_inline_transactions(
    slot: u64,
    start_index: u32,
    end_index: u32,
    completed_at: Instant,
    first_shred_observed_at: Instant,
    last_shred_observed_at: Instant,
    already_emitted_tx_count: usize,
    payload_fragments: &crate::reassembly::dataset::PayloadFragmentBatch,
    context: &DatasetProcessContext<'_>,
    scratch: &mut DatasetWorkerScratch,
) -> DatasetProcessOutcome {
    if !context
        .plugin_host
        .wants_transaction_dispatch_in_scope(context.transaction_dispatch_scope)
    {
        return DatasetProcessOutcome::Decoded;
    }

    let commitment_snapshot = context.tx_commitment_tracker.snapshot();
    let commitment_status = TxCommitmentStatus::from_slot(
        slot,
        commitment_snapshot.confirmed_slot,
        commitment_snapshot.finalized_slot,
    );
    let Some((entries, _payload_len, skipped_prefix_shreds)) =
        decode_entries_from_payload_fragments(payload_fragments, &mut scratch.payload, false)
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
            "failed to decode entries from completed data range for inline transaction dispatch"
        );
        return DatasetProcessOutcome::DecodeFailed;
    };
    if skipped_prefix_shreds > 0 {
        let _ = context
            .dataset_tail_skip_count
            .fetch_add(u64::from(skipped_prefix_shreds), Ordering::Relaxed);
    }

    let dataset_tx_count = u32::try_from(entries.iter().fold(0_usize, |count, entry| {
        count.saturating_add(entry.transactions.len())
    }))
    .unwrap_or(u32::MAX);
    let derived_state_events = &mut scratch.derived_state_events;
    derived_state_events.clear();
    let process_config = ProcessDecodedTransactionConfig {
        slot,
        completed_at,
        dataset_tx_count,
        dataset_tx_index_base: 0,
        commitment_status,
        confirmed_slot: commitment_snapshot.confirmed_slot,
        finalized_slot: commitment_snapshot.finalized_slot,
        emit_detailed_tx_events: false,
        emit_tx_observed_events: false,
        first_shred_observed_at,
        last_shred_observed_at,
        account_touch_needs_key_partitions: false,
        derived_state_account_touch_enabled: false,
        derived_state_transaction_enabled: false,
        plugin_account_touch_enabled: false,
        plugin_transaction_enabled: true,
        transaction_dispatch_scope: context.transaction_dispatch_scope,
        context,
    };
    let mut tx_count: u64 = 0;
    let mut vote_only_count: u64 = 0;
    let mut mixed_count: u64 = 0;
    let mut non_vote_count: u64 = 0;
    let mut observed_recent_blockhash: Option<[u8; 32]> = None;
    let mut process_state = ProcessDecodedTransactionState {
        derived_state_events,
        tx_count: &mut tx_count,
        vote_only_count: &mut vote_only_count,
        mixed_count: &mut mixed_count,
        non_vote_count: &mut non_vote_count,
        observed_recent_blockhash: &mut observed_recent_blockhash,
    };

    for (dataset_tx_offset, tx) in entries
        .into_iter()
        .flat_map(|entry| entry.transactions.into_iter())
        .enumerate()
        .skip(already_emitted_tx_count)
    {
        let dataset_tx_offset = u32::try_from(dataset_tx_offset).unwrap_or(u32::MAX);
        process_decoded_transaction(
            std::borrow::Cow::Owned(tx),
            dataset_tx_offset,
            &process_config,
            &mut process_state,
        );
    }

    DatasetProcessOutcome::Decoded
}

#[cfg(test)]
#[derive(Debug)]
pub(in crate::app::runtime) enum EntryStreamPrefixParse {
    Complete(Vec<SerializedTransactionRange>),
    Incomplete(Vec<SerializedTransactionRange>),
    Malformed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryStreamPrefixCursorState {
    EntryCount,
    EntryPrefix {
        remaining_entries: usize,
    },
    EntryTransactions {
        remaining_entries: usize,
        remaining_txs: usize,
    },
    Complete,
}

#[derive(Debug)]
// Incremental decoder for the contiguous serialized `Vec<Entry>` prefix carried by data shreds.
//
// Upstream Agave serializes `&[Entry]` with `wincode::serialize(entries)` before shredding and
// reconstructs by concatenating consecutive data-shred payload bytes and deserializing that buffer
// back into `WincodeVec<Elem<Entry>, MaxDataShredsLen>`. Inline tx dispatch therefore has to walk
// the serialized Entry stream itself instead of inferring tx structure from shred indices.
pub(in crate::app::runtime) struct EntryStreamPrefixCursor {
    offset: usize,
    state: EntryStreamPrefixCursorState,
}

impl Default for EntryStreamPrefixCursor {
    fn default() -> Self {
        Self {
            offset: 0,
            state: EntryStreamPrefixCursorState::EntryCount,
        }
    }
}

#[derive(Debug)]
pub(in crate::app::runtime) enum EntryStreamPrefixAdvance {
    Complete,
    Incomplete,
    Malformed,
}

impl EntryStreamPrefixCursor {
    pub(in crate::app::runtime) const fn is_complete_for(&self, payload_len: usize) -> bool {
        matches!(self.state, EntryStreamPrefixCursorState::Complete) && self.offset == payload_len
    }

    pub(in crate::app::runtime) fn advance(
        &mut self,
        payload: &[u8],
        tx_ranges: &mut Vec<SerializedTransactionRange>,
    ) -> EntryStreamPrefixAdvance {
        loop {
            match self.state {
                EntryStreamPrefixCursorState::EntryCount => {
                    let mut offset = self.offset;
                    let entry_count = match read_u64_le_partial(payload, &mut offset) {
                        Ok(entry_count) => match usize::try_from(entry_count) {
                            Ok(entry_count) => entry_count,
                            Err(_) => return EntryStreamPrefixAdvance::Malformed,
                        },
                        Err(PartialParseError::Incomplete) => {
                            return EntryStreamPrefixAdvance::Incomplete;
                        }
                        Err(PartialParseError::Invalid) => {
                            return EntryStreamPrefixAdvance::Malformed;
                        }
                    };
                    self.offset = offset;
                    self.state = EntryStreamPrefixCursorState::EntryPrefix {
                        remaining_entries: entry_count,
                    };
                }
                EntryStreamPrefixCursorState::EntryPrefix { remaining_entries } => {
                    if remaining_entries == 0 {
                        self.state = EntryStreamPrefixCursorState::Complete;
                        return if self.offset == payload.len() {
                            EntryStreamPrefixAdvance::Complete
                        } else {
                            EntryStreamPrefixAdvance::Malformed
                        };
                    }
                    let mut offset = self.offset;
                    let entry_prefix_len = size_of::<u64>().saturating_add(size_of::<Hash>());
                    match advance_fixed_partial(payload, &mut offset, entry_prefix_len) {
                        Ok(()) => {}
                        Err(PartialParseError::Incomplete) => {
                            return EntryStreamPrefixAdvance::Incomplete;
                        }
                        Err(PartialParseError::Invalid) => {
                            return EntryStreamPrefixAdvance::Malformed;
                        }
                    }
                    let tx_count = match read_u64_le_partial(payload, &mut offset) {
                        Ok(tx_count) => match usize::try_from(tx_count) {
                            Ok(tx_count) => tx_count,
                            Err(_) => return EntryStreamPrefixAdvance::Malformed,
                        },
                        Err(PartialParseError::Incomplete) => {
                            return EntryStreamPrefixAdvance::Incomplete;
                        }
                        Err(PartialParseError::Invalid) => {
                            return EntryStreamPrefixAdvance::Malformed;
                        }
                    };
                    self.offset = offset;
                    self.state = EntryStreamPrefixCursorState::EntryTransactions {
                        remaining_entries: remaining_entries.saturating_sub(1),
                        remaining_txs: tx_count,
                    };
                }
                EntryStreamPrefixCursorState::EntryTransactions {
                    remaining_entries,
                    remaining_txs,
                } => {
                    if remaining_txs == 0 {
                        self.state =
                            EntryStreamPrefixCursorState::EntryPrefix { remaining_entries };
                        continue;
                    }
                    let tx_start = self.offset;
                    let mut offset = self.offset;
                    match advance_serialized_transaction_partial(payload, &mut offset) {
                        Ok(()) => {
                            let Some(start) = u32::try_from(tx_start).ok() else {
                                return EntryStreamPrefixAdvance::Malformed;
                            };
                            let Some(end) = u32::try_from(offset).ok() else {
                                return EntryStreamPrefixAdvance::Malformed;
                            };
                            tx_ranges.push(SerializedTransactionRange::new(start, end));
                            self.offset = offset;
                            self.state = EntryStreamPrefixCursorState::EntryTransactions {
                                remaining_entries,
                                remaining_txs: remaining_txs.saturating_sub(1),
                            };
                        }
                        Err(PartialParseError::Incomplete) => {
                            return EntryStreamPrefixAdvance::Incomplete;
                        }
                        Err(PartialParseError::Invalid) => {
                            return EntryStreamPrefixAdvance::Malformed;
                        }
                    }
                }
                EntryStreamPrefixCursorState::Complete => {
                    return if self.offset == payload.len() {
                        EntryStreamPrefixAdvance::Complete
                    } else {
                        EntryStreamPrefixAdvance::Malformed
                    };
                }
            }
        }
    }
}

#[cfg(test)]
pub(in crate::app::runtime) fn parse_entry_stream_ready_transaction_ranges(
    payload: &[u8],
) -> EntryStreamPrefixParse {
    let mut tx_ranges = Vec::new();
    let mut cursor = EntryStreamPrefixCursor::default();
    match cursor.advance(payload, &mut tx_ranges) {
        EntryStreamPrefixAdvance::Complete => EntryStreamPrefixParse::Complete(tx_ranges),
        EntryStreamPrefixAdvance::Incomplete => EntryStreamPrefixParse::Incomplete(tx_ranges),
        EntryStreamPrefixAdvance::Malformed => EntryStreamPrefixParse::Malformed,
    }
}

fn process_completed_dataset_from_views(
    input: ViewOnlyDatasetProcessInput,
    view_batch: &ExtractedTransactionViewBatch<'_>,
    context: &DatasetProcessContext<'_>,
) -> DatasetProcessOutcome {
    let ViewOnlyDatasetProcessInput {
        slot,
        start_index,
        end_index,
        last_in_slot,
        completed_at,
        shred_count,
        confirmed_slot,
        finalized_slot,
        commitment_status,
        emit_detailed_tx_events,
    } = input;
    let mut tx_count: u64 = 0;
    let mut vote_only_count: u64 = 0;
    let mut mixed_count: u64 = 0;
    let mut non_vote_count: u64 = 0;
    let mut observed_recent_blockhash: Option<[u8; 32]> = None;

    for range in view_batch.transactions.iter() {
        let start = usize::try_from(range.start()).ok();
        let end = usize::try_from(range.end()).ok();
        let Some(bytes) = start
            .zip(end)
            .and_then(|(start, end)| view_batch.payload.get(start..end))
        else {
            let _ = context
                .dataset_decode_fail_count
                .fetch_add(1, Ordering::Relaxed);
            crate::runtime_metrics::observe_decode_failed_dataset();
            return DatasetProcessOutcome::DecodeFailed;
        };
        let Ok(view) = SanitizedTransactionView::try_new_sanitized(bytes, true) else {
            let _ = context
                .dataset_decode_fail_count
                .fetch_add(1, Ordering::Relaxed);
            crate::runtime_metrics::observe_decode_failed_dataset();
            return DatasetProcessOutcome::DecodeFailed;
        };
        if observed_recent_blockhash.is_none() {
            observed_recent_blockhash = Some(view.recent_blockhash().to_bytes());
        }
        let kind = classify_tx_kind_view(&view);
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
        if emit_detailed_tx_events {
            let event = TxObservedEvent::Detailed {
                slot,
                signature: view.signatures().first().copied().unwrap_or_default(),
                kind,
                commitment_status,
            };
            if context.tx_event_tx.try_send(event).is_err() {
                let _ = context.tx_event_drop_count.fetch_add(1, Ordering::Relaxed);
                crate::runtime_metrics::observe_tx_event_drops(1);
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
        if context.derived_state_host.wants_control_plane_observed() {
            let mut events = vec![DerivedStateFeedEvent::RecentBlockhashObserved(
                event.clone(),
            )];
            context.derived_state_host.on_events_drain(
                FeedWatermarks {
                    canonical_tip_slot: None,
                    processed_slot: Some(slot),
                    confirmed_slot,
                    finalized_slot,
                },
                &mut events,
            );
        }
        if context.plugin_host.wants_recent_blockhash() {
            context.plugin_host.on_recent_blockhash(event);
        }
    }

    if context.plugin_host.wants_dataset() {
        context.plugin_host.on_dataset(DatasetEvent {
            slot,
            start_index,
            end_index,
            last_in_slot,
            shreds: shred_count,
            payload_len: view_batch.payload_len,
            tx_count,
        });
    }

    if context.log_dataset_reconstruction {
        tracing::info!(
            slot,
            start_index,
            end_index,
            last_in_slot,
            shreds = shred_count,
            payload_len = view_batch.payload_len,
            tx_count,
            skipped_prefix_shreds = view_batch.skipped_prefix_shreds,
            "completed dataset reconstruction"
        );
    }
    crate::runtime_metrics::observe_decoded_dataset(tx_count);
    let _ = completed_at;
    DatasetProcessOutcome::Decoded
}

fn flatten_entry_transactions(entries: Vec<Entry>) -> (Arc<[VersionedTransaction]>, u32) {
    let total_tx_count = entries.iter().fold(0_usize, |count, entry| {
        count.saturating_add(entry.transactions.len())
    });
    let dataset_tx_count = u32::try_from(total_tx_count).unwrap_or(u32::MAX);
    let mut flat_transactions = Vec::with_capacity(total_tx_count);
    for entry in entries {
        flat_transactions.extend(entry.transactions);
    }
    (Arc::from(flat_transactions), dataset_tx_count)
}

struct ProcessDecodedTransactionConfig<'context, 'host> {
    slot: u64,
    completed_at: Instant,
    dataset_tx_count: u32,
    dataset_tx_index_base: u32,
    commitment_status: TxCommitmentStatus,
    confirmed_slot: Option<u64>,
    finalized_slot: Option<u64>,
    emit_detailed_tx_events: bool,
    emit_tx_observed_events: bool,
    first_shred_observed_at: Instant,
    last_shred_observed_at: Instant,
    account_touch_needs_key_partitions: bool,
    derived_state_account_touch_enabled: bool,
    derived_state_transaction_enabled: bool,
    plugin_account_touch_enabled: bool,
    plugin_transaction_enabled: bool,
    transaction_dispatch_scope: TransactionDispatchScope,
    context: &'context DatasetProcessContext<'host>,
}

struct ProcessDecodedTransactionState<'derived> {
    derived_state_events: &'derived mut Vec<DerivedStateFeedEvent>,
    tx_count: &'derived mut u64,
    vote_only_count: &'derived mut u64,
    mixed_count: &'derived mut u64,
    non_vote_count: &'derived mut u64,
    observed_recent_blockhash: &'derived mut Option<[u8; 32]>,
}

fn process_decoded_transaction(
    tx: std::borrow::Cow<'_, VersionedTransaction>,
    dataset_tx_offset: u32,
    config: &ProcessDecodedTransactionConfig<'_, '_>,
    state: &mut ProcessDecodedTransactionState<'_>,
) {
    let tx_ref: &VersionedTransaction = match &tx {
        std::borrow::Cow::Borrowed(tx) => tx,
        std::borrow::Cow::Owned(tx) => tx,
    };
    if state.observed_recent_blockhash.is_none() {
        *state.observed_recent_blockhash = Some(tx_ref.message.recent_blockhash().to_bytes());
    }
    let kind = classify_tx_kind(tx_ref);
    *state.tx_count = state.tx_count.saturating_add(1);
    match kind {
        TxKind::VoteOnly => {
            *state.vote_only_count = state.vote_only_count.saturating_add(1);
        }
        TxKind::Mixed => {
            *state.mixed_count = state.mixed_count.saturating_add(1);
        }
        TxKind::NonVote => {
            *state.non_vote_count = state.non_vote_count.saturating_add(1);
        }
    }
    if config.context.skip_vote_only_tx_detail_path && kind == TxKind::VoteOnly {
        emit_detailed_tx_observed_event(
            config.context,
            config.slot,
            tx_ref.signatures.first().copied().unwrap_or_default(),
            kind,
            config.commitment_status,
            config.emit_detailed_tx_events,
        );
        return;
    }

    let signature = tx_ref.signatures.first().copied();
    let static_account_keys = tx_ref.message.static_account_keys();
    let account_touch_event_ref = AccountTouchEventRef {
        slot: config.slot,
        commitment_status: config.commitment_status,
        confirmed_slot: config.confirmed_slot,
        finalized_slot: config.finalized_slot,
        signature,
        account_keys: static_account_keys,
        lookup_table_account_key_count: lookup_table_account_key_count(tx_ref),
    };
    let plugin_account_touch_dispatch = config.plugin_account_touch_enabled.then(|| {
        config
            .context
            .plugin_host
            .classify_account_touch_ref(account_touch_event_ref)
    });
    let account_touch_event = (config.derived_state_account_touch_enabled
        || plugin_account_touch_dispatch
            .as_ref()
            .is_some_and(|dispatch| !dispatch.is_empty()))
    .then(|| {
        let (static_account_keys, writable_account_keys, readonly_account_keys) =
            if config.account_touch_needs_key_partitions {
                partition_static_account_keys(tx_ref)
            } else {
                (
                    Arc::from(static_account_keys),
                    empty_pubkey_vec(),
                    empty_pubkey_vec(),
                )
            };
        AccountTouchEvent {
            slot: config.slot,
            commitment_status: config.commitment_status,
            confirmed_slot: config.confirmed_slot,
            finalized_slot: config.finalized_slot,
            signature,
            account_keys: static_account_keys,
            writable_account_keys,
            readonly_account_keys,
            lookup_table_account_keys: lookup_table_account_keys(tx_ref),
        }
    });
    let tx_index = config
        .dataset_tx_index_base
        .saturating_add(dataset_tx_offset);
    let transaction_event_ref = TransactionEventRef {
        slot: config.slot,
        commitment_status: config.commitment_status,
        confirmed_slot: config.confirmed_slot,
        finalized_slot: config.finalized_slot,
        signature,
        tx: tx_ref,
        kind,
    };
    let plugin_transaction_dispatch = config.plugin_transaction_enabled.then(|| {
        config
            .context
            .plugin_host
            .classify_transaction_ref_in_scope(
                transaction_event_ref,
                config.transaction_dispatch_scope,
            )
    });
    let transaction_event = if config.derived_state_transaction_enabled
        || plugin_transaction_dispatch
            .as_ref()
            .is_some_and(|dispatch| !dispatch.is_empty())
    {
        Some(
            crate::framework::plugin::clone_cached_transaction_event(&transaction_event_ref)
                .unwrap_or_else(|| TransactionEvent {
                    slot: config.slot,
                    commitment_status: config.commitment_status,
                    confirmed_slot: config.confirmed_slot,
                    finalized_slot: config.finalized_slot,
                    signature,
                    tx: Arc::new(match tx {
                        std::borrow::Cow::Borrowed(tx) => tx.clone(),
                        std::borrow::Cow::Owned(tx) => tx,
                    }),
                    kind,
                }),
        )
    } else {
        None
    };
    let plugin_account_touch_needed = plugin_account_touch_dispatch
        .as_ref()
        .is_some_and(|dispatch| !dispatch.is_empty());
    let plugin_transaction_needed = plugin_transaction_dispatch
        .as_ref()
        .is_some_and(|dispatch| !dispatch.is_empty());
    let mut account_touch_event = account_touch_event;
    let mut transaction_event = transaction_event;
    if config.derived_state_account_touch_enabled {
        if plugin_account_touch_needed {
            if let Some(event) = account_touch_event.clone() {
                state
                    .derived_state_events
                    .push(DerivedStateFeedEvent::AccountTouchObserved(
                        (tx_index, event).into(),
                    ));
            }
        } else if let Some(event) = account_touch_event.take() {
            state
                .derived_state_events
                .push(DerivedStateFeedEvent::AccountTouchObserved(
                    (tx_index, event).into(),
                ));
        }
    }
    if config.derived_state_transaction_enabled {
        if plugin_transaction_needed {
            if let Some(event) = transaction_event.clone() {
                state
                    .derived_state_events
                    .push(DerivedStateFeedEvent::TransactionApplied(
                        (tx_index, event).into(),
                    ));
            }
        } else if let Some(event) = transaction_event.take() {
            state
                .derived_state_events
                .push(DerivedStateFeedEvent::TransactionApplied(
                    (tx_index, event).into(),
                ));
        }
    }
    if config.plugin_account_touch_enabled
        && let Some(dispatch) = plugin_account_touch_dispatch
        && !dispatch.is_empty()
        && let Some(event) = account_touch_event
    {
        config
            .context
            .plugin_host
            .on_selected_account_touch(dispatch, event);
    }
    if config.plugin_transaction_enabled
        && let Some(dispatch) = plugin_transaction_dispatch
        && !dispatch.is_empty()
        && let Some(event) = transaction_event
    {
        config.context.plugin_host.on_classified_transaction(
            dispatch,
            event,
            config.completed_at,
            config.first_shred_observed_at,
            config.last_shred_observed_at,
            crate::framework::host::InlineTransactionDispatchSource::CompletedDatasetFallback,
            config.dataset_tx_count,
            tx_index.saturating_sub(config.dataset_tx_index_base),
        );
    }
    emit_detailed_tx_observed_event(
        config.context,
        config.slot,
        signature.unwrap_or_default(),
        kind,
        config.commitment_status,
        config.emit_tx_observed_events && config.emit_detailed_tx_events,
    );
}

fn emit_detailed_tx_observed_event(
    context: &DatasetProcessContext<'_>,
    slot: u64,
    signature: Signature,
    kind: TxKind,
    commitment_status: TxCommitmentStatus,
    emit_detailed_tx_events: bool,
) {
    if !emit_detailed_tx_events {
        return;
    }
    let event = TxObservedEvent::Detailed {
        slot,
        signature,
        kind,
        commitment_status,
    };
    if context.tx_event_tx.try_send(event).is_err() {
        let _ = context.tx_event_drop_count.fetch_add(1, Ordering::Relaxed);
        crate::runtime_metrics::observe_tx_event_drops(1);
    }
}

struct ExtractedTransactionViewBatch<'payload> {
    payload: &'payload [u8],
    transactions: Arc<[SerializedTransactionRange]>,
    payload_len: usize,
    skipped_prefix_shreds: u32,
}

#[derive(Clone, Copy)]
struct ViewOnlyDatasetProcessInput {
    slot: u64,
    start_index: u32,
    end_index: u32,
    last_in_slot: bool,
    completed_at: std::time::Instant,
    shred_count: usize,
    confirmed_slot: Option<u64>,
    finalized_slot: Option<u64>,
    commitment_status: TxCommitmentStatus,
    emit_detailed_tx_events: bool,
}

fn partition_static_account_keys(
    tx: &VersionedTransaction,
) -> (Arc<[Pubkey]>, Arc<[Pubkey]>, Arc<[Pubkey]>) {
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
        Arc::from(static_account_keys),
        Arc::from(writable_account_keys),
        Arc::from(readonly_account_keys),
    )
}

fn empty_pubkey_vec() -> Arc<[Pubkey]> {
    static EMPTY: std::sync::OnceLock<Arc<[Pubkey]>> = std::sync::OnceLock::new();
    Arc::clone(EMPTY.get_or_init(|| Arc::from([])))
}

fn lookup_table_account_key_count(tx: &VersionedTransaction) -> usize {
    tx.message
        .address_table_lookups()
        .map_or(0, |lookups| lookups.len())
}

fn lookup_table_account_keys(tx: &VersionedTransaction) -> Arc<[Pubkey]> {
    let Some(lookups) = tx.message.address_table_lookups() else {
        return empty_pubkey_vec();
    };
    if lookups.is_empty() {
        return empty_pubkey_vec();
    }
    Arc::from(
        lookups
            .iter()
            .map(|lookup| lookup.account_key)
            .collect::<Vec<_>>(),
    )
}

fn decode_entries_from_payload_fragments(
    payload_fragments: &crate::reassembly::dataset::PayloadFragmentBatch,
    scratch_payload: &mut Vec<u8>,
    scratch_contains_joined_payload: bool,
) -> Option<(Vec<Entry>, usize, u32)> {
    let total_payload_len = payload_fragments.total_len();
    if total_payload_len == 0 {
        return Some((Vec::new(), 0, 0));
    }
    if payload_fragments.len() > 1 && !scratch_contains_joined_payload {
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

fn extract_transaction_view_batch_from_payload_fragments<'payload>(
    payload_fragments: &'payload crate::reassembly::dataset::PayloadFragmentBatch,
    scratch_payload: &'payload mut Vec<u8>,
) -> Option<ExtractedTransactionViewBatch<'payload>> {
    let total_payload_len = payload_fragments.total_len();
    if total_payload_len == 0 {
        return None;
    }
    if payload_fragments.len() > 1 {
        let all_fragments = payload_fragments.slice_from(0)?;
        join_payload_fragments_into(scratch_payload, all_fragments, total_payload_len);
    }
    for skipped_prefix in 0..payload_fragments.len() {
        let payload_len = payload_fragments.total_len_from(skipped_prefix)?;
        let payload = payload_fragments
            .single_fragment_from(skipped_prefix)
            .map_or_else(
                || {
                    let skipped_bytes = total_payload_len.saturating_sub(payload_len);
                    scratch_payload.get(skipped_bytes..).unwrap_or_default()
                },
                |fragment| fragment.as_slice(),
            );
        if payload.is_empty() {
            continue;
        }
        let Some(ranges) = parse_transaction_view_ranges(payload) else {
            continue;
        };
        return Some(ExtractedTransactionViewBatch {
            payload,
            transactions: Arc::from(ranges),
            payload_len,
            skipped_prefix_shreds: u32::try_from(skipped_prefix).ok()?,
        });
    }
    None
}

fn parse_transaction_view_ranges(payload: &[u8]) -> Option<Vec<SerializedTransactionRange>> {
    let mut offset = 0_usize;
    let entry_count = usize::try_from(read_u64_le(payload, &mut offset)?).ok()?;
    let mut tx_ranges = Vec::new();
    for _ in 0..entry_count {
        let entry_prefix_len = size_of::<u64>().saturating_add(size_of::<Hash>());
        advance_fixed(payload, &mut offset, entry_prefix_len)?;
        let tx_count = usize::try_from(read_u64_le(payload, &mut offset)?).ok()?;
        for _ in 0..tx_count {
            let tx_start = offset;
            advance_serialized_transaction(payload, &mut offset)?;
            let tx_end = offset;
            tx_ranges.push(SerializedTransactionRange::new(
                u32::try_from(tx_start).ok()?,
                u32::try_from(tx_end).ok()?,
            ));
        }
    }
    (offset == payload.len()).then_some(tx_ranges)
}

fn advance_serialized_transaction(payload: &[u8], offset: &mut usize) -> Option<()> {
    let signature_count = decode_short_u16_len(payload, offset)?;
    if signature_count == 0 || signature_count > MAX_SIGNATURES_PER_PACKET {
        return None;
    }
    advance_fixed(
        payload,
        offset,
        signature_count.checked_mul(size_of::<Signature>())?,
    )?;
    advance_versioned_message_strict(payload, offset, signature_count)
}

fn advance_serialized_transaction_partial(
    payload: &[u8],
    offset: &mut usize,
) -> Result<(), PartialParseError> {
    let signature_count = decode_short_u16_len_partial(payload, offset)?;
    if signature_count == 0 || signature_count > MAX_SIGNATURES_PER_PACKET {
        return Err(PartialParseError::Invalid);
    }
    advance_fixed_partial(
        payload,
        offset,
        signature_count
            .checked_mul(size_of::<Signature>())
            .ok_or(PartialParseError::Invalid)?,
    )?;
    advance_versioned_message_partial(payload, offset, signature_count)
}

fn advance_versioned_message_strict(
    payload: &[u8],
    offset: &mut usize,
    signature_count: usize,
) -> Option<()> {
    let variant = *payload.get(*offset)?;
    *offset = (*offset).checked_add(1)?;

    let (num_required_signatures, num_readonly_signed_accounts, num_readonly_unsigned_accounts) =
        if variant & MESSAGE_VERSION_PREFIX == 0 {
            let num_readonly_signed_accounts = usize::from(*payload.get(*offset)?);
            let num_readonly_unsigned_accounts = usize::from(*payload.get(offset.checked_add(1)?)?);
            *offset = (*offset).checked_add(2)?;
            (
                usize::from(variant),
                num_readonly_signed_accounts,
                num_readonly_unsigned_accounts,
            )
        } else {
            let version = variant & !MESSAGE_VERSION_PREFIX;
            if version != 0 {
                return None;
            }
            let num_required_signatures = usize::from(*payload.get(*offset)?);
            let num_readonly_signed_accounts = usize::from(*payload.get(offset.checked_add(1)?)?);
            let num_readonly_unsigned_accounts = usize::from(*payload.get(offset.checked_add(2)?)?);
            *offset = (*offset).checked_add(3)?;
            (
                num_required_signatures,
                num_readonly_signed_accounts,
                num_readonly_unsigned_accounts,
            )
        };

    let static_account_count = decode_short_u16_len(payload, offset)?;
    if static_account_count == 0 || static_account_count > MAX_STATIC_ACCOUNTS_PER_PACKET {
        return None;
    }
    if signature_count != num_required_signatures || static_account_count < signature_count {
        return None;
    }
    if num_readonly_unsigned_accounts > static_account_count.saturating_sub(num_required_signatures)
        || num_readonly_signed_accounts >= num_required_signatures
    {
        return None;
    }

    advance_fixed(
        payload,
        offset,
        static_account_count.checked_mul(size_of::<Pubkey>())?,
    )?;
    advance_fixed(payload, offset, size_of::<Hash>())?;

    let instruction_metrics =
        advance_compiled_instructions_strict(payload, offset, static_account_count)?;

    let (total_writable_lookup_accounts, total_readonly_lookup_accounts) =
        if variant & MESSAGE_VERSION_PREFIX == 0 {
            (0_usize, 0_usize)
        } else {
            advance_address_table_lookups_strict(payload, offset)?
        };

    let total_account_count = static_account_count
        .checked_add(total_writable_lookup_accounts)?
        .checked_add(total_readonly_lookup_accounts)?;
    if total_account_count > 256 {
        return None;
    }

    if instruction_metrics.max_account_index_seen >= total_account_count {
        return None;
    }

    Some(())
}

fn advance_versioned_message_partial(
    payload: &[u8],
    offset: &mut usize,
    signature_count: usize,
) -> Result<(), PartialParseError> {
    let variant = *payload.get(*offset).ok_or(PartialParseError::Incomplete)?;
    *offset = (*offset).checked_add(1).ok_or(PartialParseError::Invalid)?;

    let (num_required_signatures, num_readonly_signed_accounts, num_readonly_unsigned_accounts) =
        if variant & MESSAGE_VERSION_PREFIX == 0 {
            let num_readonly_signed_accounts =
                usize::from(*payload.get(*offset).ok_or(PartialParseError::Incomplete)?);
            let num_readonly_unsigned_accounts = usize::from(
                *payload
                    .get(offset.checked_add(1).ok_or(PartialParseError::Invalid)?)
                    .ok_or(PartialParseError::Incomplete)?,
            );
            *offset = (*offset).checked_add(2).ok_or(PartialParseError::Invalid)?;
            (
                usize::from(variant),
                num_readonly_signed_accounts,
                num_readonly_unsigned_accounts,
            )
        } else {
            let version = variant & !MESSAGE_VERSION_PREFIX;
            if version != 0 {
                return Err(PartialParseError::Invalid);
            }
            let num_required_signatures =
                usize::from(*payload.get(*offset).ok_or(PartialParseError::Incomplete)?);
            let num_readonly_signed_accounts = usize::from(
                *payload
                    .get(offset.checked_add(1).ok_or(PartialParseError::Invalid)?)
                    .ok_or(PartialParseError::Incomplete)?,
            );
            let num_readonly_unsigned_accounts = usize::from(
                *payload
                    .get(offset.checked_add(2).ok_or(PartialParseError::Invalid)?)
                    .ok_or(PartialParseError::Incomplete)?,
            );
            *offset = (*offset).checked_add(3).ok_or(PartialParseError::Invalid)?;
            (
                num_required_signatures,
                num_readonly_signed_accounts,
                num_readonly_unsigned_accounts,
            )
        };

    let static_account_count = decode_short_u16_len_partial(payload, offset)?;
    if static_account_count == 0 || static_account_count > MAX_STATIC_ACCOUNTS_PER_PACKET {
        return Err(PartialParseError::Invalid);
    }
    if signature_count != num_required_signatures || static_account_count < signature_count {
        return Err(PartialParseError::Invalid);
    }
    if num_readonly_unsigned_accounts > static_account_count.saturating_sub(num_required_signatures)
        || num_readonly_signed_accounts >= num_required_signatures
    {
        return Err(PartialParseError::Invalid);
    }

    advance_fixed_partial(
        payload,
        offset,
        static_account_count
            .checked_mul(size_of::<Pubkey>())
            .ok_or(PartialParseError::Invalid)?,
    )?;
    advance_fixed_partial(payload, offset, size_of::<Hash>())?;

    let instruction_metrics =
        advance_compiled_instructions_partial(payload, offset, static_account_count)?;

    let (total_writable_lookup_accounts, total_readonly_lookup_accounts) =
        if variant & MESSAGE_VERSION_PREFIX == 0 {
            (0_usize, 0_usize)
        } else {
            advance_address_table_lookups_partial(payload, offset)?
        };

    let total_account_count = static_account_count
        .checked_add(total_writable_lookup_accounts)
        .ok_or(PartialParseError::Invalid)?
        .checked_add(total_readonly_lookup_accounts)
        .ok_or(PartialParseError::Invalid)?;
    if total_account_count > 256 {
        return Err(PartialParseError::Invalid);
    }

    if instruction_metrics.max_account_index_seen >= total_account_count {
        return Err(PartialParseError::Invalid);
    }

    Ok(())
}

struct InstructionMetrics {
    max_account_index_seen: usize,
}

fn advance_compiled_instructions_strict(
    payload: &[u8],
    offset: &mut usize,
    static_account_count: usize,
) -> Option<InstructionMetrics> {
    let instruction_count = decode_short_u16_len(payload, offset)?;
    if instruction_count > MAX_INSTRUCTION_TRACE_LENGTH {
        return None;
    }
    let max_program_id_index = static_account_count.saturating_sub(1);
    let mut max_account_index_seen = 0_usize;
    for _ in 0..instruction_count {
        let program_id_index = usize::from(*payload.get(*offset)?);
        *offset = (*offset).checked_add(1)?;
        if program_id_index > max_program_id_index || program_id_index == 0 {
            return None;
        }
        let account_index_count = decode_short_u16_len(payload, offset)?;
        let account_index_end = offset.checked_add(account_index_count)?;
        let account_indexes = payload.get(*offset..account_index_end)?;
        if let Some(max_index) = account_indexes.iter().copied().max() {
            max_account_index_seen = max_account_index_seen.max(usize::from(max_index));
        }
        *offset = account_index_end;
        advance_short_vec_bytes(payload, offset)?;
    }
    Some(InstructionMetrics {
        max_account_index_seen,
    })
}

fn advance_compiled_instructions_partial(
    payload: &[u8],
    offset: &mut usize,
    static_account_count: usize,
) -> Result<InstructionMetrics, PartialParseError> {
    let instruction_count = decode_short_u16_len_partial(payload, offset)?;
    if instruction_count > MAX_INSTRUCTION_TRACE_LENGTH {
        return Err(PartialParseError::Invalid);
    }
    let max_program_id_index = static_account_count.saturating_sub(1);
    let mut max_account_index_seen = 0_usize;
    for _ in 0..instruction_count {
        let program_id_index =
            usize::from(*payload.get(*offset).ok_or(PartialParseError::Incomplete)?);
        *offset = (*offset).checked_add(1).ok_or(PartialParseError::Invalid)?;
        if program_id_index > max_program_id_index || program_id_index == 0 {
            return Err(PartialParseError::Invalid);
        }
        let account_index_count = decode_short_u16_len_partial(payload, offset)?;
        let account_index_end = offset
            .checked_add(account_index_count)
            .ok_or(PartialParseError::Invalid)?;
        let account_indexes = payload
            .get(*offset..account_index_end)
            .ok_or(PartialParseError::Incomplete)?;
        if let Some(max_index) = account_indexes.iter().copied().max() {
            max_account_index_seen = max_account_index_seen.max(usize::from(max_index));
        }
        *offset = account_index_end;
        advance_short_vec_bytes_partial(payload, offset)?;
    }
    Ok(InstructionMetrics {
        max_account_index_seen,
    })
}

fn advance_address_table_lookups_strict(
    payload: &[u8],
    offset: &mut usize,
) -> Option<(usize, usize)> {
    let lookup_count = decode_short_u16_len(payload, offset)?;
    let mut total_writable_lookup_accounts = 0_usize;
    let mut total_readonly_lookup_accounts = 0_usize;
    for _ in 0..lookup_count {
        advance_fixed(payload, offset, size_of::<Pubkey>())?;
        let writable_lookup_count = decode_short_u16_len(payload, offset)?;
        total_writable_lookup_accounts =
            total_writable_lookup_accounts.checked_add(writable_lookup_count)?;
        advance_fixed(payload, offset, writable_lookup_count)?;
        let readonly_lookup_count = decode_short_u16_len(payload, offset)?;
        total_readonly_lookup_accounts =
            total_readonly_lookup_accounts.checked_add(readonly_lookup_count)?;
        advance_fixed(payload, offset, readonly_lookup_count)?;
        if writable_lookup_count == 0 && readonly_lookup_count == 0 {
            return None;
        }
    }
    Some((
        total_writable_lookup_accounts,
        total_readonly_lookup_accounts,
    ))
}

fn advance_address_table_lookups_partial(
    payload: &[u8],
    offset: &mut usize,
) -> Result<(usize, usize), PartialParseError> {
    let lookup_count = decode_short_u16_len_partial(payload, offset)?;
    let mut total_writable_lookup_accounts = 0_usize;
    let mut total_readonly_lookup_accounts = 0_usize;
    for _ in 0..lookup_count {
        advance_fixed_partial(payload, offset, size_of::<Pubkey>())?;
        let writable_lookup_count = decode_short_u16_len_partial(payload, offset)?;
        total_writable_lookup_accounts = total_writable_lookup_accounts
            .checked_add(writable_lookup_count)
            .ok_or(PartialParseError::Invalid)?;
        advance_fixed_partial(payload, offset, writable_lookup_count)?;
        let readonly_lookup_count = decode_short_u16_len_partial(payload, offset)?;
        total_readonly_lookup_accounts = total_readonly_lookup_accounts
            .checked_add(readonly_lookup_count)
            .ok_or(PartialParseError::Invalid)?;
        advance_fixed_partial(payload, offset, readonly_lookup_count)?;
        if writable_lookup_count == 0 && readonly_lookup_count == 0 {
            return Err(PartialParseError::Invalid);
        }
    }
    Ok((
        total_writable_lookup_accounts,
        total_readonly_lookup_accounts,
    ))
}

fn advance_short_vec_bytes(payload: &[u8], offset: &mut usize) -> Option<()> {
    let len = decode_short_u16_len(payload, offset)?;
    advance_fixed(payload, offset, len)
}

fn advance_short_vec_bytes_partial(
    payload: &[u8],
    offset: &mut usize,
) -> Result<(), PartialParseError> {
    let len = decode_short_u16_len_partial(payload, offset)?;
    advance_fixed_partial(payload, offset, len)
}

fn advance_fixed(payload: &[u8], offset: &mut usize, len: usize) -> Option<()> {
    let end = offset.checked_add(len)?;
    let _ = payload.get(*offset..end)?;
    *offset = end;
    Some(())
}

fn advance_fixed_partial(
    payload: &[u8],
    offset: &mut usize,
    len: usize,
) -> Result<(), PartialParseError> {
    let end = offset.checked_add(len).ok_or(PartialParseError::Invalid)?;
    let _ = payload
        .get(*offset..end)
        .ok_or(PartialParseError::Incomplete)?;
    *offset = end;
    Ok(())
}

fn read_u64_le(payload: &[u8], offset: &mut usize) -> Option<u64> {
    let end = offset.checked_add(size_of::<u64>())?;
    let bytes = payload.get(*offset..end)?;
    let mut raw = [0_u8; size_of::<u64>()];
    raw.copy_from_slice(bytes);
    *offset = end;
    Some(u64::from_le_bytes(raw))
}

fn read_u64_le_partial(payload: &[u8], offset: &mut usize) -> Result<u64, PartialParseError> {
    let end = offset
        .checked_add(size_of::<u64>())
        .ok_or(PartialParseError::Invalid)?;
    let bytes = payload
        .get(*offset..end)
        .ok_or(PartialParseError::Incomplete)?;
    let mut raw = [0_u8; size_of::<u64>()];
    raw.copy_from_slice(bytes);
    *offset = end;
    Ok(u64::from_le_bytes(raw))
}

fn decode_short_u16_len(payload: &[u8], offset: &mut usize) -> Option<usize> {
    let mut value = 0_usize;
    let mut shift = 0_u32;
    for byte_index in 0..3 {
        let byte = usize::from(*payload.get(*offset)?);
        *offset = (*offset).saturating_add(1);
        value |= (byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift = shift.saturating_add(7);
        if byte_index == 2 {
            return None;
        }
    }
    None
}

fn decode_short_u16_len_partial(
    payload: &[u8],
    offset: &mut usize,
) -> Result<usize, PartialParseError> {
    let mut value = 0_usize;
    let mut shift = 0_u32;
    for byte_index in 0..3 {
        let byte = usize::from(*payload.get(*offset).ok_or(PartialParseError::Incomplete)?);
        *offset = (*offset).saturating_add(1);
        value |= (byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift = shift.saturating_add(7);
        if byte_index == 2 {
            return Err(PartialParseError::Invalid);
        }
    }
    Err(PartialParseError::Invalid)
}

enum PartialParseError {
    Incomplete,
    Invalid,
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

pub(in crate::app::runtime) fn classify_tx_kind(
    tx: &solana_transaction::versioned::VersionedTransaction,
) -> TxKind {
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

fn classify_tx_kind_view<D: agave_transaction_view::transaction_data::TransactionData>(
    view: &SanitizedTransactionView<D>,
) -> TxKind {
    let mut has_vote = false;
    let mut has_non_vote_non_budget = false;
    for (program_id, _) in view.program_instructions_iter() {
        if *program_id == vote::id() {
            has_vote = true;
            continue;
        }
        if *program_id != compute_budget::id() {
            has_non_vote_non_budget = true;
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rand::thread_rng;
    use rand::{SeedableRng, rngs::StdRng};
    use solana_entry::entry::{Entry, MaxDataShredsLen};
    use solana_perf::test_tx::{new_test_vote_tx, test_tx};
    use std::{
        net::{Ipv4Addr, SocketAddr, SocketAddrV4},
        sync::Arc,
        time::{Duration, Instant},
    };
    use wincode::{
        Deserialize as _, Serialize as _,
        containers::{Elem, Vec as WincodeVec},
    };

    use crate::{
        event::{ForkSlotStatus, TxObservedEvent},
        framework::{
            ClusterNodeInfo, ClusterTopologyEvent, ControlPlaneSource, LeaderScheduleEntry,
            LeaderScheduleEvent, Plugin, PluginConfig, PluginContext, PluginDispatchMode,
            PluginHost, PluginSetupError, RawPacketEvent, ReorgEvent, ShredEvent, SlotStatusEvent,
        },
        shred::wire::{
            CommonHeader, DataHeader, ParsedDataShredHeader, ParsedShredHeader, ShredType,
            ShredVariant,
        },
    };

    const PROFILE_ENTRY_COUNT: usize = 32;
    const PROFILE_FRAGMENT_COUNT: usize = 32;

    macro_rules! define_noop_plugin {
        ($name:ident, $plugin_name:literal, $config:expr, $method:ident, $event_ty:ty) => {
            #[derive(Debug, Clone, Copy, Default)]
            struct $name;

            #[async_trait]
            impl Plugin for $name {
                fn name(&self) -> &'static str {
                    $plugin_name
                }

                fn config(&self) -> PluginConfig {
                    $config
                }

                async fn setup(&self, _ctx: PluginContext) -> Result<(), PluginSetupError> {
                    Ok(())
                }

                async fn $method(&self, _event: $event_ty) {}
            }
        };
    }

    define_noop_plugin!(
        ProfileRawPacketPlugin,
        "profile-raw-packet",
        PluginConfig::new().with_raw_packet(),
        on_raw_packet,
        RawPacketEvent
    );
    define_noop_plugin!(
        ProfileShredPlugin,
        "profile-shred",
        PluginConfig::new().with_shred(),
        on_shred,
        ShredEvent
    );
    define_noop_plugin!(
        ProfileInlineTransactionPlugin,
        "profile-inline-transaction",
        PluginConfig::new().with_inline_transaction(),
        on_transaction,
        &TransactionEvent
    );
    define_noop_plugin!(
        ProfileStandardTransactionPlugin,
        "profile-standard-transaction",
        PluginConfig::new().with_transaction(),
        on_transaction,
        &TransactionEvent
    );
    define_noop_plugin!(
        ProfileDatasetPlugin,
        "profile-dataset",
        PluginConfig::new().with_dataset(),
        on_dataset,
        DatasetEvent
    );
    define_noop_plugin!(
        ProfileTransactionBatchPlugin,
        "profile-transaction-batch",
        PluginConfig::new().with_transaction_batch(),
        on_transaction_batch,
        &TransactionBatchEvent
    );
    define_noop_plugin!(
        ProfileTransactionViewBatchPlugin,
        "profile-transaction-view-batch",
        PluginConfig::new().with_transaction_view_batch(),
        on_transaction_view_batch,
        &TransactionViewBatchEvent
    );
    define_noop_plugin!(
        ProfileAccountTouchPlugin,
        "profile-account-touch",
        PluginConfig::new().with_account_touch(),
        on_account_touch,
        &AccountTouchEvent
    );
    define_noop_plugin!(
        ProfileSlotStatusPlugin,
        "profile-slot-status",
        PluginConfig::new().with_slot_status(),
        on_slot_status,
        SlotStatusEvent
    );
    define_noop_plugin!(
        ProfileReorgPlugin,
        "profile-reorg",
        PluginConfig::new().with_reorg(),
        on_reorg,
        ReorgEvent
    );
    define_noop_plugin!(
        ProfileRecentBlockhashPlugin,
        "profile-recent-blockhash",
        PluginConfig::new().with_recent_blockhash(),
        on_recent_blockhash,
        ObservedRecentBlockhashEvent
    );
    define_noop_plugin!(
        ProfileClusterTopologyPlugin,
        "profile-cluster-topology",
        PluginConfig::new().with_cluster_topology(),
        on_cluster_topology,
        ClusterTopologyEvent
    );
    define_noop_plugin!(
        ProfileLeaderSchedulePlugin,
        "profile-leader-schedule",
        PluginConfig::new().with_leader_schedule(),
        on_leader_schedule,
        LeaderScheduleEvent
    );

    #[test]
    fn transaction_view_batch_parser_matches_owned_decode() {
        let dataset_entry = Entry {
            num_hashes: 1,
            hash: Hash::new_from_array([9_u8; 32]),
            transactions: vec![
                VersionedTransaction::from(test_tx()),
                VersionedTransaction::from(new_test_vote_tx(&mut thread_rng())),
            ],
        };
        let payload = WincodeVec::<Elem<Entry>, MaxDataShredsLen>::serialize(&vec![dataset_entry])
            .expect("serialize entry payload");
        let ranges = parse_transaction_view_ranges(&payload).expect("parse tx view ranges");
        let entries = <WincodeVec<Elem<Entry>, MaxDataShredsLen>>::deserialize(payload.as_slice())
            .expect("decode owned entries");
        let owned_transactions = entries
            .iter()
            .flat_map(|entry| entry.transactions.iter())
            .collect::<Vec<_>>();

        assert_eq!(ranges.len(), owned_transactions.len());
        for (range, owned_tx) in ranges.iter().zip(owned_transactions.iter()) {
            let start = usize::try_from(range.start()).expect("range start");
            let end = usize::try_from(range.end()).expect("range end");
            let bytes = payload.get(start..end).expect("tx bytes");
            let view = SanitizedTransactionView::try_new_sanitized(bytes, true)
                .expect("sanitized transaction view");
            assert_eq!(view.signatures(), owned_tx.signatures.as_slice());
            assert_eq!(view.recent_blockhash(), owned_tx.message.recent_blockhash());
        }
    }

    #[test]
    fn serialized_transaction_advancer_matches_full_transaction_bytes() {
        let transactions = [
            VersionedTransaction::from(test_tx()),
            VersionedTransaction::from(new_test_vote_tx(&mut thread_rng())),
        ];
        for transaction in transactions {
            let bytes = bincode::serialize(&transaction).expect("serialize transaction");
            let mut offset = 0_usize;
            advance_serialized_transaction(&bytes, &mut offset)
                .expect("advance serialized transaction");
            assert_eq!(offset, bytes.len());
            let view = SanitizedTransactionView::try_new_sanitized(bytes.as_slice(), true)
                .expect("sanitized transaction view");
            assert_eq!(view.signatures(), transaction.signatures.as_slice());
        }
    }

    #[test]
    fn transaction_view_batch_parser_rejects_malformed_transaction() {
        let dataset_entry = Entry {
            num_hashes: 1,
            hash: Hash::new_from_array([7_u8; 32]),
            transactions: vec![VersionedTransaction::from(test_tx())],
        };
        let mut payload =
            WincodeVec::<Elem<Entry>, MaxDataShredsLen>::serialize(&vec![dataset_entry])
                .expect("serialize entry payload");
        let range = *parse_transaction_view_ranges(&payload)
            .expect("parse tx view ranges")
            .first()
            .expect("first tx range");
        let tx_start = usize::try_from(range.start()).expect("tx start");
        payload[tx_start] = 0;
        assert!(parse_transaction_view_ranges(&payload).is_none());
    }

    #[test]
    fn entry_stream_prefix_parser_emits_complete_prefix_transactions() {
        let dataset_entry = Entry {
            num_hashes: 1,
            hash: Hash::new_from_array([5_u8; 32]),
            transactions: vec![
                VersionedTransaction::from(test_tx()),
                VersionedTransaction::from(new_test_vote_tx(&mut thread_rng())),
            ],
        };
        let payload = WincodeVec::<Elem<Entry>, MaxDataShredsLen>::serialize(&vec![dataset_entry])
            .expect("serialize entry payload");
        let first_range = *parse_transaction_view_ranges(&payload)
            .expect("full tx ranges")
            .first()
            .expect("first tx range");
        let split = usize::try_from(first_range.end()).expect("first range end");
        let prefix = &payload[..split];

        let EntryStreamPrefixParse::Incomplete(ranges) =
            parse_entry_stream_ready_transaction_ranges(prefix)
        else {
            panic!("expected incomplete entry-stream parse");
        };
        assert_eq!(ranges, vec![first_range]);
    }

    #[test]
    fn entry_stream_prefix_parser_rejects_malformed_prefix() {
        let dataset_entry = Entry {
            num_hashes: 1,
            hash: Hash::new_from_array([6_u8; 32]),
            transactions: vec![VersionedTransaction::from(test_tx())],
        };
        let mut payload =
            WincodeVec::<Elem<Entry>, MaxDataShredsLen>::serialize(&vec![dataset_entry])
                .expect("serialize entry payload");
        let range = *parse_transaction_view_ranges(&payload)
            .expect("full tx ranges")
            .first()
            .expect("first tx range");
        let tx_start = usize::try_from(range.start()).expect("tx start");
        payload[tx_start] = 0;

        assert!(matches!(
            parse_entry_stream_ready_transaction_ranges(&payload),
            EntryStreamPrefixParse::Malformed
        ));
    }

    #[test]
    fn entry_stream_prefix_cursor_matches_full_parser_across_prefix_growth() {
        let dataset_entry = Entry {
            num_hashes: 1,
            hash: Hash::new_from_array([8_u8; 32]),
            transactions: vec![
                VersionedTransaction::from(test_tx()),
                VersionedTransaction::from(new_test_vote_tx(&mut thread_rng())),
            ],
        };
        let payload = WincodeVec::<Elem<Entry>, MaxDataShredsLen>::serialize(&vec![dataset_entry])
            .expect("serialize entry payload");
        let mut cursor = EntryStreamPrefixCursor::default();
        let mut tx_ranges = Vec::new();

        for prefix_len in 0..=payload.len() {
            let prefix = &payload[..prefix_len];
            let status = cursor.advance(prefix, &mut tx_ranges);
            match (status, parse_entry_stream_ready_transaction_ranges(prefix)) {
                (
                    EntryStreamPrefixAdvance::Complete,
                    EntryStreamPrefixParse::Complete(expected),
                )
                | (
                    EntryStreamPrefixAdvance::Incomplete,
                    EntryStreamPrefixParse::Incomplete(expected),
                ) => {
                    assert_eq!(tx_ranges, expected);
                }
                (EntryStreamPrefixAdvance::Malformed, EntryStreamPrefixParse::Malformed) => {}
                (actual, expected) => {
                    panic!(
                        "entry-stream cursor/parser mismatch: actual={actual:?} expected={expected:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn entry_stream_prefix_parser_handles_tick_and_transaction_entries() {
        let entries = vec![
            Entry {
                num_hashes: 1,
                hash: Hash::new_from_array([1_u8; 32]),
                transactions: vec![],
            },
            Entry {
                num_hashes: 2,
                hash: Hash::new_from_array([2_u8; 32]),
                transactions: vec![
                    VersionedTransaction::from(test_tx()),
                    VersionedTransaction::from(new_test_vote_tx(&mut thread_rng())),
                ],
            },
            Entry {
                num_hashes: 1,
                hash: Hash::new_from_array([3_u8; 32]),
                transactions: vec![],
            },
        ];
        let payload = WincodeVec::<Elem<Entry>, MaxDataShredsLen>::serialize(&entries)
            .expect("serialize entry payload");
        let expected = parse_transaction_view_ranges(&payload).expect("full tx ranges");

        match parse_entry_stream_ready_transaction_ranges(&payload) {
            EntryStreamPrefixParse::Complete(actual) => assert_eq!(actual, expected),
            EntryStreamPrefixParse::Incomplete(actual) => {
                panic!("unexpected incomplete parse status: {actual:?}");
            }
            EntryStreamPrefixParse::Malformed => panic!("unexpected malformed parse status"),
        }
    }

    #[test]
    #[ignore = "profiling fixture for perf"]
    fn multi_hook_profile_fixture() {
        let iterations = std::env::var("SOF_MULTI_HOOK_PROFILE_ITERS")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(512);
        let payload = build_profile_payload(PROFILE_ENTRY_COUNT);
        let payload_fragments =
            crate::reassembly::dataset::PayloadFragmentBatch::from_owned_fragments(split_payload(
                &payload,
                PROFILE_FRAGMENT_COUNT,
            ));
        let plugin_host = build_profile_plugin_host();
        let derived_state_host = DerivedStateHost::default();
        let (tx_event_tx, _tx_event_rx) = mpsc::channel::<TxObservedEvent>(262_144);
        let tx_commitment_tracker = CommitmentSlotTracker::new();
        let tx_event_drop_count = AtomicU64::new(0);
        let dataset_decode_fail_count = AtomicU64::new(0);
        let dataset_tail_skip_count = AtomicU64::new(0);
        let context = DatasetProcessContext {
            derived_state_host: &derived_state_host,
            plugin_host: &plugin_host,
            transaction_dispatch_scope: TransactionDispatchScope::All,
            tx_event_tx: &tx_event_tx,
            tx_commitment_tracker: &tx_commitment_tracker,
            tx_event_drop_count: &tx_event_drop_count,
            dataset_decode_fail_count: &dataset_decode_fail_count,
            dataset_tail_skip_count: &dataset_tail_skip_count,
            log_dataset_reconstruction: false,
            log_all_txs: false,
            log_non_vote_txs: false,
            skip_vote_only_tx_detail_path: true,
        };
        let mut scratch = DatasetWorkerScratch::default();
        let started_at = Instant::now();
        for iteration in 0..iterations {
            let slot = 2_000_000_u64.saturating_add(u64::try_from(iteration).unwrap_or(u64::MAX));
            plugin_host.on_raw_packet(synthetic_raw_packet_event(iteration));
            plugin_host.on_shred(synthetic_shred_event(slot));
            plugin_host.on_slot_status(SlotStatusEvent {
                slot,
                parent_slot: slot.checked_sub(1),
                previous_status: Some(ForkSlotStatus::Processed),
                status: ForkSlotStatus::Confirmed,
                tip_slot: Some(slot),
                confirmed_slot: slot.checked_sub(1),
                finalized_slot: slot.checked_sub(32),
            });
            plugin_host.on_reorg(ReorgEvent {
                old_tip: slot.saturating_sub(1),
                new_tip: slot,
                common_ancestor: slot.checked_sub(2),
                detached_slots: vec![slot.saturating_sub(1)],
                attached_slots: vec![slot],
                confirmed_slot: slot.checked_sub(1),
                finalized_slot: slot.checked_sub(32),
            });
            plugin_host.on_cluster_topology(synthetic_cluster_topology_event(slot));
            plugin_host.on_leader_schedule(synthetic_leader_schedule_event(slot));
            let outcome = process_completed_dataset(
                DatasetProcessInput {
                    slot,
                    start_index: 0,
                    end_index: u32::try_from(payload_fragments.len().saturating_sub(1))
                        .unwrap_or(u32::MAX),
                    last_in_slot: true,
                    completed_at: Instant::now(),
                    first_shred_observed_at: Instant::now(),
                    last_shred_observed_at: Instant::now(),
                    payload_fragments: payload_fragments.clone(),
                },
                &context,
                &mut scratch,
            );
            assert!(matches!(outcome, DatasetProcessOutcome::Decoded));
        }
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(dataset_decode_fail_count.load(Ordering::Relaxed), 0);
        assert_eq!(tx_event_drop_count.load(Ordering::Relaxed), 0);
        assert_eq!(plugin_host.dropped_event_count(), 0);
        let metrics = crate::runtime_metrics::snapshot();
        println!(
            "multi_hook_profile_fixture iterations={} elapsed_ms={} dataset_tail_skips={} inline_samples={} inline_first_avg_us={} inline_last_avg_us={} inline_completed_avg_us={} inline_first_max_us={} inline_last_max_us={} inline_completed_max_us={}",
            iterations,
            started_at.elapsed().as_millis(),
            dataset_tail_skip_count.load(Ordering::Relaxed),
            metrics.inline_transaction_plugin_latency_samples_total,
            metrics.inline_transaction_plugin_first_shred_lag_us_total
                / metrics
                    .inline_transaction_plugin_latency_samples_total
                    .max(1),
            metrics.inline_transaction_plugin_last_shred_lag_us_total
                / metrics
                    .inline_transaction_plugin_latency_samples_total
                    .max(1),
            metrics.inline_transaction_plugin_completed_dataset_lag_us_total
                / metrics
                    .inline_transaction_plugin_latency_samples_total
                    .max(1),
            metrics.max_inline_transaction_plugin_first_shred_lag_us,
            metrics.max_inline_transaction_plugin_last_shred_lag_us,
            metrics.max_inline_transaction_plugin_completed_dataset_lag_us,
        );
    }

    fn build_profile_plugin_host() -> PluginHost {
        PluginHost::builder()
            .with_event_queue_capacity(262_144)
            .with_dispatch_mode(PluginDispatchMode::Sequential)
            .with_transaction_dispatch_workers(4)
            .add_plugin(ProfileRawPacketPlugin)
            .add_plugin(ProfileShredPlugin)
            .add_plugin(ProfileInlineTransactionPlugin)
            .add_plugin(ProfileStandardTransactionPlugin)
            .add_plugin(ProfileDatasetPlugin)
            .add_plugin(ProfileTransactionBatchPlugin)
            .add_plugin(ProfileTransactionViewBatchPlugin)
            .add_plugin(ProfileAccountTouchPlugin)
            .add_plugin(ProfileSlotStatusPlugin)
            .add_plugin(ProfileReorgPlugin)
            .add_plugin(ProfileRecentBlockhashPlugin)
            .add_plugin(ProfileClusterTopologyPlugin)
            .add_plugin(ProfileLeaderSchedulePlugin)
            .build()
    }

    fn build_profile_payload(entry_count: usize) -> Vec<u8> {
        let mut rng = StdRng::seed_from_u64(7);
        let mut entries = Vec::with_capacity(entry_count);
        for index in 0..entry_count {
            let hash_byte = u8::try_from(index & 0xff).unwrap_or(u8::MAX);
            entries.push(Entry {
                num_hashes: 1,
                hash: Hash::new_from_array([hash_byte; 32]),
                transactions: vec![
                    VersionedTransaction::from(test_tx()),
                    VersionedTransaction::from(new_test_vote_tx(&mut rng)),
                ],
            });
        }
        WincodeVec::<Elem<Entry>, MaxDataShredsLen>::serialize(&entries)
            .expect("serialize profile payload")
    }

    fn split_payload(payload: &[u8], fragment_count: usize) -> Vec<Vec<u8>> {
        let fragment_count = fragment_count.max(1);
        let chunk_len = payload.len().div_ceil(fragment_count).max(1);
        payload.chunks(chunk_len).map(ToOwned::to_owned).collect()
    }

    fn synthetic_raw_packet_event(iteration: usize) -> RawPacketEvent {
        let byte = u8::try_from(iteration & 0xff).unwrap_or(u8::MAX);
        RawPacketEvent {
            source: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8_001)),
            bytes: Arc::from(vec![byte; 256]),
        }
    }

    fn synthetic_shred_event(slot: u64) -> ShredEvent {
        let parsed = ParsedShredHeader::Data(ParsedDataShredHeader {
            common: CommonHeader {
                shred_variant: ShredVariant {
                    shred_type: ShredType::Data,
                    proof_size: 0,
                    resigned: false,
                },
                slot,
                index: 0,
                version: 1,
                fec_set_index: 0,
            },
            data_header: DataHeader {
                parent_offset: 0,
                flags: 0,
                size: 256,
            },
            payload_offset: 0,
            payload_len: 256,
        });
        ShredEvent {
            source: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8_001)),
            packet: Arc::from(vec![7_u8; 256]),
            parsed: Arc::new(parsed),
        }
    }

    fn synthetic_cluster_topology_event(slot: u64) -> ClusterTopologyEvent {
        ClusterTopologyEvent {
            source: ControlPlaneSource::Direct,
            slot: Some(slot),
            epoch: Some(0),
            active_entrypoint: Some("synthetic".to_owned()),
            total_nodes: 1,
            added_nodes: vec![ClusterNodeInfo {
                pubkey: Pubkey::new_from_array([3_u8; 32]),
                wallclock: slot,
                shred_version: 1,
                gossip: Some(SocketAddr::V4(SocketAddrV4::new(
                    Ipv4Addr::LOCALHOST,
                    8_001,
                ))),
                tpu: Some(SocketAddr::V4(SocketAddrV4::new(
                    Ipv4Addr::LOCALHOST,
                    8_003,
                ))),
                tpu_quic: None,
                tpu_forwards: None,
                tpu_forwards_quic: None,
                tpu_vote: None,
                tvu: Some(SocketAddr::V4(SocketAddrV4::new(
                    Ipv4Addr::LOCALHOST,
                    8_004,
                ))),
                rpc: None,
            }],
            removed_pubkeys: Vec::new(),
            updated_nodes: Vec::new(),
            snapshot_nodes: Vec::new(),
        }
    }

    fn synthetic_leader_schedule_event(slot: u64) -> LeaderScheduleEvent {
        LeaderScheduleEvent {
            source: ControlPlaneSource::Direct,
            slot: Some(slot),
            epoch: Some(0),
            added_leaders: vec![LeaderScheduleEntry {
                slot,
                leader: Pubkey::new_from_array([5_u8; 32]),
            }],
            removed_slots: Vec::new(),
            updated_leaders: Vec::new(),
            snapshot_leaders: Vec::new(),
        }
    }
}
