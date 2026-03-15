# Install SOF

This page is for consumers adding SOF crates to their own project.

If you are evaluating the repository examples instead, use the commands shown in
[First Runtime Bring-Up](first-runtime.md) from a repository checkout.

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
sof = { version = "0.9.2", features = ["gossip-bootstrap"] }
sof-tx = { version = "0.9.2", features = ["sof-adapters"] }
```

## Choose Your Starting Point

Start with the app shape that matches what you need to build right now.

### Observer/runtime host

`Cargo.toml`:

```toml
[dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
sof = "0.9.2"
```

`src/main.rs`:

```rust
#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    sof::runtime::run_async().await
}
```

Use this when you need ingest, plugin events, datasets, or local control-plane signals.

### Observer/runtime host with one plugin

`Cargo.toml`:

```toml
[dependencies]
async-trait = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
sof = "0.9.2"
tracing = "0.1"
```

`src/main.rs`:

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

Use this when you already know you want to consume SOF events in your own code.

### Submitter using external control-plane state

`Cargo.toml`:

```toml
[dependencies]
sof-tx = "0.9.2"
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
#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    sof::runtime::run_async().await
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

If you are developing the repository itself rather than consuming the crates, switch to the
[Maintain SOF](../maintainers/README.md) track.
