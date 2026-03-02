# sof

`sof` is a low-latency Solana observer runtime focused on:

- shred ingestion (direct UDP, relay, optional gossip bootstrap)
- optional shred verification
- dataset reconstruction and transaction extraction
- plugin-driven event hooks for custom logic
- local transaction commitment status tagging (`processed` / `confirmed` / `finalized`) without RPC dependency

## Install

```bash
cargo add sof
```

Optional gossip bootstrap support at compile time:

```toml
sof = { version = "0.4", features = ["gossip-bootstrap"] }
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

## RuntimeExtension Quickstart

```rust
use async_trait::async_trait;
use sof::framework::{
    ExtensionCapability, ExtensionManifest, ExtensionStartupContext, PacketSubscription,
    RuntimeExtension, RuntimeExtensionHost, RuntimePacketSourceKind,
};

#[derive(Debug, Clone, Copy)]
struct IngressExtension;

#[async_trait]
impl RuntimeExtension for IngressExtension {
    async fn on_startup(
        &self,
        _ctx: ExtensionStartupContext,
    ) -> Result<ExtensionManifest, sof::framework::extension::ExtensionStartupError> {
        Ok(ExtensionManifest {
            capabilities: vec![ExtensionCapability::ObserveObserverIngress],
            resources: Vec::new(),
            subscriptions: vec![PacketSubscription {
                source_kind: Some(RuntimePacketSourceKind::ObserverIngress),
                ..PacketSubscription::default()
            }],
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let host = RuntimeExtensionHost::builder()
        .add_extension(IngressExtension)
        .build();
    sof::runtime::run_async_with_extension_host(host).await
}
```

## Plugin Hooks

Current hook set:

- `on_raw_packet`
- `on_shred`
- `on_dataset`
- `on_transaction`
- `on_slot_status`
- `on_reorg`
- `on_recent_blockhash`
- `on_cluster_topology` (gossip-bootstrap mode)
- `on_leader_schedule` (gossip-bootstrap mode)

`on_transaction` events include:

- `commitment_status`
- `confirmed_slot`
- `finalized_slot`

These commitment fields are derived from local shred-stream slot progress (depth-based), not RPC polling.

`on_slot_status` events include local canonical transitions:

- `processed`
- `confirmed`
- `finalized`
- `orphaned`

## Operational Notes

- Hooks are dispatched off the ingest hot path through a bounded queue.
- Queue pressure drops hook events instead of stalling ingest.
- `RuntimeExtension` WebSocket connectors support full `ws://` / `wss://` handshake + frame decoding.
- WebSocket close frames emit `RuntimePacketEventClass::ConnectionClosed` in `on_packet_received`.
- WebSocket packet events expose `websocket_frame_type` (`Text`/`Binary`/`Ping`/`Pong`) for startup-time filtering and runtime routing.
- In gossip mode, SOF runs as an active bounded relay client by default (UDP relay + repair serve), not as an observer-only passive consumer.
- `SOF_LIVE_SHREDS_ENABLED=false` enables control-plane-only mode.

## Examples

- `observer_runtime`
- `observer_with_non_vote_plugin`
- `observer_with_multiple_plugins`
- `non_vote_tx_logger`
- `raydium_contract`
- `tpu_leader_logger`
- `runtime_extension_observer_ingress`
- `runtime_extension_udp_listener`
- `runtime_extension_shared_stream`
- `runtime_extension_with_plugins`
- `runtime_extension_websocket_connector`

Run any example:

```bash
cargo run --release -p sof --example observer_with_multiple_plugins
```

## Docs

- Workspace docs index: `../../docs/README.md`
- Architecture docs: `../../docs/architecture/README.md`
- Operations docs: `../../docs/operations/README.md`
- Contribution guide: `../../CONTRIBUTING.md`
