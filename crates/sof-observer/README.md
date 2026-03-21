# sof

`sof` is the SOF observer/runtime crate.

It is built for low-latency Solana data ingest, local runtime-derived signals, and services that
need infrastructure-style control over replay, control-plane freshness, and bounded multicore
packet handling.

Core responsibilities:

- shred ingestion (direct UDP, relay, optional gossip bootstrap)
- optional shred verification
- dataset reconstruction and transaction extraction
- plugin-driven event hooks for custom logic
- local transaction commitment status tagging (`processed` / `confirmed` / `finalized`) without RPC dependency

## At a Glance

- Embed SOF directly inside a Tokio application
- Attach `Plugin` or `RuntimeExtension` consumers
- Run with built-in UDP ingress or external kernel-bypass ingress
- Treat SOF as a local market-data and control-plane engine, not just a passive observer
- Use packet-worker and dataset-worker fanout to keep multi-core hosts busy under sustained shred load
- Consume local slot/reorg/transaction/account-touch signals
- Use the replayable derived-state feed for restart-safe stateful consumers
- Apply typed gossip and ingest tuning profiles instead of env-string bundles
- Keep more runtime work on borrowed/shared data instead of eagerly allocating owned transaction or dataset payload copies
- Drop duplicate or conflicting shred observations before they can re-emit duplicate dataset or transaction events downstream

## Install

```bash
cargo add sof
```

Optional gossip bootstrap support at compile time:

```toml
sof = { version = "0.11.0", features = ["gossip-bootstrap"] }
```

Optional external `kernel-bypass` ingress support:

```toml
sof = { version = "0.11.0", features = ["kernel-bypass"] }
```

The bundled `sof-solana-gossip` backend defaults to SOF's lightweight in-memory duplicate/conflict
path. The heavier ledger-backed duplicate-shred tooling remains available behind the vendored
crate's explicit `solana-ledger` feature.

## Semantic Shred Dedupe

SOF now treats shred dedupe as a semantic correctness boundary, not just a packet-cache hint.

- One shared semantic shred registry is used across both ingest and canonical emission stages.
- Exact repeats are dropped before they can waste verify/FEC/reassembly work.
- Conflicting repeats are also suppressed before they can re-emit duplicate downstream events.
- The HFT/observer contract is that normal downstream consumers should not need their own duplicate
  shred suppression logic.

The shared registry publishes runtime telemetry for:

- current and max retained shred identities
- current and max eviction-queue depth
- capacity-driven vs expiry-driven evictions
- ingress duplicate/conflict drops
- canonical duplicate/conflict drops

Those metrics are intended to help tune `SOF_SHRED_DEDUP_CAPACITY` and
`SOF_SHRED_DEDUP_TTL_MS` under real traffic instead of guessing.

## Quick Start

Run the bundled runtime example:

```bash
cargo run --release -p sof --example observer_runtime
```

With gossip bootstrap:

