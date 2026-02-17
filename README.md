# SOF: Solana Observer Framework

SOF is a lightweight Solana observer engine for low-latency shred ingestion and transaction observation.

## What it gives you

- Live shred ingestion (gossip bootstrap, relay, or direct UDP sink).
- Shred parsing and optional verification.
- Dataset reassembly and transaction extraction.
- Plugin hook surface for custom event handling.

## Install and build

```bash
cargo check -p sof
```

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

## Release automation

Publishing is automated in `.github/workflows/release-crates.yml`.

- Trigger: push a semver tag like `v0.1.0`.
- Preflight trigger: run `Release Crate` via `workflow_dispatch` to execute release checks without publishing.
- Gate: tag version must match `crates/sof-observer/Cargo.toml` package version.
- Checks: runs `cargo make ci` and `cargo publish --dry-run` before publish.
- Secret required: `CARGO_REGISTRY_TOKEN` in repository settings.

## Run the observer runtime

`sof` is library-first. Use the provided runtime example:

```bash
RUST_LOG=info cargo run --release -p sof --example observer_runtime
```

With gossip bootstrap:

```bash
SOF_GOSSIP_ENTRYPOINT=entrypoint.mainnet-beta.solana.com:8001 \
SOF_PORT_RANGE=12000-12100 \
RUST_LOG=info \
cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

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
use sof::{
    event::TxKind,
    framework::{Plugin, PluginHost, TransactionEvent},
};

#[derive(Clone, Copy, Debug, Default)]
struct MyTxPlugin;

impl Plugin for MyTxPlugin {
    fn on_transaction(&self, event: TransactionEvent<'_>) {
        if event.kind == TxKind::VoteOnly {
            return;
        }
        tracing::info!(slot = event.slot, kind = ?event.kind, "transaction observed");
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct MyDatasetPlugin;

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

### Hook lifecycle

- `on_raw_packet`: every UDP packet before shred parsing.
- `on_shred`: every packet that produced a parsed shred header.
- `on_dataset`: every contiguous reconstructed dataset.
- `on_transaction`: every decoded transaction from reconstructed datasets.

### API notes

- `Plugin` is an alias of `ObserverPlugin`. Both are valid.
- `Plugin::name()` is optional; default is `core::any::type_name::<Self>()`.
- Builder methods:
  - Primary: `add_plugin`, `add_shared_plugin`, `add_plugins`, `add_shared_plugins`.
  - Compatibility: `with_plugin`, `with_plugin_arc`, `with_plugins`, `with_plugin_arcs`.

### Performance contract for plugins

- Hooks run synchronously on hot paths.
- Avoid blocking I/O inside hook methods.
- Avoid expensive allocations in hook loops.
- For heavy processing, enqueue work into your own worker task.
- Hooks can run concurrently across runtime tasks; guard shared plugin state with atomics/locks.

Examples are release-only and should be run with `--release`.

Runnable plugin+runtime examples:

- `cargo run --release -p sof --example observer_with_non_vote_plugin`
- `cargo run --release -p sof --example observer_with_multiple_plugins`

With gossip bootstrap:

```bash
SOF_GOSSIP_ENTRYPOINT=entrypoint.mainnet-beta.solana.com:8001 \
SOF_PORT_RANGE=12000-12100 \
RUST_LOG=info \
cargo run --release -p sof --example observer_with_non_vote_plugin --features gossip-bootstrap
```

More plugin+runtime examples:

- `SOF_GOSSIP_ENTRYPOINT=entrypoint.mainnet-beta.solana.com:8001 SOF_PORT_RANGE=12000-12100 RUST_LOG=info cargo run --release -p sof --example non_vote_tx_logger --features gossip-bootstrap`
- `SOF_GOSSIP_ENTRYPOINT=entrypoint.mainnet-beta.solana.com:8001 SOF_PORT_RANGE=12000-12100 RUST_LOG=info cargo run --release -p sof --example raydium_contract --features gossip-bootstrap`

You can also run both examples without extra env vars:

- `cargo run --release -p sof --example non_vote_tx_logger --features gossip-bootstrap`
- `cargo run --release -p sof --example raydium_contract --features gossip-bootstrap`

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

## Docs

- Contribution guide: `CONTRIBUTING.md`
- Operations overview: `docs/operations/README.md`
- Home-router/proxy ingestion playbook: `docs/operations/shred-ingestion-home-router-and-proxy.md`
- Advanced env controls (expert-only): `docs/operations/advanced-env.md`
- Architecture index: `docs/architecture/README.md`
- Runtime bootstrap modes (`gossip-bootstrap` vs direct/relay): `docs/architecture/runtime-bootstrap-modes.md`
- Plugin hooks: `docs/architecture/framework-plugin-hooks.md`
