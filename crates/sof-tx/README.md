# sof-tx

`sof-tx` is the transaction SDK for SOF.

It provides:

- ergonomic transaction/message building
- signed and pre-signed submission APIs
- route-plan based submission:
  - `SubmitPlan::rpc_only()`
  - `SubmitPlan::jito_only()`
  - `SubmitPlan::direct_only()`
  - `SubmitPlan::ordered(...)`
  - `SubmitPlan::hybrid()`
  - `SubmitPlan::all_at_once(...)`
- routing policy and signature-level dedupe

It works both as:

- a standalone transaction SDK for RPC-backed, Jito-backed, or signed-byte submit flows, and
- a lower-latency execution SDK when paired with `sof` or another local control-plane source for
  direct or hybrid routing

Together with `sof`, this is intended for low-latency execution services that want locally
sourced control-plane state and predictable submit behavior, not just a generic wallet helper.

## At a Glance

- Build `V0` or legacy Solana transactions
- Submit through RPC, Jito block engine, direct leader routing, or hybrid fallback
- Attach live `sof` runtime adapters when you want local leader/blockhash signals
- Use replayable derived-state adapters when your service must survive restart or replay
- Read control-plane snapshots from lower-contention adapter state instead of serializing all readers through one mutex
- Evaluate typed flow-safety policies before acting on local control-plane state
- Use optional kernel-bypass direct transports for custom low-latency networking

## Install

```bash
cargo add sof-tx
```

Enable SOF runtime adapters when you want provider values from live `sof` plugin events:

```toml
sof-tx = { version = "0.18.1", features = ["sof-adapters"] }
```

Enable `kernel-bypass` transport hooks for kernel-bypass direct submit integrations:

```toml
sof-tx = { version = "0.18.1", features = ["kernel-bypass"] }
```

Use `sof-solana-compat` when you want the Solana-native `TxBuilder` plus unsigned convenience
submission helpers on top of `sof-tx`:

```toml
sof-solana-compat = "0.18.1"
```

## Quick Start

Start with the simplest unsigned-submit path:

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

Pick the setup that matches what you need:

```rust
use std::sync::Arc;

use sof_tx::{
    TxSubmitClient,
    submit::JsonRpcTransport,
};

let mut rpc_client = TxSubmitClient::builder()
    .with_rpc_defaults("https://api.mainnet-beta.solana.com")?
    .build();

let mut jito_client = TxSubmitClient::builder()
    .with_jito_defaults("https://api.mainnet-beta.solana.com")?
    .build();

let mut signed_only_client = TxSubmitClient::builder()
    .with_rpc_transport(Arc::new(JsonRpcTransport::new(
        "https://api.mainnet-beta.solana.com",
    )?))
    .build();
```

- `with_rpc_defaults(...)`: use one RPC URL for unsigned RPC-route submission.
- `with_jito_defaults(...)`: use one RPC URL for blockhash plus Jito transport for Jito-route submission.
- `with_rpc_transport(...)`: enough for `submit_signed_via(...)` because that path does not build the
  transaction inside the client and does not need a blockhash provider.

The builder gives you the product-level paths first. Drop down to the provider APIs only when you
need custom control-plane wiring.

For most services, the practical order is:

- start with `SubmitPlan::rpc_only()`, `SubmitPlan::jito_only()`, or `submit_signed_via(...)`
- add direct or hybrid only when you have a trustworthy local routing source and a measured reason
  to use it

## Core Types

- `TxSubmitClientBuilder`: configure common RPC, Jito, direct, and control-plane paths without
  wiring providers by hand first.
- `sof_solana_compat::TxBuilder`: compose transaction instructions and signing inputs.
- `TxSubmitClient`: submit through one or more configured routes.
- `SubmitPlan`: primary route-plan API.
- `SubmitRoute`: one concrete route (`Rpc`, `Jito`, `Direct`).
- `SubmitStrategy`: ordered fallback or all-at-once execution.
- `SubmitMode`: legacy preset compatibility surface.
- `SignedTx`: submit externally signed transaction bytes.
- `RoutingPolicy`: leader/backup fanout controls.
- `LeaderProvider` and `RecentBlockhashProvider`: provider boundaries.
- `TxMessageVersion`: select `V0` (default) or `Legacy` message output.
- `MAX_TRANSACTION_WIRE_BYTES`: current max over-the-wire transaction bytes.
- `MAX_TRANSACTION_ACCOUNT_LOCKS`: current max account locks per transaction.

## Message Version and Limits

`TxBuilder` emits `V0` messages by default without requiring address lookup tables.

Legacy output remains available through `with_legacy_message()`.

Current protocol-aligned limits exposed by `sof-tx`:

