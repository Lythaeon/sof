# SOF Docs

This folder contains architecture and operations documentation for the SOF workspace.

## Where To Read First

1. Runtime observer users:
   - `../crates/sof-observer/README.md`
   - `operations/README.md`
2. Transaction SDK users:
   - `../crates/sof-tx/README.md`
   - `architecture/adr/0006-transaction-sdk-and-dual-submit-routing.md`
3. Contributors:
   - `../CONTRIBUTING.md`
   - `architecture/README.md`

## Architecture

- `architecture/README.md`: ADR/ARD index and scope.
- `architecture/framework-plugin-hooks.md`: plugin hook contracts and semantics.
- `architecture/runtime-extension-hooks.md`: runtime extension capabilities, resources, and filter semantics.
- `architecture/runtime-bootstrap-modes.md`: runtime capability profiles and source selection.

## Operations

- `operations/README.md`: operational runbook index.
- `operations/advanced-env.md`: expert tuning knobs and guardrails.
- `operations/shred-ingestion-home-router-and-proxy.md`: home-router/proxy deployment guide.
