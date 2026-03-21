# `sof-tx`

`sof-tx` is the transaction SDK in the workspace. It is built for services that need predictable
submit behavior and can benefit from local control-plane signals.

It is not a traffic-ingest runtime. It does not observe shreds, derive slot state, or discover
leaders by itself.

## What It Owns

- message and transaction construction
- signing boundary types
- submit mode orchestration
- routing policy and signature dedupe
- direct leader-target submission
- optional Jito and kernel-bypass transports
- adapters that ingest live or replayed state from `sof`

## What It Does Not Own

- Solana traffic ingest
- shred parsing or verification
- dataset reconstruction
- slot, fork, or topology observation
- deriving control-plane state directly from live network traffic

Those responsibilities belong to `sof` or to your own external control plane.

## When Not To Use It

`sof-tx` is probably the wrong first dependency if:

- you need shred ingest, dataset reconstruction, or plugin events
- you want local leader and blockhash state but do not yet have a source for it
- you are looking for a wallet-oriented UX helper rather than an execution SDK

## Main Types

| Type | Purpose |
| --- | --- |
| `TxBuilder` | build legacy or `V0` transactions |
| `TxSubmitClient` | configure transports and submit policy |
| `SubmitMode` | choose `RpcOnly`, `JitoOnly`, `DirectOnly`, or `Hybrid` |
| `RoutingPolicy` | choose primary and fallback fanout behavior |
| `SignatureDeduper` | avoid duplicate sends at signature granularity |
| `LeaderProvider` / `RecentBlockhashProvider` | abstract control-plane sources |

## The First Three Code Paths You Will Usually Need

### 1. Use `sof-tx` with RPC-sourced blockhash

Start here when you want RPC submission and you want the client to source recent blockhashes from
that same RPC endpoint.

```rust
use sof_tx::{SubmitMode, TxBuilder, TxSubmitClient};
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_system_interface::instruction as system_instruction;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let payer = Keypair::new();
    let recipient = Keypair::new();

    let mut client = TxSubmitClient::builder()
        .with_rpc_defaults("https://api.mainnet-beta.solana.com")?
        .build();

    let builder = TxBuilder::new(payer.pubkey()).add_instruction(
        system_instruction::transfer(&payer.pubkey(), &recipient.pubkey(), 1),
    );

    let _ = client
        .submit_unsigned(builder, &[&payer], SubmitMode::RpcOnly)
        .await?;

    Ok(())
}
```

Use this path when you want `sof-tx` for RPC submission without building a separate blockhash
layer first.

This path does not poll in the background. The client refreshes the recent blockhash only when the
unsigned submit path is about to use it.

For `JitoOnly`, keep the same RPC-backed blockhash source and attach a Jito transport on top. The
unsigned submit path still needs a recent blockhash even when the submit itself goes to Jito.

```rust
use std::sync::Arc;

use sof_tx::{
    SubmitMode, TxBuilder, TxSubmitClient,
    submit::JitoJsonRpcTransport,
};
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_system_interface::instruction as system_instruction;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let payer = Keypair::new();
    let recipient = Keypair::new();

    let mut client = TxSubmitClient::builder()
        .with_jito_defaults("https://api.mainnet-beta.solana.com")?
        .with_jito_transport(Arc::new(JitoJsonRpcTransport::new()?))
        .build();

    let builder = TxBuilder::new(payer.pubkey()).add_instruction(
        system_instruction::transfer(&payer.pubkey(), &recipient.pubkey(), 1),
    );

    let _ = client
        .submit_unsigned(builder, &[&payer], SubmitMode::JitoOnly)
        .await?;

    Ok(())
}
```

### 2. Submit signed bytes without blockhash setup in the client

Start here when your signer already lives elsewhere and you only need the submit pipeline.

```rust
use std::sync::Arc;

use sof_tx::{
    SignedTx, SubmitMode, TxSubmitClient,
    submit::JsonRpcTransport,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = TxSubmitClient::builder()
        .with_rpc_transport(Arc::new(JsonRpcTransport::new(
            "https://api.mainnet-beta.solana.com",
        )?))
        .build();

    let tx_bytes = Vec::new();
    let _ = client
        .submit_signed(SignedTx::WireTransactionBytes(tx_bytes), SubmitMode::RpcOnly)
        .await?;

    Ok(())
}
```

