# sof-tx

`sof-tx` is the transaction SDK for SOF.

It provides:

- ergonomic transaction/message building
- signed and pre-signed submission APIs
- runtime mode selection:
  - `RpcOnly`
  - `DirectOnly` (leader-targeted UDP send)
  - `Hybrid` (direct first, RPC fallback)
- routing policy and signature-level dedupe

## Install

```bash
cargo add sof-tx
```

Enable SOF runtime adapters when you want provider values from live `sof` plugin events:

```toml
sof-tx = { version = "0.4", features = ["sof-adapters"] }
```

## Core Types

- `TxBuilder`: compose transaction instructions and signing inputs.
- `TxSubmitClient`: submit through RPC/direct/hybrid.
- `SubmitMode`: runtime mode switch.
- `SignedTx`: submit externally signed transaction bytes.
- `RoutingPolicy`: leader/backup fanout controls.
- `LeaderProvider` and `RecentBlockhashProvider`: provider boundaries.

## SOF Adapter Layer

With `sof-adapters` enabled, `PluginHostTxProviderAdapter` can be:

- registered as a SOF plugin to ingest blockhash/leader/topology events, and
- passed directly into `TxSubmitClient` as both providers.

```rust
use std::sync::Arc;

use sof::framework::{ObserverPlugin, PluginHost};
use sof_tx::{
    SubmitMode, SubmitReliability, TxBuilder, TxSubmitClient,
    adapters::PluginHostTxProviderAdapter,
    submit::{JsonRpcTransport, UdpDirectTransport},
};
use solana_keypair::Keypair;
use solana_signer::Signer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let adapter = Arc::new(PluginHostTxProviderAdapter::default());
    let host = PluginHost::builder()
        .add_shared_plugin(adapter.clone() as Arc<dyn ObserverPlugin>)
        .build();

    // If SOF runtime was already running, optionally seed from host snapshots.
    adapter.prime_from_plugin_host(&host);

    let client = TxSubmitClient::new(adapter.clone(), adapter.clone())
        .with_reliability(SubmitReliability::Balanced)
        .with_rpc_transport(Arc::new(JsonRpcTransport::new("https://api.mainnet-beta.solana.com")?))
        .with_direct_transport(Arc::new(UdpDirectTransport));

    let payer = Keypair::new();
    let recipient = Keypair::new();
    let builder = TxBuilder::new(payer.pubkey())
        .with_compute_unit_limit(450_000)
        .with_priority_fee_micro_lamports(100_000)
        .add_instruction(solana_system_interface::instruction::transfer(
            &payer.pubkey(),
            &recipient.pubkey(),
            1,
        ));

    let _ = client
        .submit_builder(builder, &[&payer], SubmitMode::Hybrid)
        .await?;

    Ok(())
}
```

Direct submit needs TPU endpoints for scheduled leaders. The adapter gets these from
`on_cluster_topology` events, or you can inject them manually with:

- `set_leader_tpu_addr(pubkey, tpu_addr)`
- `remove_leader_tpu_addr(pubkey)`

## Quickstart

```rust
use std::sync::Arc;

use sof_tx::{
    SubmitMode, SubmitReliability, TxBuilder, TxSubmitClient,
    providers::{LeaderProvider, LeaderTarget, RecentBlockhashProvider, StaticLeaderProvider, StaticRecentBlockhashProvider},
    submit::{JsonRpcTransport, UdpDirectTransport},
};
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_system_interface::instruction as system_instruction;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let payer = Keypair::new();
    let recipient = Keypair::new();

    let blockhash_provider = Arc::new(StaticRecentBlockhashProvider::new(Some([7_u8; 32])));
    let leader_provider = Arc::new(StaticLeaderProvider::new(None, Vec::new()));

    let client = TxSubmitClient::new(blockhash_provider, leader_provider)
        .with_reliability(SubmitReliability::Balanced)
        .with_rpc_transport(Arc::new(JsonRpcTransport::new("https://api.mainnet-beta.solana.com")?))
        .with_direct_transport(Arc::new(UdpDirectTransport));

    let builder = TxBuilder::new(payer.pubkey())
        .with_compute_unit_limit(450_000)
        .with_priority_fee_micro_lamports(100_000)
        .tip_developer()
        .add_instruction(system_instruction::transfer(
            &payer.pubkey(),
            &recipient.pubkey(),
            1,
        ));

    let _result = client
        .submit_builder(builder, &[&payer], SubmitMode::Hybrid)
        .await?;

    Ok(())
}
```

## Simplifying OpenBook + CPMM Flows

The recommended pattern is to keep strategy-specific instruction creation separate, and route both through one shared SDK pipeline.

```rust
use sof_tx::{SubmitMode, TxBuilder, TxSubmitClient};
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
    client: &TxSubmitClient,
    payer: &Keypair,
    ixs: Vec<Instruction>,
) -> Result<(), Box<dyn std::error::Error>> {
    let builder = TxBuilder::new(payer.pubkey())
        .with_compute_unit_limit(500_000)
        .with_priority_fee_micro_lamports(120_000)
        .tip_developer()
        .add_instructions(ixs);

    let _ = client
        .submit_builder(builder, &[payer], SubmitMode::Hybrid)
        .await?;

    Ok(())
}
```

This gives one consistent path for signing, dedupe, routing, and fallback behavior.

## Submitting Pre-Signed Transactions

If your signer is external (wallet/HSM/offline), submit bytes directly:

```rust
use sof_tx::{SignedTx, SubmitMode, TxSubmitClient};

async fn send_presigned(
    client: &TxSubmitClient,
    tx_bytes: Vec<u8>,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = client
        .submit_signed(SignedTx::WireTransactionBytes(tx_bytes), SubmitMode::Hybrid)
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

## Mode Guidance

- `RpcOnly`: maximum compatibility.
- `DirectOnly`: lowest path latency when leader targets are reliable.
- `Hybrid`: practical default for latency + fallback resilience.

## Reliability Profiles

Direct and hybrid modes include built-in reliability defaults through `SubmitReliability`.

- `LowLatency`: minimal retries for fastest failover (`direct_target_rounds=1`, `hybrid_direct_attempts=1`)
- `Balanced` (default): extra direct retries before fallback (`direct_target_rounds=2`, `hybrid_direct_attempts=2`)
- `HighReliability`: most direct retrying (`direct_target_rounds=3`, `hybrid_direct_attempts=3`)

Use `TxSubmitClient::with_reliability(...)` for presets, or `with_direct_config(...)` for full control.

## Feature Model

Current implementation supports RPC/direct/hybrid runtime modes through one API.
Compile-time capability flags from ADR-0006 can be introduced incrementally as the SDK stabilizes.

## Docs

- ADR for SDK scope: `../../docs/architecture/adr/0006-transaction-sdk-and-dual-submit-routing.md`
- Workspace docs index: `../../docs/README.md`
- Contribution guide: `../../CONTRIBUTING.md`
