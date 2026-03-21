# First Runtime Bring-Up

Use this page to get `sof` running locally and to understand which runtime mode is active.

If terms like shreds, relay, repair, or leader schedule still feel fuzzy, read
[Before You Start](before-you-start.md) first. The rest of this page assumes the basic shape
of what SOF is doing on the network.

## Try It From This Repository

From a repository checkout, the packaged examples are the fastest starting point.

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

For crate consumers, start with `RuntimeSetup` or the plain runtime entrypoints.

### Programmatic Setup

Embedded services usually want `RuntimeSetup` instead of pushing startup behavior into environment
strings.

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

For a first real integration, this progression usually works well:

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
    framework::{Plugin, PluginConfig, PluginHost, TransactionEvent},
    runtime::RuntimeSetup,
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

If this works, the runtime is no longer just launching; it is already feeding service logic.

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

## What Success Looks Like

For a first bring-up, success is usually one of these:

- the runtime starts and stays healthy for a few minutes
- your plugin logs the event type you expected
- you can clearly tell whether you are in direct UDP mode or gossip mode

What is not required on day one:

- perfect packet coverage
- final tuning values
- understanding every environment variable
- jumping immediately into relay/repair tuning

## Examples Worth Trying

- `observer_runtime`: base runtime bring-up
- `observer_with_non_vote_plugin`: plugin wiring
- `observer_with_multiple_plugins`: what a more realistic plugin host looks like
- `runtime_extension_websocket_connector`: extension resource model
- `derived_state_slot_mirror`: derived-state style consumer example; run it with
  `SOF_RUN_EXAMPLE=1` because it is intentionally guarded
- `kernel_bypass_ingress_metrics`: external ingress path

If the next step is still unclear, open the example source before reading deeper operations pages:

- [`observer_runtime.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_runtime.rs)
- [`observer_with_non_vote_plugin.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_with_non_vote_plugin.rs)
- [`observer_with_multiple_plugins.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_with_multiple_plugins.rs)
- [`tpu_leader_logger.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/tpu_leader_logger.rs)
