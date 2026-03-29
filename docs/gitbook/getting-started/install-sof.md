# Install SOF

Use this page when adding SOF crates to an existing Rust project.

If Solana traffic internals are still unfamiliar, read these first:

- [Before You Start](before-you-start.md)
- [Common Questions](common-questions.md)

If the goal is to try the packaged repository examples first, use the commands in
[First Runtime Bring-Up](first-runtime.md).

## Add The Crates To Your Project

Observer/runtime:

```bash
cargo add sof
```

Transaction SDK:

```bash
cargo add sof-tx
```

Typed tuning profiles:

```bash
cargo add sof-gossip-tuning
```

Only add `sof-gossip-tuning` if you are embedding `sof` and want typed host/runtime tuning in code.

Common feature combinations:

```toml
sof = { version = "0.17.3", features = ["gossip-bootstrap"] }
sof-tx = { version = "0.17.3", features = ["sof-adapters"] }
```

## Choose Your Starting Point

Start with the app shape that matches what you need to build right now.

### Observer/runtime host

`Cargo.toml`:

```toml
[dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
sof = "0.17.3"
```

`src/main.rs`:

```rust
use sof::runtime::ObserverRuntime;

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    ObserverRuntime::new().run_until_termination_signal().await
}
```

Use this when you need ingest, plugin events, datasets, or local control-plane signals.

### Observer/runtime host with one plugin

`Cargo.toml`:

```toml
[dependencies]
async-trait = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
sof = "0.17.3"
tracing = "0.1"
```

`src/main.rs`:

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

Use this when you already know you want to consume SOF events in your own code.

### Submitter using external control-plane state

`Cargo.toml`:

```toml
[dependencies]
sof-tx = "0.17.3"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

`src/main.rs`:

```rust
use sof_tx::TxSubmitClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _client = TxSubmitClient::builder()
        .with_rpc_defaults("https://api.mainnet-beta.solana.com")?
        .build();
    Ok(())
}
```

Use this when you only need the send pipeline and want `sof-tx` to source recent blockhashes from
the same RPC endpoint you submit to.

If your signer already lives elsewhere and you only call `submit_signed(...)`, you can stop at
transport setup instead of wiring blockhash sourcing too.

## Minimal App-Level Validation

After adding the dependency, validate it in your own application with a small compile target or a
minimal runtime bring-up.

Example `sof` runtime skeleton:

```rust
use sof::runtime::ObserverRuntime;

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    ObserverRuntime::new().run_until_termination_signal().await
}
```

Example `sof-tx` provider wiring skeleton:

```rust
use sof_tx::TxSubmitClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _client = TxSubmitClient::builder()
        .with_rpc_defaults("https://api.mainnet-beta.solana.com")?
        .build();
    Ok(())
}
```

Example `sof-tx` signed-bytes skeleton:

```rust
use std::sync::Arc;

use sof_tx::{TxSubmitClient, submit::JsonRpcTransport};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _client = TxSubmitClient::builder()
        .with_rpc_transport(Arc::new(JsonRpcTransport::new(
            "https://api.mainnet-beta.solana.com",
        )?))
        .build();
    Ok(())
}
```

If you want `sof-tx` to consume live control-plane state from `sof`, enable the `sof-adapters`
feature before wiring the adapter types.

Once your minimal path compiles, the next useful step is not more docs, but one real example:

- [`observer_runtime.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_runtime.rs)
- [`observer_with_non_vote_plugin.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_with_non_vote_plugin.rs)
- [`crates/sof-tx/README.md`](https://github.com/Lythaeon/sof/blob/main/crates/sof-tx/README.md) for the current end-to-end submit examples

## If You Want To Try SOF From This Repository

From a checkout of this repository, you can use the packaged examples:

```bash
cargo run --release -p sof --example observer_runtime
```

Or:

```bash
cargo test -p sof-tx
```

Repository and contributor material lives under [Maintain SOF](../maintainers/README.md).
