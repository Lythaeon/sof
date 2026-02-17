# SOF Observer Runtime: Code-Accurate Reverse Engineering

This document describes how `sof-observer` currently works in code (not a progress log).
Primary implementation lives in:

- `crates/sof-observer/src/app/runtime.rs`
- `crates/sof-observer/src/ingest/core.rs`
- `crates/sof-observer/src/shred/wire/*`
- `crates/sof-observer/src/reassembly/*`
- `crates/sof-observer/src/verify/core/*`
- `crates/sof-observer/src/repair/core.rs`
- `crates/sof-observer/src/framework/*`

## Runtime entrypoints and composition

- `sof_observer::runtime::run()` creates a Tokio multi-thread runtime and runs `run_async()`.
- `sof_observer::runtime::run_async()` orchestrates all ingest, parse, verify, reassembly, repair, telemetry, and tx classification.
- Ingest sources can run concurrently:
  - direct UDP listener (`SOF_BIND`, default `0.0.0.0:8001`)
  - TCP relay clients (`SOF_RELAY_CONNECT`, comma-separated)
  - gossip bootstrap sockets (`SOF_GOSSIP_ENTRYPOINT`, requires `gossip-bootstrap` feature)
  - optional TCP relay server (`SOF_RELAY_LISTEN`) that rebroadcasts raw packets

All sources feed a shared `mpsc::Sender<RawPacketBatch>`.

## End-to-end pipeline

For each packet in the runtime loop:

1. Emit `on_raw_packet` plugin hook.
2. Optionally mirror raw bytes to relay clients (`SOF_RELAY_LISTEN` bus).
3. If gossip repair is enabled, detect and route repair ping packets to repair handler.
4. Parse shred header (`parse_shred_header`).
5. Emit `on_shred` plugin hook.
6. Optional recent duplicate suppression (signature+slot+index+fec+version+variant key).
7. Optional cryptographic verification (`ShredVerifier`).
8. Feed packet into FEC recovery (`FecRecoverer`) to attempt recovered data shreds.
9. Update missing-shred tracker (repair), slot coverage metrics, and counters.
10. Feed data shreds into dataset reassembler; enqueue completed datasets to worker queues.
11. Process recovered data shreds through the same dataset path (with separate counters).

Dataset workers then:

1. De-duplicate recent dataset decode attempts.
2. Decode deshredded payload into `Vec<Entry>`.
3. Classify tx kind (`VoteOnly`, `Mixed`, `NonVote`).
4. Emit `on_transaction` and `on_dataset` plugin hooks.
5. Publish `TxObservedEvent` back to main runtime for aggregate metrics/logging.

## Wire format mirrored in code

Constants are in `crates/sof-observer/src/protocol/shred_wire.rs`.

Common header offsets:

- signature: `[0..64)`
- shred variant: byte `64`
- slot: `[65..73)` LE
- index: `[73..77)` LE
- version: `[77..79)` LE
- fec_set_index: `[79..83)` LE

Data header:

- parent_offset: `[83..85)` LE
- flags: byte `85`
- size: `[86..88)` LE

Coding header:

- num_data_shreds: `[83..85)` LE
- num_coding_shreds: `[85..87)` LE
- position: `[87..89)` LE

Variant high nibble mapping:

- `0x60`: merkle code
- `0x70`: merkle code resigned
- `0x90`: merkle data
- `0xB0`: merkle data resigned

Parser behavior (`shred/wire/parser.rs`):

- Data packets must be at least `SIZE_OF_DATA_SHRED_PAYLOAD` (`1203`) bytes.
- Coding packets must be at least `SIZE_OF_CODING_SHRED_PAYLOAD` (`1228`) bytes.
- Data `size` must be `>= 88` and `<=` computed variant-specific max (accounts for merkle proof + optional resigned sig trailer).
- Coding header must satisfy:
  - `num_data_shreds > 0`
  - `num_coding_shreds > 0`
  - `position < num_coding_shreds`

## Verification model

Verification is optional (`SOF_VERIFY_SHREDS`, default `false`).

`ShredVerifier` does:

1. Parse variant/signature/slot/index/fec_set from packet.
2. Compute merkle root from shred bytes + proof path.
3. Verify signature against:
  - slot leader map (if present),
  - cached signature->pubkey hits,
  - known pubkeys list.

Statuses:

- `Verified`
- `UnknownLeader`
- `InvalidMerkle`
- `InvalidSignature`
- `Malformed`

`SOF_VERIFY_STRICT=false` accepts `UnknownLeader`; strict mode drops it.

