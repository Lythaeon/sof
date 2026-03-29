# `sof-tx`

`sof-tx` is the transaction SDK in the workspace. It is built for services that need predictable
submit behavior.

It works in two broad shapes:

- standalone, with RPC/Jito/external providers or signed-byte submission
- paired with `sof` when local control-plane signals should drive direct or hybrid routing

It is not a traffic-ingest runtime. It does not observe shreds, derive slot state, or discover
leaders by itself.

## What It Owns

- message and transaction construction
- signing boundary types
- submit route-plan orchestration
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
| `sof-solana-compat::TxBuilder` | build legacy or `V0` transactions |
| `TxSubmitClient` | configure transports and submit policy |
| `SubmitPlan` | choose one or more routes plus execution strategy |
| `SubmitRoute` | one concrete route: `Rpc`, `Jito`, or `Direct` |
| `SubmitStrategy` | ordered fallback or all-at-once execution |
| `SubmitMode` | legacy preset compatibility layer |
| `RoutingPolicy` | choose primary and fallback fanout behavior |
| `SignatureDeduper` | avoid duplicate sends at signature granularity |
| `LeaderProvider` / `RecentBlockhashProvider` | abstract control-plane sources |

Use `sof-solana-compat` when you want the Solana-native `TxBuilder` plus unsigned convenience
submission helpers on top of `sof-tx`.

## The First Three Code Paths You Will Usually Need

### 1. Use `sof-tx` with RPC-sourced blockhash

Start here when you want RPC submission and you want the client to source recent blockhashes from
that same RPC endpoint.

```rust
use sof_solana_compat::{TxBuilder, TxSubmitClientSolanaExt};
use sof_tx::{SubmitPlan, TxSubmitClient};
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
        .submit_unsigned_via(builder, &[&payer], SubmitPlan::rpc_only())
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

use sof_solana_compat::{TxBuilder, TxSubmitClientSolanaExt};
use sof_tx::{SubmitPlan, TxSubmitClient, submit::JitoJsonRpcTransport};
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
        .submit_unsigned_via(builder, &[&payer], SubmitPlan::jito_only())
        .await?;

    Ok(())
}
```

### 2. Submit signed bytes without blockhash setup in the client

Start here when your signer already lives elsewhere and you only need the submit pipeline.

```rust
use std::sync::Arc;

use sof_tx::{SignedTx, SubmitPlan, SubmitRoute, TxSubmitClient, submit::JsonRpcTransport};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = TxSubmitClient::builder()
        .with_rpc_transport(Arc::new(JsonRpcTransport::new(
            "https://api.mainnet-beta.solana.com",
        )?))
        .build();

    let tx_bytes = Vec::new();
    let _ = client
        .submit_signed_via(
            SignedTx::WireTransactionBytes(tx_bytes),
            SubmitPlan::all_at_once(vec![SubmitRoute::Rpc, SubmitRoute::Jito]),
        )
        .await?;

    Ok(())
}
```

Use this path when you already have signed transaction bytes. `submit_signed_via(...)` does not build
the transaction inside the client, so there is no blockhash lookup step here.

### 3. Use `sof-tx` with live control-plane state from `sof`

Start here when one process owns both observation and submission.

See the full example in:

- `crates/sof-solana-compat/examples/submit_all_at_once_with_sof.rs`

It shows one full process shape with:

- `PluginHostTxProviderAdapter`
- SOF plugin-host wiring
- RPC blockhash refresh plus SOF-backed leader routing
- direct, RPC, and Jito transports configured together
- `SubmitPlan::all_at_once(vec![Direct, Rpc, Jito])`

## Submit Plans

`SubmitPlan` is the primary API:

- `SubmitPlan::rpc_only()`: simplest operational path when you can accept RPC dependency
- `SubmitPlan::jito_only()`: block-engine only submission
- `SubmitPlan::direct_only()`: lowest-latency path when local leader and TPU state are reliable
- `SubmitPlan::ordered(vec![...])`: custom ordered-fallback route plan
- `SubmitPlan::hybrid()`: direct first with RPC fallback
- `SubmitPlan::all_at_once(vec![...])`: preferred multi-route shape when you want to maximize the
  chance that one of several configured routes accepts the same transaction quickly. The submit
  call returns on the first accepted route; later background accepts are surfaced through
  `TxSubmitOutcomeReporter`, and built-in telemetry still counts those accepts instead of mutating
  the original `SubmitResult`.

