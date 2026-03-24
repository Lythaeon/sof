# Transaction Latency Investigation Notes (2026-03-23)

This note tracks confirmed tx-latency findings and rejected ideas while iterating on
`perf/gossip-ingest-hot-path`. The goal is to reduce latency and tail spikes without
changing protocol behavior or shrinking the delivered data surface.

## Confirmed wins

- `ad61cfa` avoids full sanitized parsing in the transaction-view batch range scan by
  using a strict structural parser for offset discovery. The saved decode profiling on
  the Hetzner VPS showed `TransactionView::try_new_sanitized` as a meaningful fixed cost.
- `ad61cfa` also caps one queued `CompletedDatasetJob` to `4` datasets. This reduced
  head-of-line delay inside hot same-worker bursts:
  - baseline clean run: `avg_tx_visibility_ms=0.702824`,
    `max_tx_visibility_us=56992`, `max_dataset_worker_start_lag_ms=56`,
    `max_dataset_processing_us=11194`, drops `0`
  - burst-split run: `avg_tx_visibility_ms=0.721457`,
    `max_tx_visibility_us=10098`, `max_dataset_worker_start_lag_ms=6`,
    `max_dataset_processing_us=9060`, drops `0`
- `08fc088` avoids flattening the full decoded dataset and recloning interested
  transactions when the non-batch decode path is active:
  - current pushed artifact: `avg_tx_visibility_ms=0.729524`,
    `max_tx_visibility_us=11121`, `max_dataset_worker_start_lag_ms=8`,
    `max_dataset_processing_us=13334`, drops `0`
  - no-flatten candidate: `avg_tx_visibility_ms=0.662133`,
    `max_tx_visibility_us=9188`, `max_dataset_worker_start_lag_ms=5`,
    `max_dataset_processing_us=6066`, drops `0`
- Local deterministic multi-hook fixture now exercises packet, shred, inline
  transaction, deferred transaction, dataset, transaction-batch,
  transaction-view-batch, account-touch, slot-status, reorg, recent-blockhash,
  cluster-topology, and leader-schedule hooks in one release-mode test run.
- On that local multi-hook fixture at `SOF_MULTI_HOOK_PROFILE_ITERS=20000`, the
  current in-tree candidates reduced elapsed time from the original `1696 ms`
  baseline to `1470 ms`:
  - single-target owned dispatch for transaction, transaction-batch,
    transaction-view-batch, and selected account-touch: `1696 ms -> 1599 ms`
  - reusing the already-joined fragmented payload between view-batch extraction
    and owned decode: `1599 ms -> 1589 ms`
  - empty/single/multi account-touch classification instead of allocating a
    `Vec` on every transaction: `1589 ms -> 1573 ms`
  - inline `SmallVec` storage for multi-plugin transaction classification:
    `1573 ms -> 1470 ms`

## Confirmed correlations

- The earlier 20-40 ms tx visibility spikes tracked
  `sof_max_dataset_worker_start_lag_ms`, not queue drops.
- Zero ingest, dataset-queue, and packet-worker drop counters were observed during the
  problematic runs, so the spikes were not caused by overflow.
- The remaining tail is much smaller after limiting one dequeue burst and after cutting
  avoidable cloning on the non-batch path.

## Inline semantics

- `TransactionDispatchMode::Inline` now means the transaction hook is delivered as soon
  as SOF has an anchored contiguous dataset prefix containing one full serialized tx for
  the inline consumer, instead of waiting for the whole completed dataset by default.
- If SOF still cannot anchor the dataset prefix early for that tx, inline delivery falls
  back to the completed-dataset point.
- If another plugin or subsystem still needs dataset-worker processing, SOF can dispatch
  the inline transaction hook first and continue the same dataset through the deferred
  worker path for the remaining consumers.
- This keeps the current data surface intact while removing dataset-worker scheduling
  jitter and some avoidable whole-dataset waiting for plugins that need lower
  transaction visibility latency.

## Exact inline latency metrics

- SOF now carries ingress timestamps through packet workers, dataset reassembly, and
  inline transaction dispatch so `/metrics` can report exact inline callback timing.