Slot-leader bootstrap:

- Optional RPC pre-load (`SOF_VERIFY_RPC_SLOT_LEADERS=true` by default) via `getSlot` + `getSlotLeaders`.
- In gossip mode, known pubkeys are continuously refreshed from repair peer snapshot.

## FEC recovery model

`FecRecoverer` groups packets by `(slot, fec_set_index)`.

- Tracks raw data shreds by `index`.
- Tracks coding shreds by `position` and enforces one consistent `(num_data, num_coding)` config.
- Rejects mixed variant class in same set (proof size/resigned mismatch).
- Runs Reed-Solomon reconstruction once total present shards is at least `num_data`.
- Recovered data shreds are synthesized and re-validated by `parse_shred`.

Recovered shreds can be independently verified when `SOF_VERIFY_RECOVERED_SHREDS=true`.

## Dataset reassembly and tx extraction

`DataSetReassembler` tracks per-slot fragment streams:

- Uses `DATA_COMPLETE`/`LAST_IN_SLOT` to mark dataset boundaries.
- Emits contiguous ranges `[start_index..end_index]` when all shreds in that interval exist.
- Supports "tail emission without slot-0 anchor" when contiguous tail length reaches `SOF_DATASET_TAIL_MIN_SHREDS` (default `2`).

`process_completed_dataset()` then:

- Tries deshred/decode with progressive prefix skipping to recover from missing early shreds.
- Deserializes entry payload as `WincodeVec<Entry>`.
- Iterates transactions and classifies kinds:
  - vote program only (plus optional compute budget): `VoteOnly`
  - vote + non-vote programs: `Mixed`
  - no vote program: `NonVote`

## Repair subsystem

Repair requires gossip mode (`gossip-bootstrap` feature + `SOF_REPAIR_ENABLED=true`).

Core pieces:

- `MissingShredTracker`: slot/FEC gap detection and request planning.
- `OutstandingRepairRequests`: in-flight dedupe/timeout for sent requests.
- `GossipRepairClient`: peer discovery, scoring, and UDP request send.

Request types:

- `WindowIndex {slot, index}`
- `HighestWindowIndex {slot, index}`

Planner behavior:

- defers requests by settle delay (`SOF_REPAIR_SETTLE_MS`, default `250`)
- enforces cooldown (`SOF_REPAIR_COOLDOWN_MS`, default `150`)
- respects slot-lag policy (`SOF_REPAIR_MIN_SLOT_LAG`, default `4`)
- applies per-slot cap (`SOF_REPAIR_PER_SLOT_CAP`, default `16`)
- supports forward probing and startup seeding (`SOF_REPAIR_SEED_SLOTS`)

Wire send path:

- Requests are serialized with fixed-int bincode `RepairProtocol::*`.
- Signature field is patched after signing the signable bytes.
- Repair response pings are recognized and answered with signed pongs.

Peer selection:

- Candidate peers from `cluster_info.repair_peers(slot)` (fallback to compatible gossip peers).
- Scores include send success/error, observed ping RTT, and shred source hit hints.
- Top-ranked active peers are retained; pick is weighted by score-derived weight.

## Gossip bootstrap and runtime switching

When `SOF_GOSSIP_ENTRYPOINT` is set:

- Entrypoints are expanded/resolved; startup falls back across candidates until one stabilizes.
- Node sockets include TVU receive sockets and repair socket; both feed ingest.
- Shred version is discovered from entrypoint unless overridden (`SOF_SHRED_VERSION`).

Runtime switch (only when repair is enabled and switch is enabled):

- Detect stall from shred age and dataset age thresholds.
- Optionally switch to alternate entrypoint.
- Supports overlap switching if a secondary non-overlapping port range is configured or auto-split is possible.
- Uses stabilization checks (sustain, packet minimum, max wait) before committing new runtime.

## Plugin framework (current runtime wiring)

Hook API:

- `on_raw_packet`
- `on_shred`
- `on_dataset`
- `on_transaction`

Current packaged runtime constructs `PluginHostBuilder::new().build()` (empty host).
So hooks exist and are called, but no plugins are registered unless you wire a non-empty `PluginHost` in your embedding runtime.

## Telemetry cadence

Every 5 seconds, runtime logs a consolidated `ingest telemetry` event with:

- packet/source-port mix
- parse/verify stats
- dedupe stats
- FEC/dataset worker queue stats
- tx class counts
- repair request and peer stats
- gossip switch stats
- slot coverage window stats

For full environment knobs and defaults, see `docs/operations/advanced-env.md`.