```bash
cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

Basic programmatic setup:

```rust
use sof::runtime::{ObserverRuntime, RuntimeSetup};

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let setup = RuntimeSetup::new().with_startup_step_logs(true);

    ObserverRuntime::new()
        .with_setup(setup)
        .run_until_termination_signal()
        .await
}
```

## Runtime API

Embed directly in Tokio:

```rust
use sof::runtime::ObserverRuntime;

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    ObserverRuntime::new().run_until_termination_signal().await
}
```

Or use programmatic setup:

```rust
use std::net::SocketAddr;
use sof::runtime::{ObserverRuntime, RuntimeSetup};

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let setup = RuntimeSetup::new()
        .with_bind_addr(SocketAddr::from(([0, 0, 0, 0], 8001)))
        .with_observability_bind_addr(SocketAddr::from(([127, 0, 0, 1], 9108)))
        .with_startup_step_logs(true);

    ObserverRuntime::new()
        .with_setup(setup)
        .run_until_termination_signal()
        .await
}
```

When `SOF_OBSERVABILITY_BIND` (or `RuntimeSetup::with_observability_bind_addr`) is set, the
packaged runtime also serves:

- `/metrics`
- `/healthz`
- `/readyz`

Or apply one typed gossip/ingest profile instead of stringly env overrides:

```rust
use sof::runtime::{ObserverRuntime, RuntimeSetup};
use sof_gossip_tuning::{GossipTuningProfile, HostProfilePreset};

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let setup = RuntimeSetup::new()
        .with_gossip_tuning_profile(GossipTuningProfile::preset(HostProfilePreset::Vps));

    ObserverRuntime::new()
        .with_setup(setup)
        .run_until_termination_signal()
        .await
}
```

Linux busy-poll is available as an explicit host-side experiment when you want to trade CPU
efficiency for steadier UDP receive behavior:

```rust
use sof::runtime::ObserverRuntime;
use sof_gossip_tuning::{GossipTuningProfile, HostProfilePreset};

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let setup = sof::runtime::RuntimeSetup::new()
        .with_gossip_tuning_profile(GossipTuningProfile::preset(HostProfilePreset::Vps))
        .with_udp_busy_poll_us(50)
        .with_udp_busy_poll_budget(64)
        .with_udp_prefer_busy_poll(true);

    ObserverRuntime::new()
        .with_setup(setup)
        .run_until_termination_signal()
        .await
}
```

With external `kernel-bypass` ingress, feed `RawPacketBatch` values through SOF's ingress queue:

```rust
#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let (tx, rx) = sof::runtime::create_kernel_bypass_ingress_queue();
    // Publish batches from your bypass receiver thread:
    // let _ok = tx.send_batch(batch, false);
    // Spawn your kernel-bypass receiver and forward batches into `tx`.
    sof::runtime::run_async_with_kernel_bypass_ingress(rx).await
}
```

Run the kernel-bypass ingress metrics example for 180 seconds:

```bash
SOF_KERNEL_BYPASS_EXAMPLE_DURATION_SECS=180 \
  cargo run --release -p sof --example kernel_bypass_ingress_metrics --features kernel-bypass
```

Run the same example against live Solana gossip traffic (real chain data):

```bash
SOF_KERNEL_BYPASS_EXAMPLE_SOURCE=gossip \
SOF_KERNEL_BYPASS_EXAMPLE_DURATION_SECS=180 \
RUST_LOG=info \
  cargo run --release -p sof --example kernel_bypass_ingress_metrics --features "kernel-bypass gossip-bootstrap"
```

Run AF_XDP external-ingress example (requires Linux, AF_XDP-capable NIC setup, and privileges to create XDP sockets/programs):

```bash
SOF_AF_XDP_IFACE=enp17s0 \
SOF_AF_XDP_EXAMPLE_DURATION_SECS=180 \
  cargo run --release -p sof --example af_xdp_kernel_bypass_ingress_metrics --features "kernel-bypass gossip-bootstrap"
```

Notes for high-ingest runs:

- The example configures `SOF_PORT_RANGE=12000-12100` and `SOF_GOSSIP_PORT=8001`.
- It defaults live gossip mode to `SOF_INGEST_QUEUE_MODE=lockfree` with `SOF_INGEST_QUEUE_CAPACITY=262144`.
- The bundled gossip backend also exposes `SOF_GOSSIP_CONSUME_THREADS`, `SOF_GOSSIP_LISTEN_THREADS`, `SOF_GOSSIP_SOCKET_CONSUME_PARALLEL_PACKET_THRESHOLD`, `SOF_GOSSIP_LISTEN_PARALLEL_BATCH_THRESHOLD`, `SOF_GOSSIP_LISTEN_PARALLEL_MESSAGE_THRESHOLD`, and `SOF_GOSSIP_STATS_INTERVAL_SECS` for host-specific tuning.
- `SOF_UDP_DROP_ON_CHANNEL_FULL` only applies to SOF's built-in UDP receiver path (non-external ingress).
- Queue mode is configurable with `SOF_INGEST_QUEUE_MODE`:
  - `bounded` (default): Tokio bounded channel.
  - `unbounded`: Tokio unbounded channel (no backpressure drops; memory grows with load).
  - `lockfree`: lock-free `ArrayQueue` ring + async wakeups.
- Ring/bounded capacity is configurable with `SOF_INGEST_QUEUE_CAPACITY` (default `16384`).

## Plugin Quickstart

```rust
use async_trait::async_trait;
use sof::{
    event::TxKind,
    framework::{Plugin, PluginConfig, PluginHost, TransactionEvent},
    runtime::ObserverRuntime,
};