- Exported counters and gauges:
  - `sof_inline_transaction_plugin_latency_samples_total`
  - `sof_inline_transaction_plugin_first_shred_lag_us_total`
  - `sof_latest_inline_transaction_plugin_first_shred_lag_us`
  - `sof_max_inline_transaction_plugin_first_shred_lag_us`
  - `sof_inline_transaction_plugin_last_shred_lag_us_total`
  - `sof_latest_inline_transaction_plugin_last_shred_lag_us`
  - `sof_max_inline_transaction_plugin_last_shred_lag_us`
  - `sof_inline_transaction_plugin_completed_dataset_lag_us_total`
  - `sof_latest_inline_transaction_plugin_completed_dataset_lag_us`
  - `sof_max_inline_transaction_plugin_completed_dataset_lag_us`
  - `sof_inline_transaction_plugin_source_latency_samples_total{source=...}`
  - `sof_inline_transaction_plugin_source_first_shred_lag_us_total{source=...}`
  - `sof_inline_transaction_plugin_source_last_shred_lag_us_total{source=...}`
  - `sof_inline_transaction_plugin_source_completed_dataset_lag_us_total{source=...}`
- Metric meanings:
  - `first_shred_lag_us`: first observed shred that contributes to the inline tx path ->
    inline `on_transaction` callback start
  - `last_shred_lag_us`: last observed shred required to dispatch the inline tx ->
    inline `on_transaction` callback start
  - `completed_dataset_lag_us`: inline dispatch-ready timestamp -> inline
    `on_transaction` callback start
  - `source=early_prefix`: dispatched from the anchored open-dataset prefix before
    completed-dataset fallback
  - `source=completed_dataset_fallback`: inline delivery still had to wait for the
    completed-dataset decode path
- The local synthetic multi-hook fixture can validate the plumbing, but only live or
  ingress-aware replay runs should be treated as production-representative HFT latency.

## Latest live inline split

- VPS 60 second live window on March 23, 2026 with the current early-inline tree and
  `SOF_PACKET_WORKER_BATCH_MAX_PACKETS=8`:
  - aggregate: `samples=140758`, `first_avg_ms=75.650`, `last_avg_ms=11.157`,
    `ready_avg_ms=5.446`
  - `early_prefix`: `samples=41396` (`29.41%`), `first_avg_ms=34.865`,
    `last_avg_ms=16.889`, `ready_avg_ms=16.889`
  - `completed_dataset_fallback`: `samples=99362` (`70.59%`), `first_avg_ms=92.642`,
    `last_avg_ms=8.769`, `ready_avg_ms=0.679`
- Interpretation:
  - the majority of inline callbacks are still falling back to completed-dataset
    dispatch, so the next large win must come from increasing true early-prefix coverage
    rather than shaving sub-millisecond callback overhead
  - the early-prefix path itself still spends roughly `16-17 ms` between tx
    reconstructability and callback start, so there is also remaining upstream packet /
    reassembly / parse cost before dispatch
- A follow-up 60 second live window with `SOF_PACKET_WORKER_BATCH_MAX_PACKETS=1`
  regressed instead of helping:
  - aggregate: `samples=94160`, `first_avg_ms=81.286`, `last_avg_ms=12.044`,
    `ready_avg_ms=6.292`
  - `early_prefix`: `samples=27535` (`29.24%`), `first_avg_ms=43.644`,
    `last_avg_ms=19.994`, `ready_avg_ms=19.994`
  - `completed_dataset_fallback`: `samples=66625` (`70.76%`), `first_avg_ms=96.843`,
    `last_avg_ms=8.759`, `ready_avg_ms=0.630`
- So packet-worker burst size `8` remains preferable to `1`; the remaining inline gap
  is not explained by large packet-worker batches alone.

## Current suspects

- Default `transaction_interest_ref` / `accepts_transaction_ref` implementations still
  materialize owned `TransactionEvent`s through `TransactionEventRef::to_owned()`
  when plugins do not provide a borrowed classifier. The latest local `perf` run still
  shows this path prominently.
- `extract_transaction_view_batch_from_payload_fragments()` still shows up because it
  has to validate ranges and clone payload/range storage for the batch event.
- Further wins are more likely to come from reducing allocation, copying, fanout work,
  and scheduler/context-switch overhead than from changing the data surface.

## Rejected shortcuts

- Do not reduce the current data emitted by the observer just to improve latency.
- Do not weaken transaction validation semantics in paths that define externally visible
  behavior.
- Do not make inline dispatch advisory or best-effort for plugins that explicitly asked
  for it.
