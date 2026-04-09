# SOF Docs

This folder is the documentation index for the SOF workspace.

Use it as the entry point for architecture references, operations guides, and crate-level setup docs.
The project is aimed at low-latency Solana infrastructure: ingest, replayable state, execution
control planes, and deployment/tuning concerns that matter in real trading and market-data systems.

For a locally previewable documentation website, use `docs/gitbook/`:

```bash
cd docs/gitbook
npm install
npm run serve
```

## Start Here

Observer/runtime users:

- `../crates/sof-observer/README.md`
- `operations/README.md`

Transaction SDK users:

- `../crates/sof-tx/README.md`
- `architecture/adr/0006-transaction-sdk-and-dual-submit-routing.md`

Contributors:

- `../CONTRIBUTING.md`
- `architecture/README.md`
- `gitbook/README.md`

## Architecture Docs

- `architecture/README.md`: architecture index and ADR/ARD scope
- `gitbook/architecture/ts-and-python-integration.md`: planned TS/Python authoring model, typed
  bridge contract, and phased next plan
- `architecture/framework-plugin-hooks.md`: observer plugin hooks and event contracts
- `architecture/runtime-extension-hooks.md`: runtime extension capabilities and filtered ingress model
- `architecture/runtime-bootstrap-modes.md`: runtime bootstrap and ingress modes
- `architecture/derived-state-extension-contract.md`: stateful extension contract
- `architecture/derived-state-feed-contract.md`: replayable derived-state feed contract

## Operations Docs

- `operations/README.md`: runbook index
- `operations/advanced-env.md`: advanced environment variables and tuning knobs
- `operations/shred-ingestion-home-router-and-proxy.md`: deployment guide for public ingress hosts

## Crate Docs

- `../crates/sof-observer/README.md`: runtime/observer setup, examples, and features
- `../crates/sof-tx/README.md`: transaction SDK setup, routing, and transport usage
- `../crates/sof-gossip-tuning/README.md`: typed gossip/ingest tuning presets for SOF hosts
