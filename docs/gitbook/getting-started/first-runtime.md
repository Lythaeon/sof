# First Runtime Bring-Up

This page is the shortest path to running `sof` locally and understanding which mode you are in.

## Try It From This Repository

If you are evaluating SOF from a repository checkout, start with the packaged examples.

### Simplest Runtime Start

Run the packaged example:

```bash
cargo run --release -p sof --example observer_runtime
```

That path uses SOF's direct UDP listener mode.

### Enable Gossip Bootstrap

If you want SOF to discover cluster peers and run the gossip-backed bootstrap path, build with the
feature enabled:

```bash
cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

Important runtime effect:

- without `gossip-bootstrap`, `sof` remains a local observer/runtime
- with `gossip-bootstrap`, SOF can also operate as an active relay and bounded repair client

## Embed It In Your Own App

If you are consuming `sof` as a crate, start with `RuntimeSetup` or the plain runtime entrypoints.

### Programmatic Setup

If you are embedding `sof` in your own app, you will usually want `RuntimeSetup` instead of
stuffing startup behavior into environment strings.

```rust
#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let setup = sof::runtime::RuntimeSetup::new()
        .with_startup_step_logs(true);
    sof::runtime::run_async_with_setup(&setup).await
}
```

Useful setup helpers include:

- `with_bind_addr(...)`
- `with_gossip_entrypoints(...)`
- `with_gossip_validators(...)`
- `with_gossip_tuning_profile(...)`
- `with_derived_state_config(...)`
- `with_packet_workers(...)`
- `with_dataset_workers(...)`

### The First Useful Progression

If you are new to the crate, this is the progression you will usually want:

1. make `run_async()` compile and start
2. switch to `RuntimeSetup` so startup is explicit in code
3. attach one plugin and prove you can consume events
4. only then move into gossip mode, runtime extensions, or derived-state consumers

That progression matters because it separates “SOF starts” from “my app logic is wired correctly”.

### A Minimal Plugin-Backed Bring-Up

If you need to validate that your service can actually consume SOF output, start here instead of
stopping at a bare runtime:

```rust
use async_trait::async_trait;
use sof::{
    event::TxKind,
    framework::{Plugin, PluginHost, TransactionEvent},
    runtime::RuntimeSetup,
};

#[derive(Clone, Copy, Debug, Default)]
struct NonVoteLogger;

#[async_trait]
impl Plugin for NonVoteLogger {
    fn wants_transaction(&self) -> bool {
        true
    }

    async fn on_transaction(&self, event: &TransactionEvent) {
        if event.kind == TxKind::VoteOnly {
            return;
        }
        tracing::info!(slot = event.slot, "non-vote transaction observed");
    }
}

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let setup = RuntimeSetup::new().with_startup_step_logs(true);
    let host = PluginHost::builder().add_plugin(NonVoteLogger).build();
    sof::runtime::run_async_with_plugin_host_and_setup(host, &setup).await
}
```

If this works, you have crossed the line from “you can launch SOF” to “you can integrate SOF”.

## First Operational Knobs

Use only these when you are bringing up a host for the first time:

- `RUST_LOG`
- `SOF_BIND`
- `SOF_GOSSIP_ENTRYPOINT` for gossip mode

If you need less outbound activity while keeping ingest active:

- `SOF_UDP_RELAY_ENABLED=false`
- `SOF_REPAIR_ENABLED=false`

Do not start with the advanced queue, repair, or gossip thread knobs unless you are already
measuring a concrete problem.

## Examples Worth Trying

- `observer_runtime`: base runtime bring-up
- `observer_with_non_vote_plugin`: plugin wiring
- `observer_with_multiple_plugins`: what a more realistic plugin host looks like
- `runtime_extension_websocket_connector`: extension resource model
- `derived_state_slot_mirror`: derived-state style consumer example; run it with
  `SOF_RUN_EXAMPLE=1` because it is intentionally guarded
- `kernel_bypass_ingress_metrics`: external ingress path

If you are unsure where to look next, open the example source before reading deeper operations
pages:

- [`observer_runtime.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_runtime.rs)
- [`observer_with_non_vote_plugin.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_with_non_vote_plugin.rs)
- [`tpu_leader_logger.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/tpu_leader_logger.rs)