- `MAX_TRANSACTION_WIRE_BYTES = 1232`
- `MAX_TRANSACTION_ACCOUNT_LOCKS = 128`

## SOF Adapter Layer

With `sof-adapters` enabled, `PluginHostTxProviderAdapter` can be:

- registered as a SOF plugin to ingest blockhash/leader/topology events, and
- passed directly into `TxSubmitClient` as both providers.
- evaluated with `evaluate_flow_safety(...)` before using its control-plane state for direct send decisions.

See the full example:

- `crates/sof-solana-compat/examples/submit_all_at_once_with_sof.rs`

That example shows:

- `PluginHostTxProviderAdapter` wired into a `PluginHost`
- RPC blockhash refresh plus SOF-backed leader routing
- direct, RPC, and Jito transports configured together
- `SubmitPlan::all_at_once(vec![Direct, Rpc, Jito])`

For restart-safe services built on SOF derived-state, use `DerivedStateTxProviderAdapter` instead.
It consumes the replayable derived-state feed, supports checkpoint persistence, and exposes the
same `evaluate_flow_safety(...)` helper for control-plane freshness checks.

Those SOF adapter paths are complete today with raw-shred or gossip-backed observer runtimes.
Built-in processed provider adapters such as Yellowstone, LaserStream, and websocket are
transaction-first today:

- they can drive recent blockhash state through observed provider transactions
- they do not, by themselves, provide the full `sof-tx` control-plane feed for direct routing
- direct submit still needs leaders/topology from gossip, manual target injection, or another
  control-plane source

So a mixed setup is already valid:

- provider-stream transactions for recent blockhash freshness
- gossip full or `control_plane_only` for cluster topology
- `PluginHostTxProviderAdapter::topology_only(...)` when that mixed setup is topology-backed but
  does not also emit leader-schedule hooks
- one shared SOF adapter feeding both into `sof-tx` in a custom embedding where
  your host/runtime composition supplies both surfaces together

The packaged observer runtime now supports one honest mixed built-in shape:

- built-in websocket / Yellowstone / LaserStream transaction ingress
- gossip bootstrap for cluster topology in the same runtime

That mixed packaged mode still does not synthesize leader-schedule or reorg hooks.
So:

- use `PluginHostTxProviderAdapter::default()` when SOF emits recent blockhash, topology, and
  leader schedule
- use `PluginHostTxProviderAdapter::topology_only(...)` when SOF emits recent blockhash plus
  topology, but not leader schedule
- use `ProviderStreamMode::Generic` when your custom producer supplies the full control-plane feed

The observer-side feed now also emits canonical control-plane quality snapshots, so services can
source freshness and confidence metadata from `sof` first and keep `sof-tx` focused on send-time
guard decisions.

For services that do not want to maintain a parallel checkpoint file format, use the adapter
persistence helper backed by SOF's generic `DerivedStateCheckpointStore`.

Direct submit needs TPU endpoints for scheduled leaders. That requirement applies to any submit
plan that includes `SubmitRoute::Direct`. The adapter gets those from `on_cluster_topology`
events, or you can inject them manually with:

- `set_leader_tpu_addr(pubkey, tpu_addr)`
- `remove_leader_tpu_addr(pubkey)`

The flow-safety report is intended to keep stale or degraded control-plane state from silently
driving direct or hybrid sends. Typical checks include:

- missing recent blockhash
- stale tip slot
- missing leader schedule when that input is enabled
- missing TPU addresses for targeted leaders
- degraded topology freshness

## Simplifying OpenBook + CPMM Flows

The recommended pattern is to keep strategy-specific instruction creation separate, and route both through one shared SDK pipeline.

```rust
use sof_solana_compat::{TxBuilder, TxSubmitClientSolanaExt};
use sof_tx::{SubmitPlan, TxSubmitClient};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn build_openbook_instructions(/* params */) -> Vec<Instruction> {
    // your OpenBook ix generation
    vec![]
}

fn build_cpmm_instructions(/* params */) -> Vec<Instruction> {
    // your CPMM ix generation
    vec![]
}

async fn submit_strategy_ixs(
    client: &mut TxSubmitClient,
    payer: &Keypair,
    ixs: Vec<Instruction>,
) -> Result<(), Box<dyn std::error::Error>> {
    let builder = TxBuilder::new(payer.pubkey())
        .with_compute_unit_limit(500_000)
        .with_priority_fee_micro_lamports(120_000)
        .tip_developer()
        .add_instructions(ixs);

    let _ = client
        .submit_unsigned_via(builder, &[payer], SubmitPlan::hybrid())
        .await?;

    Ok(())
}
```

This gives one consistent path for signing, dedupe, routing, and fallback behavior.

## Submitting Pre-Signed Transactions

