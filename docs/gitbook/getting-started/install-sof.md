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
use std::sync::Arc;

use sof_tx::{
    TxSubmitClient,
    providers::{StaticLeaderProvider, StaticRecentBlockhashProvider},
};

fn main() {
    let blockhash_provider = Arc::new(StaticRecentBlockhashProvider::new(Some([7_u8; 32])));
    let leader_provider = Arc::new(StaticLeaderProvider::new(None, Vec::new()));
    let _client = TxSubmitClient::new(blockhash_provider, leader_provider);
}
```

If you want `sof-tx` to consume live control-plane state from `sof`, enable the `sof-adapters`
feature before wiring the adapter types.

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
