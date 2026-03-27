# First Runtime Bring-Up

Use this page to prove that `sof` starts, stays healthy, and can feed one small piece of service
logic.

Do not treat this page as a latency guide. First bring-up is about correctness and posture, not
about winning a shred race.

## Start With The Simplest Runtime

From a repository checkout:

```bash
cargo run --release -p sof --example observer_runtime
```

That uses the direct UDP path. It is the simplest runtime bring-up and the easiest way to verify
that the host, logging, and runtime lifecycle all behave the way you expect.

## Add Gossip Only When You Need Gossip

If you want cluster discovery and the active gossip-backed bootstrap path:

```bash
cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

That changes more than startup:

- SOF can discover peers
- SOF can participate in bounded relay
- SOF can participate in bounded repair

Use this because you need those semantics, not because you assume it is the fastest mode.

## Embed It In Your Own App

The normal embedded entry point is `ObserverRuntime` plus `RuntimeSetup`.

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

Useful setup helpers include:

- `with_bind_addr(...)`
- `with_gossip_entrypoints(...)`
- `with_gossip_validators(...)`
- `with_gossip_tuning_profile(...)`
- `with_derived_state_config(...)`
- `with_packet_workers(...)`
- `with_dataset_workers(...)`

## The Right First Progression

This order usually prevents confusion:

1. start the runtime
2. make startup explicit in code with `RuntimeSetup`
3. attach one plugin and prove you receive events
4. only then choose between direct UDP, gossip, private raw ingress, or processed providers for
   the real deployment

That separates:

- "SOF starts"
- "my service consumes events"
- "my ingress choice is correct"

## Minimal Plugin Bring-Up

```rust
use async_trait::async_trait;
use sof::{
    event::TxKind,
    framework::{Plugin, PluginConfig, PluginHost, TransactionEvent, TxCommitmentStatus},
    runtime::{ObserverRuntime, RuntimeSetup},
};

#[derive(Clone, Copy, Debug, Default)]
struct NonVoteLogger;

#[async_trait]
impl Plugin for NonVoteLogger {
    fn config(&self) -> PluginConfig {
        PluginConfig::new()
            .with_transaction()
            .at_commitment(TxCommitmentStatus::Confirmed)
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

    ObserverRuntime::new()
        .with_setup(setup)
        .with_plugin_host(host)
        .run_until_termination_signal()
        .await
}
```

If this works, the runtime is already useful. You have proven lifecycle, dispatch, and one piece
of application logic.

## First Operational Knobs

For first bring-up, keep it narrow:

- `RUST_LOG`
- `SOF_BIND`
- `SOF_GOSSIP_ENTRYPOINT` only if you are deliberately testing gossip

If you want gossip but with a quieter first posture:

- `SOF_UDP_RELAY_ENABLED=false`
- `SOF_REPAIR_ENABLED=false`

Do not start by tuning queue counts, worker counts, or repair pacing unless you are already
measuring a specific problem.

## What Success Looks Like

Success on day one is simple:

- the runtime starts and stays healthy
- one plugin receives the event family you expected
- you can clearly tell which ingress mode is active

What is not required:

- best-possible coverage
- final tuning values
- using gossip immediately
- proving a latency advantage before you have chosen the right ingress