If your signer is external (wallet/HSM/offline), submit bytes directly. This path does not need a
blockhash provider inside `TxSubmitClient` because the transaction is already signed before submit:

```rust
use sof_tx::{SignedTx, SubmitPlan, SubmitRoute, TxSubmitClient};

async fn send_presigned(
    client: &mut TxSubmitClient,
    tx_bytes: Vec<u8>,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = client
        .submit_signed_via(
            SignedTx::WireTransactionBytes(tx_bytes),
            SubmitPlan::all_at_once(vec![SubmitRoute::Direct, SubmitRoute::Jito]),
        )
        .await?;
    Ok(())
}
```

## Developer Tip

`TxBuilder::tip_developer()` adds a developer-support transfer instruction using a default tip of `5000` lamports.

- default amount: `5000` lamports (`DEFAULT_DEVELOPER_TIP_LAMPORTS`)
- default recipient: `G3WHMVjx7Cb3MFhBAHe52zw8yhbHodWnas5gYLceaqze`
- custom amount: `tip_developer_lamports(...)`
- custom recipient + amount: `tip_to(...)`

Thanks in advance for supporting continued SDK development.

## Submit Plans

Use `SubmitPlan` as the primary API:

- `SubmitPlan::rpc_only()`: maximum compatibility.
- `SubmitPlan::jito_only()`: Jito block-engine only when your strategy already includes the right
  fee/tip shape for that path.
- `SubmitPlan::direct_only()`: lowest path latency when leader targets are reliable.
- `SubmitPlan::ordered(vec![...])`: custom ordered-fallback route plan.
- `SubmitPlan::hybrid()`: practical default for latency plus RPC fallback resilience.
- `SubmitPlan::all_at_once(vec![...])`: preferred multi-route shape when you want to maximize the
  chance that one of several configured routes accepts the same transaction quickly, without
  depending on any single route's transient latency or availability. The submit call returns on
  the first accepted route; later background accepts are reported through
  `TxSubmitOutcomeReporter`, and built-in telemetry counts those accepts without mutating the
  returned `SubmitResult`. The reporter path is asynchronous and best-effort through one
  bounded FIFO dispatcher per reporter instance, shared across clients that use that same
  reporter, so it stays off the submit hot path while preserving callback order for queued
  outcomes. If that reporter path drops or cannot deliver outcomes, the built-in telemetry
  snapshot surfaces it through `reporter_outcomes_dropped` and
  `reporter_outcomes_unavailable`.

Arbitrary plans are first-class:

```rust
use sof_tx::{SubmitPlan, SubmitRoute, TxSubmitClient};

let plan = SubmitPlan::ordered(vec![
    SubmitRoute::Direct,
    SubmitRoute::Jito,
    SubmitRoute::Rpc,
]);

let burst_plan = SubmitPlan::all_at_once(vec![
    SubmitRoute::Direct,
    SubmitRoute::Jito,
]);

let _ = (plan, burst_plan, TxSubmitClient::builder());
```

When you intentionally configure more than one route, prefer `SubmitPlan::all_at_once(...)`
unless you specifically need ordered fallback semantics.

`SubmitMode` still exists as a legacy preset surface. It maps to exact ordered-fallback plans:

- `RpcOnly` -> `SubmitPlan::rpc_only()`
- `JitoOnly` -> `SubmitPlan::jito_only()`
- `DirectOnly` -> `SubmitPlan::direct_only()`
- `Hybrid` -> `SubmitPlan::hybrid()`

Use `submit_*_via(...)` when you want explicit multi-route behavior. Keep `submit_* (...)` with
`SubmitMode` only for compatibility or when one legacy preset is enough.

## Jito Configuration

Jito is split into two layers on purpose:

- transport-level configuration on `JitoJsonRpcTransport`
- per-submit behavior on `JitoSubmitConfig`

Default transport settings:

- endpoint: `JitoBlockEngineEndpoint::mainnet()` which resolves to `https://mainnet.block-engine.jito.wtf`
- `request_timeout = 10s`

Default submit settings:

- `bundle_only = false`

That means the default path is:

- standard Jito `sendTransaction`
- base64 payload encoding
- no auth header unless you configure one
- no revert-protection query parameter unless you opt into `bundle_only`

Example with explicit tuning:

```rust
use std::{sync::Arc, time::Duration};

use sof_tx::{
    JitoBlockEngineEndpoint, JitoSubmitConfig, JitoTransportConfig, TxSubmitClient,
    submit::JitoJsonRpcTransport,
};
use reqwest::Url;

let block_engine_url = Url::parse("https://amsterdam.mainnet.block-engine.jito.wtf")?;

let jito_transport = Arc::new(JitoJsonRpcTransport::with_config(
    JitoTransportConfig {
        endpoint: JitoBlockEngineEndpoint::custom(block_engine_url),
        request_timeout: Duration::from_secs(3),
    },
)?);

let client = TxSubmitClient::blockhash_only(blockhash_provider)
    .with_jito_transport(jito_transport)
    .with_jito_config(JitoSubmitConfig {
        bundle_only: true,
    });
```

