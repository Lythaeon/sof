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

## Core Types

- `TxBuilder`: compose transaction instructions and signing inputs.
- `TxSubmitClient`: submit through RPC/direct/hybrid.
- `SubmitMode`: runtime mode switch.
- `SignedTx`: submit externally signed transaction bytes.
- `RoutingPolicy`: leader/backup fanout controls.
- `LeaderProvider` and `RecentBlockhashProvider`: provider boundaries.

## Quickstart

```rust
use std::sync::Arc;

use sof_tx::{
    SubmitMode, TxBuilder, TxSubmitClient,
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

## Mode Guidance

- `RpcOnly`: maximum compatibility.
- `DirectOnly`: lowest path latency when leader targets are reliable.
- `Hybrid`: practical default for latency + fallback resilience.

## Feature Model

Current implementation supports RPC/direct/hybrid runtime modes through one API.
Compile-time capability flags from ADR-0006 can be introduced incrementally as the SDK stabilizes.

## Docs

- ADR for SDK scope: `../../docs/architecture/adr/0006-transaction-sdk-and-dual-submit-routing.md`
- Workspace docs index: `../../docs/README.md`
- Contribution guide: `../../CONTRIBUTING.md`
