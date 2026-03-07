# SOF Docs

This folder is the documentation index for the SOF workspace.

Use it as the entry point for architecture references, operations guides, and crate-level setup docs.

## Start Here

Observer/runtime users:

- `../crates/sof-observer/README.md`
- `operations/README.md`

Transaction SDK users:

- `../crates/sof-tx/README.md`
- `architecture/adr/0006-transaction-sdk-and-dual-submit-routing.md`
- `architecture/toxic-flow-todo.md`

Contributors:

- `../CONTRIBUTING.md`
- `architecture/README.md`

## Architecture Docs

- `architecture/README.md`: architecture index and ADR/ARD scope
- `architecture/framework-plugin-hooks.md`: observer plugin hooks and event contracts
- `architecture/runtime-extension-hooks.md`: runtime extension capabilities and filtered ingress model
- `architecture/runtime-bootstrap-modes.md`: runtime bootstrap and ingress modes
- `architecture/derived-state-extension-contract.md`: stateful extension contract
- `architecture/derived-state-feed-contract.md`: replayable derived-state feed contract
- `architecture/toxic-flow-todo.md`: implemented toxic-flow freshness, invalidation, and tx guard substrate

## Operations Docs

- `operations/README.md`: runbook index
- `operations/advanced-env.md`: advanced environment variables and tuning knobs
- `operations/shred-ingestion-home-router-and-proxy.md`: deployment guide for public ingress hosts

## Crate Docs

- `../crates/sof-observer/README.md`: runtime/observer setup, examples, and features
- `../crates/sof-tx/README.md`: transaction SDK setup, routing, and transport usage
- `../crates/sof-gossip-tuning/README.md`: typed gossip/ingest tuning presets for SOF hosts
