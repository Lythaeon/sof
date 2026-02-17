# SOF: Solana Observer Framework

SOF is a lightweight Solana observer engine for low-latency shred ingestion and transaction observation.

## What it gives you

- Live shred ingestion (gossip bootstrap, relay, or direct UDP sink).
- Shred parsing and optional verification.
- Dataset reassembly and transaction extraction.
- Plugin hook surface for custom event handling.

## Quick start

Pick the path you want and run one command.

```bash
cargo run --release -p sof --example observer_runtime
```

Run with gossip bootstrap:

```bash
cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

Run a ready-made plugin example (TPU leader logger):

```bash
cargo run --release -p sof --example tpu_leader_logger --features gossip-bootstrap
```

Optional verbose logs:

```bash
RUST_LOG=info cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

## Build and verify locally

- Fast compile/type check (does not produce a runnable binary):
  - `cargo check -p sof`
- Build release artifacts:
  - `cargo build --release -p sof`

## Embed with Tokio (`#[tokio::main]`)

```rust
#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    Ok(sof::runtime::run_async().await?)
}
```

Programmatic setup (instead of only env vars):

```rust
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let setup = sof::runtime::RuntimeSetup::new()
        .with_bind_addr(SocketAddr::from(([0, 0, 0, 0], 8001)))
        .with_gossip_entrypoints(["entrypoint.mainnet-beta.solana.com:8001"])
        .with_startup_step_logs(true);
    Ok(sof::runtime::run_async_with_setup(&setup).await?)
}
```

## Plugin framework quickstart

```rust
use async_trait::async_trait;
use sof::{
    event::TxKind,
    framework::{Plugin, PluginHost, TransactionEvent},
};

#[derive(Clone, Copy, Debug, Default)]
struct MyTxPlugin;

#[async_trait]
impl Plugin for MyTxPlugin {
    async fn on_transaction(&self, event: TransactionEvent) {
        if event.kind == TxKind::VoteOnly {
            return;
        }
        tracing::info!(slot = event.slot, kind = ?event.kind, "transaction observed");
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct MyDatasetPlugin;

#[async_trait]
impl Plugin for MyDatasetPlugin {}

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let host = PluginHost::builder()
        .add_plugin(MyTxPlugin)
        .add_plugin(MyDatasetPlugin)
        .build();
    Ok(sof::runtime::run_async_with_plugin_host(host).await?)
}
```

### Hook lifecycle (current: 7 hooks)

- `on_raw_packet`: every UDP packet before shred parsing.
- `on_shred`: every packet that produced a parsed shred header.
- `on_dataset`: every contiguous reconstructed dataset.
- `on_transaction`: every decoded transaction from reconstructed datasets.
- `on_recent_blockhash`: deduplicated observed recent blockhash updates from decoded datasets.
- `on_cluster_topology`: near-real-time (~250ms) gossip topology diffs plus periodic snapshots (gossip-bootstrap mode).
- `on_leader_schedule`: event-driven leader diffs emitted when slot-leader mappings change from live data (gossip-bootstrap mode).

This list must stay in sync with `sof::framework::ObserverPlugin`.

### API notes

- `Plugin` is an alias of `ObserverPlugin`. Both are valid.
- `Plugin::name()` is optional; default is `core::any::type_name::<Self>()`.
- Global observed-state reads on `PluginHost`:
  - `latest_observed_recent_blockhash()`
  - `latest_observed_tpu_leader()`
- Builder methods:
  - Preferred: `add_plugin`, `add_shared_plugin`, `add_plugins`, `add_shared_plugins`.
  - Compatibility aliases: `with_plugin`, `with_plugin_arc`, `with_plugins`, `with_plugin_arcs`.
  - The aliases are kept to avoid breaking older integrations while keeping the newer `add_*` API explicit.
  - Dispatch queue tuning: `with_event_queue_capacity`.
  - Dispatch strategy: `with_dispatch_mode(PluginDispatchMode::Sequential | PluginDispatchMode::BoundedConcurrent(N))`.

