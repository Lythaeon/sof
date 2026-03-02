# SOF Workspace

SOF is a Solana-focused Rust workspace with two user-facing crates:

1. `sof` (`crates/sof-observer`): low-latency observer runtime and active relay client for shred ingest, bounded cache/serve, shred propagation to peers, dataset reconstruction, plugin-driven events, local fork/reorg tracking, and tx commitment tagging (`processed`/`confirmed`/`finalized`) without RPC dependency.
2. `sof-tx` (`crates/sof-tx`): transaction SDK for building, signing, and submitting transactions with RPC/direct/hybrid routing.

## Pick Your Starting Point

- Building a runtime observer: see `crates/sof-observer/README.md`.
- Building/sending transactions: see `crates/sof-tx/README.md`.
- Contributing: see `CONTRIBUTING.md`.

## Quick Commands

Run observer example:

```bash
cargo run --release -p sof --example observer_runtime
```

Run observer with gossip bootstrap:

```bash
cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

## Relay Behavior

`sof` is not observer-only in gossip mode. By default, it also helps propagate shreds and serves bounded repair responses to strengthen the network.

To reduce/disable outward relay behavior:

- Disable UDP relay forwarding: `SOF_UDP_RELAY_ENABLED=false`
- Disable repair serving path: `SOF_REPAIR_ENABLED=false`

Using both disables active relay contribution while keeping observer ingest/processing.

Run SDK tests:

```bash
cargo test -p sof-tx
```

Run contributor quality gates:

```bash
cargo make ci
```

## Documentation Index

- Docs home: `docs/README.md`
- Architecture index: `docs/architecture/README.md`
- Operations index: `docs/operations/README.md`
- Runtime bootstrap modes: `docs/architecture/runtime-bootstrap-modes.md`
- Plugin hook model: `docs/architecture/framework-plugin-hooks.md`
- Transaction SDK ADR: `docs/architecture/adr/0006-transaction-sdk-and-dual-submit-routing.md`

## Release Notes

- Crate release workflow: `.github/workflows/release-crates.yml`
- CI workflow: `.github/workflows/ci.yml`
