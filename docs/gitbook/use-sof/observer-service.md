# Build An Observer Service

Start here when you want one service that observes Solana traffic and emits your own events,
metrics, or logs.

This is the simplest useful `sof` shape.

It is also a good place to stay honest about latency:

- if ingress is early, this shape can be low-latency
- if ingress is late, this shape is still useful, but it is not magically early

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

Use `.at_commitment(...)` when the service should ignore lower-confidence traffic, or
`.only_at_commitment(...)` when it should react only to one exact commitment. If you do not set
either one, SOF defaults to processed-or-better delivery.

## What You Usually Add Next

Once this works, the normal next step is one of these:

- write events into your own queue or channel
- aggregate metrics from transaction or slot events
- add more plugins for different event classes
- switch ingress mode when you have a real reason
  - gossip for cluster discovery and topology
  - private raw ingress for earlier shreds
  - processed providers when you want typed transaction, account, block-meta, log, or slot feeds

## What To Avoid In The First Version

Do not add all of this on day one:

- custom runtime extension plumbing
- advanced queue tuning
- direct transaction submission
- replayable derived-state consumers

Those are second-step concerns. First prove that your observer logic is correct.