### Runtime guarantees

- Plugin hooks are async and off hot path by default.
- Runtime uses bounded queue + non-blocking enqueue to protect ingest latency.
- Queue pressure drops hook events (sampled warnings) instead of stalling ingest.
- `SOF_LIVE_SHREDS_ENABLED=false` switches to control-plane-only mode
  (topology hooks stay active; live shred data-plane hooks are skipped).

Examples are release-only and should be run with `--release`.

Runnable plugin+runtime examples:

- `cargo run --release -p sof --example observer_with_non_vote_plugin`
- `cargo run --release -p sof --example observer_with_multiple_plugins`

With gossip bootstrap:

```bash
cargo run --release -p sof --example observer_with_non_vote_plugin --features gossip-bootstrap
```

More plugin+runtime examples:

- `cargo run --release -p sof --example non_vote_tx_logger --features gossip-bootstrap`
- `cargo run --release -p sof --example raydium_contract --features gossip-bootstrap`
- `cargo run --release -p sof --example tpu_leader_logger --features gossip-bootstrap`

Default logging is `info,solana_metrics=off,solana_streamer=warn,solana_gossip=off` when `RUST_LOG` is unset.
If your shell exports `RUST_LOG=warn`, plugin `info` logs will be hidden.
By default, gossip peer/node chatter is suppressed (`solana_gossip=off` and
`SOF_LOG_REPAIR_PEER_TRAFFIC=false`).
`SOF_LOG_STARTUP_STEPS=true` by default, so startup/bootstrapping stages are logged out of the box.
Set `SOF_LOG_REPAIR_PEER_TRAFFIC=true` to re-enable sampled repair-peer request/ping logs.

With `--features gossip-bootstrap`, `SOF_GOSSIP_ENTRYPOINT` defaults to mainnet bootstrap endpoints:
`104.204.142.108:8001,64.130.54.173:8001,85.195.118.195:8001,160.202.131.177:8001`
with `entrypoint.mainnet-beta.solana.com:8001` as a final fallback.
To force direct UDP listener mode (`SOF_BIND`, default `0.0.0.0:8001`) without relay, set `SOF_GOSSIP_ENTRYPOINT` to an empty value.

## Release automation

Publishing is automated in `.github/workflows/release-crates.yml`.

- Trigger: push a semver tag like `v0.1.0`.
- Preflight trigger: run `Release Crate` via `workflow_dispatch` to execute release checks without publishing.
- Gate: tag version must match `crates/sof-observer/Cargo.toml` package version.
- Checks: runs `cargo make ci` and `cargo publish --dry-run` before publish.
- Secret required: `CARGO_REGISTRY_TOKEN` in repository settings.

## Contributor quality gates

Use these before opening a PR:

```bash
cargo make ci
```

What `ci` runs:

- `cargo make format-check`
- `cargo make arch-check` (enforces ARD slice boundaries)
- `cargo make clippy-matrix` (all-features and no-default-features)
- `cargo make test-matrix` (default and all-features tests)

For dependency-policy checks as well:

```bash
cargo make ci-full
```

GitHub Actions mirrors this in `.github/workflows/ci.yml` for every PR and pushes to `main`.

## Docs

- Contribution guide: `CONTRIBUTING.md`
- Operations overview: `docs/operations/README.md`
- Home-router/proxy ingestion playbook: `docs/operations/shred-ingestion-home-router-and-proxy.md`
- Advanced env controls (expert-only): `docs/operations/advanced-env.md`
- Architecture index: `docs/architecture/README.md`
- Runtime bootstrap modes (`gossip-bootstrap` vs direct/relay): `docs/architecture/runtime-bootstrap-modes.md`
- Plugin hooks: `docs/architecture/framework-plugin-hooks.md`
