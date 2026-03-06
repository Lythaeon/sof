# SOF

SOF is a Solana-focused Rust workspace for low-latency data ingest and transaction execution.

It is split into two user-facing crates:

- `sof`: observer/runtime crate for shred ingest, relay/cache, dataset reconstruction, plugin and runtime-extension events, fork/reorg tracking, and local commitment tagging without RPC dependency
- `sof-tx`: transaction SDK for building, signing, and submitting Solana transactions through RPC, direct leader routing, hybrid fallback, and optional kernel-bypass transports

## Highlights

- Low-latency shred ingest and dataset reconstruction
- Local `processed` / `confirmed` / `finalized` transaction tagging
- Plugin hooks and runtime extensions for downstream logic
- Optional gossip bootstrap and external kernel-bypass ingress
- Transaction submission with RPC, direct, hybrid, and kernel-bypass paths
- Derived-state replay and recovery support for stateful extensions

## Repository Layout

- `crates/sof-observer`: published as `sof`
- `crates/sof-tx`: published as `sof-tx`
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

Feature examples:

```toml
sof = { version = "0.6.0", features = ["gossip-bootstrap"] }
sof-tx = { version = "0.6.0", features = ["sof-adapters"] }
```

Kernel-bypass integrations:

- `sof` supports external ingress APIs through `--features kernel-bypass`
- `sof-tx` supports custom direct transport adapters through `--features kernel-bypass`

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

Run the full contributor gate:

```bash
cargo make ci
```

## Basic Setup Guides

- Observer/runtime setup: `crates/sof-observer/README.md`
- Transaction SDK setup: `crates/sof-tx/README.md`
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
