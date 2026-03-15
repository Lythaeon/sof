# Build An Observer Service

Start here when you want one service that ingests live Solana traffic and emits your own
application-level events, metrics, or logs.

This is the lowest-friction real product shape for `sof`.

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
    framework::{Plugin, PluginHost, TransactionEvent},
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
        tracing::info!(slot = event.slot, kind = ?event.kind, "non-vote transaction observed");
    }
}

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let host = PluginHost::builder().add_plugin(NonVoteLogger).build();
    sof::runtime::run_async_with_plugin_host(host).await
}
```

This is the right first service because it proves all of the important boundaries:

- SOF can start on your host
- your service can receive decoded events
- your application logic can stay outside the runtime core

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
