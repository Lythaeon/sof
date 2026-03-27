# TX Reconstruction And Packet Processing TODO

Date: March 24, 2026

Branch context:

- `perf/yellowstone-filter-fast-path`

Current state before the next pass:

- ingress inside this repo is mostly syscall/kernel bound on the current UDP release fixture
- the bigger remaining in-repo self-costs are decode/materialization and packet/FEC churn
- inline prefilter-backed decode skipping is already implemented and documented

This document defines the next work items and the required validation loop for each one.

## Validation Rule

Every item below must be evaluated in three layers.

1. Local focused A/B

- add or reuse one fixture that isolates the exact path being changed
- compare `before` and `after`
- keep the change only if the focused fixture improves or the profiler clearly shows a real self-cost reduction on the intended path

2. Symbolized release `perf`

- run symbolized release `perf` on the exact fixture that targets the changed path
- record which hotspot moved
- record which hotspot became the new ceiling

3. VPS 5-minute curiosity run

- deploy the same release binary to the VPS
- run the existing 5-minute harness
- record:
  - `samples`
  - `txps`
  - `first_avg_ms`
  - `last_avg_ms`
  - `ready_avg_ms`
  - `early_share`
  - `fallback_share`
- explicitly state where the change helped and where it did not

Acceptance rule for this stage:

- prioritize local focused A/B plus symbolized `perf`
- treat the VPS run as confirmation of where the change does or does not show up in the live mix
- do not reject a real self-cost win only because the current VPS mix does not exercise that path heavily

## Baseline Commands

Local library validation:

```bash
cargo fmt --all --check
CARGO_TARGET_DIR=/home/ac/.cache/sof-local-build/target cargo test -p sof --lib
```

Symbolized release build:

```bash
CARGO_PROFILE_RELEASE_STRIP=none \
CARGO_TARGET_DIR=/home/ac/.cache/sof-local-build-symbolized2/target \
cargo test --release -p sof --lib -- --ignored --nocapture
```

Current useful focused fixtures:

- inline path:
  - `app::runtime::runloop::driver::tests::inline_open_dataset_profile_fixture`
  - `app::runtime::runloop::driver::tests::inline_open_dataset_prefilter_profile_fixture`
- mixed dataset path:
  - `app::runtime::dataset::process::tests::multi_hook_profile_fixture`
- UDP ingress:
  - `ingest::receiver::core::io::tests::udp_receiver_recvmmsg_profile_fixture`

VPS run:

```bash
ssh -i ~/.ssh/sof-vps sof@91.99.102.201 \
  '~/.cache/sof-vps-inline-bench-remote.sh /home/sof/.cache/observer_inline_transactions_noop.current bounded 300 20'
```

## TODO 1: Completed-Dataset View-Based Prefilter Skip

Goal:

- extend the new inline prefilter-backed decode skip into the completed-dataset path
- classify completed-dataset transactions from sanitized views first
- full owned `VersionedTransaction` decode only for the subset that truly needs it

Why this is feasible:

