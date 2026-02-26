# sof

`sof` is a low-latency Solana observer runtime focused on:

- shred ingestion (direct UDP, relay, optional gossip bootstrap)
- optional shred verification
- dataset reconstruction and transaction extraction
- plugin-driven event hooks for custom logic

## Install

```bash
cargo add sof
```

Optional gossip bootstrap support at compile time:

```toml
sof = { version = "0.2", features = ["gossip-bootstrap"] }
```

## Quick Start

Run the bundled runtime example:

```bash
cargo run --release -p sof --example observer_runtime
```

With gossip bootstrap:

```bash
cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

## Runtime API

Embed directly in Tokio:

```rust
#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    sof::runtime::run_async().await
}
```

Or use programmatic setup:

```rust
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let setup = sof::runtime::RuntimeSetup::new()
        .with_bind_addr(SocketAddr::from(([0, 0, 0, 0], 8001)))
        .with_startup_step_logs(true);
    sof::runtime::run_async_with_setup(&setup).await
}
```

## Plugin Quickstart

```rust
use async_trait::async_trait;
use sof::{
    event::TxKind,
    framework::{Plugin, PluginHost, TransactionEvent},
};

#[derive(Clone, Copy, Debug, Default)]
struct NonVoteLogger;

#[async_trait]
impl Plugin for NonVoteLogger {
    async fn on_transaction(&self, event: TransactionEvent) {
        if event.kind == TxKind::VoteOnly {
            return;
        }
        tracing::info!(slot = event.slot, kind = ?event.kind, "transaction observed");
    }
}

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let host = PluginHost::builder().add_plugin(NonVoteLogger).build();
    sof::runtime::run_async_with_plugin_host(host).await
}
```

## Plugin Hooks

Current hook set:

- `on_raw_packet`
- `on_shred`
- `on_dataset`
- `on_transaction`
- `on_recent_blockhash`
- `on_cluster_topology` (gossip-bootstrap mode)
- `on_leader_schedule` (gossip-bootstrap mode)

## Operational Notes

- Hooks are dispatched off the ingest hot path through a bounded queue.
- Queue pressure drops hook events instead of stalling ingest.
- `SOF_LIVE_SHREDS_ENABLED=false` enables control-plane-only mode.

## Examples

- `observer_runtime`
- `observer_with_non_vote_plugin`
- `observer_with_multiple_plugins`
- `non_vote_tx_logger`
- `raydium_contract`
- `tpu_leader_logger`

Run any example:

```bash
cargo run --release -p sof --example observer_with_multiple_plugins
```

## Docs

- Workspace docs index: `../../docs/README.md`
- Architecture docs: `../../docs/architecture/README.md`
- Operations docs: `../../docs/operations/README.md`
- Contribution guide: `../../CONTRIBUTING.md`