Arbitrary plans are first-class:

```rust
use sof_tx::{SubmitPlan, SubmitRoute};

let ordered = SubmitPlan::ordered(vec![
    SubmitRoute::Direct,
    SubmitRoute::Jito,
    SubmitRoute::Rpc,
]);

let concurrent = SubmitPlan::all_at_once(vec![
    SubmitRoute::Direct,
    SubmitRoute::Jito,
]);

let _ = (ordered, concurrent);
```

When you intentionally configure more than one route, prefer `SubmitPlan::all_at_once(...)`
unless you specifically need ordered fallback semantics.

`SubmitMode` still exists, but only as a legacy preset layer:

- `RpcOnly` -> `SubmitPlan::rpc_only()`
- `JitoOnly` -> `SubmitPlan::jito_only()`
- `DirectOnly` -> `SubmitPlan::direct_only()`
- `Hybrid` -> `SubmitPlan::hybrid()`

If a plan includes `SubmitRoute::Jito`, remember that Jito is not just a transport toggle. The
transaction still needs the right fee and tip shape for that path. Jito's current transaction
docs describe a minimum bundle tip of `1000` lamports, and that floor may still be too low during
competitive periods.

## Integration With `sof`

With the `sof-adapters` feature enabled, the SDK can consume live or replayed control-plane state
originating from the observer runtime.

This is an optional integration layer, not a hard dependency:

- `sof-tx` can run standalone with static or externally supplied providers
- pair it with `sof` only when you want locally observed control-plane state to drive submission

Two important adapter paths:

- `PluginHostTxProviderAdapter`: live in-process adapter fed by plugin events
- `DerivedStateTxProviderAdapter`: replayable adapter for restart-safe services

Those adapter paths are complete today with raw-shred or gossip-backed SOF runtimes. Built-in
processed provider adapters such as Yellowstone, LaserStream, and websocket can now emit several
typed processed-event families, but they still do not by themselves provide the full `sof-tx`
control-plane feed.

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
2. wire in RPC transport first, or Jito if that is already the intended execution path
3. use `submit_signed_via(...)` when signing already lives elsewhere
4. add direct transport and `SubmitPlan::hybrid()` only after local routing state is available and measured
5. attach `sof` adapters only when you want locally observed control-plane state to drive those
   direct or hybrid decisions

If you intentionally configure more than one route, prefer `SubmitPlan::all_at_once(...)` as the
multi-route shape. It returns on the first accepted route, while later accepts are surfaced
through the external outcome reporter. That reporter path is asynchronous and best-effort through
one bounded FIFO dispatcher per reporter instance, shared across clients that use that same
reporter, while built-in telemetry still counts every outcome inline. If reporter delivery drops
or becomes unavailable, that is surfaced through the built-in telemetry snapshot.

## What To Open In The Repository

If the conceptual docs stop too early for what you need to build, open these next:

- [`crates/sof-tx/README.md`](https://github.com/Lythaeon/sof/blob/main/crates/sof-tx/README.md): current end-to-end usage patterns
- [`tpu_leader_logger.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/tpu_leader_logger.rs): how `sof` exposes leader information
- [`observer_with_non_vote_plugin.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_with_non_vote_plugin.rs): the plugin host shape that
  adapters plug into

## Feature Flags

```toml
sof-tx = { version = "0.17.0", features = ["sof-adapters"] }
sof-tx = { version = "0.17.0", features = ["kernel-bypass"] }
sof-tx = { version = "0.17.0", features = ["jito-grpc"] }
```

## Good Fit

`sof-tx` fits services that are building:

- arbitrage or execution services
- strategy engines that own their own routing policy
- infra services that need to consume leader and blockhash state during submission

It is a poor fit if you just need a generic wallet helper with minimal infrastructure concerns.