- [`process_completed_dataset`](/home/ac/projects/sof/crates/sof-observer/src/app/runtime/dataset/process.rs#L56) already has:
  - `requires_owned_decode`
  - `TransactionViewBatchEvent`
  - `process_completed_dataset_from_views`
- the remaining gap is that transaction plugin classification still assumes decoded txs on the dataset path

Primary target files:

- [`process.rs`](/home/ac/projects/sof/crates/sof-observer/src/app/runtime/dataset/process.rs)
- [`core.rs`](/home/ac/projects/sof/crates/sof-observer/src/framework/host/core.rs)
- [`plugin.rs`](/home/ac/projects/sof/crates/sof-observer/src/framework/plugin.rs)

Local A/B target:

- `multi_hook_profile_fixture`
- add one focused completed-dataset fixture for:
  - manual ignore after decode
  - prefilter ignore from views

Perf target:

- `process_decoded_transaction`
- decode/materialization leaves under completed-dataset processing

Where this should help:

- workloads with many completed-dataset transaction plugins that only need account/signature filtering
- workloads with many ignored transactions

Where this may not help:

- plugins that always need a decoded owned transaction
- live mixes where most transactions are accepted and dispatched

## TODO 2: Packet-Worker / FEC Reparse And Copy Reduction

Goal:

- stop reparsing and reshaping the same packet bytes across packet workers and FEC recovery
- reduce byte ownership churn around recovered shreds

Why this is feasible:

- [`process_packet_batch_streaming`](/home/ac/projects/sof/crates/sof-observer/src/app/runtime/runloop/packet_workers.rs#L450) already has parsed primary shred metadata
- [`FecRecoverer::ingest_packet`](/home/ac/projects/sof/crates/sof-observer/src/shred/fec/core.rs#L67) reparses the packet again
- recovered packets are returned as raw `Vec<Vec<u8>>` and parsed again in packet workers

Primary target files:

- [`packet_workers.rs`](/home/ac/projects/sof/crates/sof-observer/src/app/runtime/runloop/packet_workers.rs)
- [`core.rs`](/home/ac/projects/sof/crates/sof-observer/src/shred/fec/core.rs)
- [`recover.rs`](/home/ac/projects/sof/crates/sof-observer/src/shred/fec/recover.rs)

Local A/B target:

- add one focused packet-worker/FEC fixture that measures:
  - primary packet ingest
  - recovered data path
  - total accepted shred output

Perf target:

- `process_packet_batch_streaming`
- `FecRecoverer::ingest_packet`
- parse/copy leaves around recovered shreds

Where this should help:

- packet mixes with active FEC recovery
- cases where recovered shreds contribute to inline tx availability

Where this may not help:

- traffic with little or no recovery
- VPS windows dominated by already-complete primary data shreds

## TODO 3: Dense FEC Set Storage After Config Is Known

Goal:

- replace hash-heavy in-range FEC storage with dense indexed storage once coding reveals `num_data` and `num_coding`

Why this is feasible:

- current storage in [`core.rs`](/home/ac/projects/sof/crates/sof-observer/src/shred/fec/core.rs) is:
  - `HashMap<u32, Vec<u8>>` for data
  - `HashMap<u16, Vec<u8>>` for coding
- after config is known, in-range accesses are offset-based and do not need hash lookups

Primary target files:

- [`core.rs`](/home/ac/projects/sof/crates/sof-observer/src/shred/fec/core.rs)
- [`recover.rs`](/home/ac/projects/sof/crates/sof-observer/src/shred/fec/recover.rs)

Local A/B target:

- reuse the packet-worker/FEC fixture from TODO 2
- if necessary, add a synthetic fixture with one configured erasure set and controlled missing-data recovery

Perf target:

- FEC set ingest/update
- `recover_missing_data`
- hash lookup / reserve / grow leaves

Where this should help:

- configured erasure sets with many in-range packets
- repeated recovery work under active loss

Where this may not help:

- tiny or incomplete FEC sets that never reach configured recovery
- low-loss runs where hash-map activity stays small

## TODO 4: Packet-Worker Scratch Reuse For Small Per-Packet State

Goal:

- reuse small per-packet worker scratch structures instead of rebuilding them every packet

Why this is feasible:

- [`process_packet_batch_streaming`](/home/ac/projects/sof/crates/sof-observer/src/app/runtime/runloop/packet_workers.rs#L450) currently creates:
  - `Vec` for `accepted_shreds`
  - `HashMap` for observed leaders
- those are typically small and worker-local

Primary target files:

- [`packet_workers.rs`](/home/ac/projects/sof/crates/sof-observer/src/app/runtime/runloop/packet_workers.rs)

Local A/B target:

- same packet-worker/FEC fixture as TODO 2
- one no-recovery packet-worker fixture if needed

Perf target:

- malloc/free and `memmove` inside packet-worker result construction
- `HashMap` setup/teardown under packet-worker results

Where this should help:

- heavy packet-worker result emission
- hot loops with many tiny accepted-shred vectors

Where this may not help:

- cases where syscall/verify/FEC dominate and result containers are already negligible

## Investigation Notes To Keep Honest

- do not assume a live VPS metric must move for every real local self-cost win
- always record whether the live mix actually exercises the changed path
- if a change only helps ignored traffic, say that explicitly
- if a change helps recovery-heavy paths only, say that explicitly
- if the current limit is kernel/syscall time, do not pretend another in-repo receiver tweak will move it much

## Reporting Template For Each Item

Use this exact structure after each pass:

1. What changed
2. Focused local A/B result
3. Symbolized `perf` result
4. VPS 5-minute result
5. Helps where
6. Does not help where
7. Keep or revert