Use `bundle_only = true` when you want Jito’s revert-protection behavior. Leave it `false` when
you want the default `sendTransaction` path.

If your submit plan includes `SubmitRoute::Jito`, make sure the transaction also includes the
economic shape that path expects. Jito's current transaction path documents a minimum tip of
`1000` lamports for bundles, and during competitive periods that floor may still be too low for
good landing probability. In practice, Jito should be treated as both a transport choice and a
fee/tip policy choice, not just another endpoint toggle.

Regional endpoint selection is available for the documented Jito mainnet regions:

```rust
use sof_tx::{JitoBlockEngineEndpoint, JitoBlockEngineRegion, submit::JitoJsonRpcTransport};

let jito_transport = JitoJsonRpcTransport::with_endpoint(
    JitoBlockEngineEndpoint::mainnet_region(JitoBlockEngineRegion::Frankfurt),
)?;
```

With the `jito-grpc` feature enabled, `sof-tx` also exposes `JitoGrpcTransport`. That path sends
transactions as single-transaction bundles over Jito searcher gRPC. If Jito is the route that
accepts before return, the bundle UUID is available in `SubmitResult.jito_bundle_id`; if another
route wins first, the later Jito accept still carries its bundle UUID through
`TxSubmitOutcomeReporter`, while built-in telemetry still counts that Jito accept even if the
reporter queue drops it under sustained pressure.

## Reliability Profiles

Direct and hybrid modes include built-in reliability defaults through `SubmitReliability`.

- `LowLatency`: minimal retries for fastest failover (`direct_target_rounds=1`, `hybrid_direct_attempts=1`)
- `Balanced` (default): extra direct retries before fallback (`direct_target_rounds=2`, `hybrid_direct_attempts=2`)
- `HighReliability`: most direct retrying (`direct_target_rounds=3`, `hybrid_direct_attempts=3`)

Use `TxSubmitClient::with_reliability(...)` for presets, or `with_direct_config(...)` for full control.

Reliability profiles are transport-side only. If you are sourcing blockhash, leader, and topology
state from SOF, pair them with `evaluate_flow_safety(...)` or `TxSubmitGuardPolicy` before sending.

## KernelBypass Direct Transport

With `kernel-bypass` enabled, implement `KernelBypassDatagramSocket` for your socket type and wrap it with
`KernelBypassDirectTransport`.

```rust
use std::{net::SocketAddr, sync::Arc};
use async_trait::async_trait;
use sof_tx::{KernelBypassDatagramSocket, KernelBypassDirectTransport, TxSubmitClient};

struct MyKernelBypassSocket;

#[async_trait]
impl KernelBypassDatagramSocket for MyKernelBypassSocket {
    async fn send_to(&self, payload: &[u8], target: SocketAddr) -> std::io::Result<usize> {
        let _ = (payload, target);
        Ok(payload.len())
    }
}

fn build_client(
    blockhash_provider: Arc<dyn sof_tx::RecentBlockhashProvider>,
    leader_provider: Arc<dyn sof_tx::LeaderProvider>,
) -> TxSubmitClient {
    let socket = Arc::new(MyKernelBypassSocket);
    TxSubmitClient::new(blockhash_provider, leader_provider)
        .with_direct_transport(Arc::new(KernelBypassDirectTransport::new(socket)))
}
```

## AF_XDP Demo and E2E

Linux-only AF_XDP demo (runs in an isolated user+network namespace via `unshare -Urn`):

```bash
cargo run -p sof-tx --example kernel_bypass_af_xdp --features kernel-bypass
```

Ignored integration test for AF_XDP kernel-bypass direct submit:

```bash
cargo test -p sof-tx --features kernel-bypass --test kernel_bypass_af_xdp_e2e -- --ignored --nocapture
```

Requirements:

- `unshare` from util-linux
- `ip` from iproute2
- Linux kernel with AF_XDP support

## Feature Model

Current implementation supports RPC/Jito/direct/hybrid runtime modes through one API.
Compile-time capability flags from ADR-0006 can be introduced incrementally as the SDK stabilizes.

## Docs

- ADR for SDK scope: `../../docs/architecture/adr/0006-transaction-sdk-and-dual-submit-routing.md`
- Workspace docs index: `../../docs/README.md`
- Architecture docs: `../../docs/architecture/README.md`
- Contribution guide: `../../CONTRIBUTING.md`
