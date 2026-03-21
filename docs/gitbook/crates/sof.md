# `sof`

`sof` is the packaged observer/runtime crate for low-latency shred ingestion and local
control-plane signals.

If the words “shred”, “relay”, or “repair” still feel too low-level right now, read
[Before You Start](../getting-started/before-you-start.md) before treating this page as your first
stop.

## Core Responsibilities

- packet ingress from direct UDP, gossip bootstrap, or external kernel-bypass receivers
- shred parse, optional verification, FEC recovery, and dataset reconstruction
- plugin-driven event emission for transactions, slots, topology, and blockhash observations
- runtime extension hosting for filtered packet/resource consumers
- bounded relay and repair behavior
- local canonical and commitment tracking without requiring an RPC dependency

You do not need to understand the raw shred format to use the crate well. What matters is that
`sof` starts from live packet traffic and turns it into higher-level local signals and events.

## Where It Fits

`sof` is the crate you use when you need to observe, derive, and expose local runtime state from
live Solana traffic.

Typical downstream uses:

- market-data or observer services consuming plugin events
- local control-plane producers feeding execution systems
- runtime-extension hosts that need managed packet or socket resources
- derived-state consumers that need replay and restart recovery

If your only goal is to build and submit transactions, start with `sof-tx` instead.

## When Not To Use It

`sof` is probably the wrong first dependency if:

- you only need transaction build and submit
- you already trust an external control-plane service and do not need local ingest
- you do not want to operate an observer/runtime process at all

## Public Entry Points

| Area | Start Here |
| --- | --- |
| Packaged runtime | `sof::runtime` |
| Plugins | `sof::framework::ObserverPlugin` and `PluginHost` |
| Runtime extensions | `sof::framework::RuntimeExtension` and `RuntimeExtensionHost` |
| Derived-state consumers | `sof::framework::DerivedStateConsumer` and `DerivedStateHost` |
| Event payloads | `sof::event` |

## The First Three Code Paths You Will Usually Need

### 1. Start the runtime

```rust
use sof::runtime::ObserverRuntime;

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    ObserverRuntime::new().run_until_termination_signal().await
}
```

Use this when you only need to prove the runtime starts on your host.

### 2. Start the runtime with explicit setup

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

Use this once you want bind address, gossip entrypoints, worker counts, or derived-state behavior
to be explicit in code.

You can also enable SOF's runtime-owned metrics and probe endpoint the same way:

```rust
use std::net::SocketAddr;
use sof::runtime::{ObserverRuntime, RuntimeSetup};

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let setup = RuntimeSetup::new()
        .with_observability_bind_addr(SocketAddr::from(([127, 0, 0, 1], 9108)));

    ObserverRuntime::new()
        .with_setup(setup)
        .run_until_termination_signal()
        .await
}
```

That endpoint exposes:

- `/metrics`
- `/healthz`
- `/readyz`

### 3. Start the runtime and consume one event stream

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

Use this when you are building a real service rather than only proving SOF can boot.

## Runtime Modes

### Direct UDP listener

Best for:

- local testing
- controlled private traffic sources
- deployments that do not need gossip bootstrap

### Gossip bootstrap

Best for:

- public ingress hosts
- deployments that need live topology and leader context
- relay and bounded repair participation

Build flag:

```toml
sof = { version = "0.11.0", features = ["gossip-bootstrap"] }
```

### External kernel-bypass ingress

Best for:

- custom AF_XDP or other specialized network receivers
- deployments that want SOF for downstream processing while owning the front-end NIC path

Build flag:

```toml
sof = { version = "0.11.0", features = ["kernel-bypass"] }
```

## When To Use Plugins vs Runtime Extensions

Use plugins when you want decoded semantic events:

- transactions
- datasets
- slot status
- reorgs
- blockhash or topology changes

Use runtime extensions when you need runtime-managed resources or filtered packet sources:

- UDP/TCP bind or connect
- WebSocket connections
- shared extension streams
- raw ingress packet observation before semantic decode

## Semantics That Matter In Production

### Dedupe is part of the correctness boundary

SOF treats duplicate and conflicting shred suppression as a semantic contract. Downstream consumers
should not need their own duplicate-shred filter just to keep event streams sane.

### Relay and repair are bounded

SOF is not attempting validator-style unbounded network behavior. Cache windows, fanout, and repair
budgets are explicit and intentionally conservative.

### Local control plane is first-class

The runtime surfaces locally derived slot progression, leader schedule data, recent blockhash
observations, and reorg state so downstream services can act without routing every decision through
RPC.

## Useful Examples

| Example | Why It Matters |
| --- | --- |
| `observer_runtime` | minimal packaged runtime |
| `observer_with_non_vote_plugin` | simplest plugin attachment |
| `observer_with_multiple_plugins` | multi-plugin host |
| `runtime_extension_observer_ingress` | raw ingress observation with extensions |
| `runtime_extension_websocket_connector` | runtime-managed WebSocket resource |
| `derived_state_slot_mirror` | replayable stateful-consumer pattern; requires `SOF_RUN_EXAMPLE=1` |
| `tpu_leader_logger` | local leader and TPU endpoint observation |
| `kernel_bypass_ingress_metrics` | external ingress handoff |

The quickest way forward is to open the example that matches the service being built:

- [`observer_runtime.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_runtime.rs)
- [`observer_with_non_vote_plugin.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_with_non_vote_plugin.rs)
- [`observer_with_multiple_plugins.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_with_multiple_plugins.rs)
- [`tpu_leader_logger.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/tpu_leader_logger.rs)

## A Good Mental Model For Implementation

The implementation loop is usually:

1. choose a runtime mode
2. choose whether your app logic is a plugin, runtime extension, or derived-state consumer
3. build the matching host
4. pass that host into the runtime entrypoint

In practice, the integration usually comes down to one concrete pattern: choose the host type
that matches your service, attach it to `ObserverRuntime::new()`, then run the composed runtime.

## Operational Baseline

For initial bring-up, set only:

- `RUST_LOG`
- `SOF_BIND`
- `SOF_GOSSIP_ENTRYPOINT` when using gossip bootstrap

Reduce outbound network activity if needed:

- `SOF_UDP_RELAY_ENABLED=false`
- `SOF_REPAIR_ENABLED=false`

Then move to the [Operations](../operations/README.md) section once the host is stable.