#[derive(Clone, Copy, Debug, Default)]
struct NonVoteLogger;

#[async_trait]
impl Plugin for NonVoteLogger {
    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction()
    }

    async fn on_transaction(&self, event: &TransactionEvent) {
        if event.kind == TxKind::VoteOnly {
            return;
        }
        tracing::info!(slot = event.slot, kind = ?event.kind, "transaction observed");
    }
}

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let host = PluginHost::builder().add_plugin(NonVoteLogger).build();

    ObserverRuntime::new()
        .with_plugin_host(host)
        .run_until_termination_signal()
        .await
}
```

For sparse plugin subscriptions, prefer `PluginConfig::new().with_*()` so the enabled hooks stand
out clearly. Use a raw `PluginConfig { .. }` literal only when many flags are enabled and the full
shape is easier to scan.

## RuntimeExtension Quickstart

```rust
use async_trait::async_trait;
use sof::framework::{
    ExtensionCapability, ExtensionContext, ExtensionManifest, PacketSubscription,
    RuntimeExtension, RuntimeExtensionHost, RuntimePacketSourceKind,
};
use sof::runtime::ObserverRuntime;

#[derive(Debug, Clone, Copy)]
struct IngressExtension;

#[async_trait]
impl RuntimeExtension for IngressExtension {
    async fn setup(
        &self,
        _ctx: ExtensionContext,
    ) -> Result<ExtensionManifest, sof::framework::extension::ExtensionSetupError> {
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

    ObserverRuntime::new()
        .with_extension_host(host)
        .run_until_termination_signal()
        .await
}
```

## Plugin Hooks

Current hook set:

- `on_raw_packet`
- `on_shred`
- `on_dataset`
- `on_transaction`
- `on_account_touch`
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

`on_account_touch` events include transaction-derived static account-key sets:

- `account_keys`
- `writable_account_keys`
- `readonly_account_keys`
- `lookup_table_account_keys`

This hook is for account discovery/invalidation. It is not a validator post-write account-update feed.

## Derived-State Consumers

SOF also exposes a replayable derived-state feed intended for stateful official extensions and local consumers that need:

- retained feed continuity
- checkpoint persistence
- replay-based recovery after restart or transient failure
- explicit resync/rebuild signaling
- typed control-plane replay for recent blockhash, cluster topology, and leader schedule inputs
- canonical control-plane quality snapshots through `ControlPlaneStateUpdated`
- invalidation and tx-feedback events through `StateInvalidated` and `TxOutcomeObserved`

This is the right substrate for local service layers that want to build a bank, query index, or gRPC stream on top of SOF without depending on validator-native Geyser.

Example implementation:

- `examples/derived_state_slot_mirror.rs`

Replay retention modes:

- `DerivedStateReplayConfig::checkpoint_only()` disables the runtime-owned replay tail and keeps recovery checkpoint-driven.
- `DerivedStateReplayBackend::Disk` retains envelopes on disk without keeping a full in-process mirror of the retained tail.

Design references:

- `../../docs/architecture/derived-state-extension-contract.md`
- `../../docs/architecture/derived-state-feed-contract.md`

`on_slot_status` events include local canonical transitions:

- `processed`
- `confirmed`
- `finalized`
- `orphaned`

## Operational Notes

- Hooks are dispatched off the ingest hot path through a bounded queue.
- Queue pressure drops hook events instead of stalling ingest.
- Typed host tuning is available through `sof-gossip-tuning` and `RuntimeSetup::with_gossip_tuning_profile(...)`.
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
- `derived_state_slot_mirror`
- `kernel_bypass_ingress_metrics` (`--features kernel-bypass`)

Run kernel-bypass ingress E2E test:

```bash
cargo test -p sof --features kernel-bypass --test kernel_bypass_ingress_e2e -- --nocapture
```

Run any example:

```bash
cargo run --release -p sof --example observer_with_multiple_plugins
```

## Docs

- Workspace docs index: `../../docs/README.md`
- Architecture docs: `../../docs/architecture/README.md`
- Operations docs: `../../docs/operations/README.md`
- Derived-state feed contract: `../../docs/architecture/derived-state-feed-contract.md`
- Reverse-engineering notes: `REVERSE_ENGINEERING.md`
- Contribution guide: `../../CONTRIBUTING.md`