Use this path when you already have signed transaction bytes. `submit_signed(...)` does not build
the transaction inside the client, so there is no blockhash lookup step here.

### 3. Use `sof-tx` with live control-plane state from `sof`

Start here when one process owns both observation and submission.

```rust
use std::sync::Arc;

use sof::framework::{ObserverPlugin, PluginHost};
use sof_tx::{
    SubmitMode, TxBuilder, TxSubmitClient,
    adapters::PluginHostTxProviderAdapter,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let adapter = Arc::new(PluginHostTxProviderAdapter::default());
    let host = PluginHost::builder()
        .add_shared_plugin(adapter.clone() as Arc<dyn ObserverPlugin>)
        .build();

    adapter.prime_from_plugin_host(&host);
    let _client = TxSubmitClient::new(adapter.clone(), adapter.clone());

    // Start `sof` with the plugin host in the same process, then submit with `sof-tx`.
    let _ = SubmitMode::Hybrid;
    let _ = TxBuilder::new(solana_pubkey::Pubkey::new_unique());

    Ok(())
}
```

This is the shortest path to the “one process observes and sends” architecture.

## Submission Modes

### `RpcOnly`

Use when you want the simplest operational path and can accept RPC dependency for delivery.

### `DirectOnly`

Use when you have confidence in local leader and TPU endpoint state and want the lowest-latency
path.

### `Hybrid`

Use when you want direct leader targeting first with an RPC fallback path. This is the normal
starting point for latency-sensitive services because it balances speed with operational recovery.

### `JitoOnly`

Use when your flow is built explicitly around block-engine submission.

If you only want Jito submission, start with `TxSubmitClient::builder()` and
`.with_jito_defaults(...)`.

## Integration With `sof`

With the `sof-adapters` feature enabled, the SDK can consume live or replayed control-plane state
originating from the observer runtime.

This is an optional integration layer, not a hard dependency:

- `sof-tx` can run standalone with static or externally supplied providers
- pair it with `sof` only when you want locally observed control-plane state to drive submission

Two important adapter paths:

- `PluginHostTxProviderAdapter`: live in-process adapter fed by plugin events
- `DerivedStateTxProviderAdapter`: replayable adapter for restart-safe services

That split matters:

- live adapter for fast local runtime coupling
- replay adapter for stateful services that must recover cleanly across restarts

Practical fit:

- `sof` produces the control plane
- `sof-tx` consumes that control plane for send-time decisions

## Flow-Safety Checks

`sof-tx` can evaluate flow-safety before sending on local state. Typical failure causes:

- missing recent blockhash
- stale tip slot
- missing leader schedule
- missing TPU addresses for targeted leaders
- degraded cluster topology freshness

This is a core design choice: the SDK surfaces submit-time safety explicitly instead of burying it
in implicit retries.

## Recommended Adoption Pattern

1. start with `TxBuilder` and `TxSubmitClient`
2. wire in RPC transport first
3. add direct transport and `Hybrid` mode
4. attach `sof` adapters only after local runtime state is available and measured

## What To Open In The Repository

If the conceptual docs stop too early for what you need to build, open these next:

- [`crates/sof-tx/README.md`](https://github.com/Lythaeon/sof/blob/main/crates/sof-tx/README.md): current end-to-end usage patterns
- [`tpu_leader_logger.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/tpu_leader_logger.rs): how `sof` exposes leader information
- [`observer_with_non_vote_plugin.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_with_non_vote_plugin.rs): the plugin host shape that
  adapters plug into

## Feature Flags

```toml
sof-tx = { version = "0.11.0", features = ["sof-adapters"] }
sof-tx = { version = "0.11.0", features = ["kernel-bypass"] }
sof-tx = { version = "0.11.0", features = ["jito-grpc"] }
```

## Good Fit

`sof-tx` fits services that are building:

- arbitrage or execution services
- strategy engines that own their own routing policy
- infra services that need to consume leader and blockhash state during submission

It is a poor fit if you just need a generic wallet helper with minimal infrastructure concerns.
