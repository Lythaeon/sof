# Workspace Layout

This page is maintainer-facing. If you are only consuming SOF, you can skip it and stay on the
[Use SOF](../use-sof/README.md) track.

The repository is a Rust workspace with three published crates and one vendored internal backend.

## High-Level Map

| Path | Role |
| --- | --- |
| `crates/sof-observer` | Published as `sof`; packaged runtime, ingest pipeline, plugin and extension framework |
| `crates/sof-tx` | Transaction SDK for construction, routing, signing, and submission |
| `crates/sof-gossip-tuning` | Typed host tuning profiles applied through `sof::runtime::RuntimeSetup` |
| `crates/sof-solana-gossip` | Vendored Solana gossip backend used by `sof` when `gossip-bootstrap` is enabled |
| `docs/architecture` | ADRs, ARDs, and deeper design contracts |
| `docs/operations` | Runbooks and advanced operator notes |
| `docs/gitbook` | This documentation website |
| `scripts` | Local helper scripts and architecture checks |

## How The Runtime Code Is Split

Inside `crates/sof-observer/src`, the major slices are:

- `ingest`: packet intake and receiver plumbing
- `shred`: wire parsing and FEC support
- `verify`: optional shred verification
- `reassembly`: dataset reconstruction
- `relay`: bounded relay cache and forwarding
- `repair`: bounded repair logic and peer tracking
- `framework`: plugin host, runtime extension host, and derived-state interfaces
- `app/runtime`: packaged composition and runloop wiring

The codebase intentionally keeps hot-path logic away from broad cross-module coupling. Cross-slice
composition belongs in the runtime and infra layers rather than inside leaf modules.

## How The Crates Fit Together

```text
        live Solana traffic / external ingress
                     |
                     v
                  sof runtime
         +--------+--------+---------+
         |                 |         |
         v                 v         v
    plugin hooks   runtime extensions   local state / replay
         |                 |         |
         +--------+--------+         |
                  v                  v
          downstream services   sof-tx adapters
                                     |
                                     v
                           transaction submission
```

## Documentation Ownership

Use this split when you are deciding where a change should be documented:

- crate behavior or public API: this GitBook plus crate README updates
- architectural constraints: `docs/architecture/adr/` or `docs/architecture/ard/`
- deployment and tuning guidance: `docs/operations/`
- contributor workflow or repository policy: `CONTRIBUTING.md`
