# SOF

SOF is a Solana-focused Rust workspace for low-latency data ingest and transaction execution.

It is built more like financial systems infrastructure than a typical crypto framework:
bounded pipelines, local control-plane state, restart-safe derived-state feeds, and execution
paths designed for services that care about latency, replay, and operational discipline.

It is split into three user-facing crates:

- `sof`: observer/runtime crate for shred ingest, relay/cache, dataset reconstruction, plugin and runtime-extension events, fork/reorg tracking, and local commitment tagging without RPC dependency
- `sof-tx`: transaction SDK for building, signing, and submitting Solana transactions through RPC, direct leader routing, hybrid fallback, and optional kernel-bypass transports
- `sof-gossip-tuning`: typed gossip and ingest tuning presets for hosts embedding `sof`

## Highlights

- Multi-core packet ingest, FEC recovery, and dataset reconstruction
- Bundled gossip backend tuning for queue depths, worker counts, CPU pinning, and small-batch serial fallbacks
- Local market-facing control-plane signals for leader, topology, blockhash, replay, and fork state
- Local `processed` / `confirmed` / `finalized` transaction tagging
- Semantic shred dedupe that suppresses duplicate or conflicting downstream event emission
- Plugin hooks and runtime extensions for downstream logic
- Lower-copy hot paths through shared dataset payload fragments and borrowed transaction classification
- Replayable derived-state feed for restart-safe stateful services
- First-class `sof-tx` adapters for live plugin and replayable derived-state control-plane inputs
- Flow-safety policy evaluation for stale or degraded tx-control-plane state
- Optional gossip bootstrap and external kernel-bypass ingress
- Transaction submission with RPC, direct, hybrid, and kernel-bypass paths
- Typed gossip and ingest tuning presets for embedded SOF hosts

## Repository Layout

- `crates/sof-observer`: published as `sof`
- `crates/sof-tx`: published as `sof-tx`
- `crates/sof-gossip-tuning`: typed host tuning presets for gossip bootstrap and ingest
- `docs/architecture`: ADRs, ARDs, and framework/runtime contracts
- `docs/operations`: deployment and tuning docs
- `scripts`: local tooling and helper scripts

## Requirements

- Rust stable
- `cargo-make` for the full contributor gate

Install `cargo-make` if needed:

```bash
cargo install cargo-make
```

## Install

Observer/runtime crate:

```bash
cargo add sof
```

Transaction SDK:

```bash
cargo add sof-tx
```

Typed host tuning presets:

```bash
cargo add sof-gossip-tuning
```

Feature examples:

```toml
sof = { version = "0.8.1", features = ["gossip-bootstrap"] }
sof-tx = { version = "0.8.1", features = ["sof-adapters"] }
```

Kernel-bypass integrations:

- `sof` supports external ingress APIs through `--features kernel-bypass`
- `sof-tx` supports custom direct transport adapters through `--features kernel-bypass`
- `sof-solana-gossip` defaults to the lightweight in-memory duplicate/conflict path; pass `--features solana-ledger` only if you explicitly want the heavier ledger/RocksDB duplicate-shred tooling

## Duplicate Shred Policy

SOF is optimized by default for low-latency observer and execution workloads, not validator-style
duplicate-shred durability.

- Default behavior: SOF keeps duplicate/conflict suppression in memory and enforces a semantic
  shred uniqueness contract before downstream dataset, transaction, and account-touch emission.
- Opt-in durability: if you embed the vendored `sof-solana-gossip` crate directly and need the
  heavier validator-style duplicate-shred tooling, build it with `--features solana-ledger`.
- Tradeoff: the default path is lower latency and avoids RocksDB/native storage overhead; the
  ledger-backed path is heavier but keeps the older durable duplicate-shred machinery available.

## Quick Start

Run the observer runtime example:

```bash
cargo run --release -p sof --example observer_runtime
```

Run the observer with gossip bootstrap:

```bash
cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

Run the transaction SDK tests:

```bash
cargo test -p sof-tx
```

Run the derived-state service example:

```bash
cargo run --release -p sof --example derived_state_slot_mirror
```

Run the full contributor gate:

```bash
cargo make ci
```

## Basic Setup Guides

- Observer/runtime setup: `crates/sof-observer/README.md`
- Transaction SDK setup: `crates/sof-tx/README.md`
- Typed gossip tuning setup: `crates/sof-gossip-tuning/README.md`
- Docs entry point: `docs/README.md`
- Contribution guide: `CONTRIBUTING.md`

## Operational Notes

`sof` is not observer-only in gossip mode. By default it can also relay shreds and serve bounded repair responses.

To keep ingest/processing but reduce outward network activity:

- `SOF_UDP_RELAY_ENABLED=false`
- `SOF_REPAIR_ENABLED=false`

## Documentation

- Docs home: `docs/README.md`
- Architecture index: `docs/architecture/README.md`
- Operations index: `docs/operations/README.md`
- Runtime bootstrap modes: `docs/architecture/runtime-bootstrap-modes.md`
- Plugin hook model: `docs/architecture/framework-plugin-hooks.md`
- Runtime extension model: `docs/architecture/runtime-extension-hooks.md`
- Derived-state feed contract: `docs/architecture/derived-state-feed-contract.md`
- Transaction SDK ADR: `docs/architecture/adr/0006-transaction-sdk-and-dual-submit-routing.md`

## CI and Release

- CI workflow: `.github/workflows/ci.yml`
- Release workflow: `.github/workflows/release-crates.yml`
