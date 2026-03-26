# Build An Observer Service

Start here when you want one service that ingests live Solana traffic and emits your own
application-level events, metrics, or logs.

This is the lowest-friction real product shape for `sof`.

Low-friction does not automatically mean lowest-latency. If latency is the reason you are here,
the service also needs early shred visibility from either direct low-latency validator or peer
access, or from an external shred propagation network. SOF's efficient local processing only pays
off once ingress is early enough to matter.

## Use This When

- you need transaction or slot observation
- you want local ingest without transaction submission in the same process
- you want to prove your app logic before introducing more moving parts

## What Runs In This Service

At minimum:

- one `PluginHost`
- one or more plugins with your application logic
- one `sof` runtime entrypoint

That means your service shape is:

1. define a plugin
2. attach it to a plugin host
3. pass the host into `sof::runtime`

## Minimal Service Skeleton

```rust
use async_trait::async_trait;
use sof::{
    event::TxKind,
    framework::{Plugin, PluginConfig, PluginHost, TransactionEvent, TxCommitmentStatus},
    runtime::ObserverRuntime,
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
        tracing::info!(slot = event.slot, kind = ?event.kind, "non-vote transaction observed");
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

Use `.at_commitment(...)` when the service should ignore lower-confidence traffic,
or `.only_at_commitment(...)` when it should react only to one exact commitment.

This is the right first service because it proves all of the important boundaries:

- SOF can start on your host
- your service can receive decoded events
- your application logic can stay outside the runtime core

If the plugin needs initialization or cleanup, add `setup(ctx)` and `shutdown(ctx)`.
The packaged runtime invokes both automatically when the host is attached.

For HFT-style transaction services, prefer the explicit inline transaction mode:

```rust
use async_trait::async_trait;
use sof::{
    event::TxKind,
    framework::{Plugin, PluginConfig, PluginHost, TransactionDispatchMode, TransactionEvent},
    runtime::ObserverRuntime,
};

#[derive(Clone, Copy, Debug, Default)]
struct InlineTxLogger;

#[async_trait]
impl Plugin for InlineTxLogger {
    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction_mode(TransactionDispatchMode::Inline)
    }

    async fn on_transaction(&self, event: &TransactionEvent) {
        if event.kind == TxKind::VoteOnly {
            return;
        }
        tracing::info!(slot = event.slot, kind = ?event.kind, "inline transaction observed");
    }
}

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let host = PluginHost::builder().add_plugin(InlineTxLogger).build();

    ObserverRuntime::new()
        .with_plugin_host(host)
        .run_until_termination_signal()
        .await
}
```

That keeps `on_transaction` on the low-latency inline path. Other dataset
consumers can still continue on the dataset-worker path in parallel.

Inline dispatch removes local scheduling and queueing overhead. It does not fix late ingress. If
the host sees shreds late, inline still starts from late data.

## What You Usually Add Next

Once this works, the normal next step is one of these:

- write events into your own queue or channel
- aggregate metrics from transaction or slot events
- add more plugins for different event classes
- switch from direct UDP mode to gossip mode when you need richer topology/leader context

## What To Avoid In The First Version

Do not add all of this on day one:

- custom runtime extension plumbing
- advanced queue tuning
- direct transaction submission
- replayable derived-state consumers

Those are second-step concerns. First prove that your observer logic is correct.

## Real Example Files

- [`observer_runtime.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_runtime.rs)
- [`observer_with_non_vote_plugin.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_with_non_vote_plugin.rs)
- [`observer_with_multiple_plugins.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_with_multiple_plugins.rs)
