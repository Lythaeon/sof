# Build One Process That Observes And Submits

Start here when one service should both observe traffic and submit transactions from that local
view.

This is a useful execution shape, but it is not mandatory. `sof-tx` still works on its own with
RPC, Jito, and signed-byte flows.

This combined shape only reaches its latency potential when ingress is also early.

## Use This When

- you want fresh local leader and topology state
- you want `sof` and `sof-tx` in the same process
- you do not need restart-safe replay as the first requirement

## The Shape

You are combining:

- `sof` as the observer/runtime
- `sof-tx` as the transaction SDK
- `PluginHostTxProviderAdapter` as the bridge between them

That adapter consumes blockhash, leader, and topology events from `sof`, then exposes them through
the provider traits that `sof-tx` already understands.

That path is complete today in raw-shred and gossip-backed SOF runtimes. Built-in processed
provider adapters such as Yellowstone, LaserStream, and websocket are transaction-first today, so
they do not yet form a complete built-in `sof-tx` control-plane source on their own.

## Minimal Integration Skeleton

```rust
use std::sync::Arc;

use sof::framework::{ObserverPlugin, PluginHost};
use sof::runtime::ObserverRuntime;
use sof_tx::{
    SubmitMode, TxBuilder, TxSubmitClient,
    adapters::PluginHostTxProviderAdapter,
};
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_system_interface::instruction as system_instruction;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let adapter = Arc::new(PluginHostTxProviderAdapter::default());

    let host = PluginHost::builder()
        .add_shared_plugin(adapter.clone() as Arc<dyn ObserverPlugin>)
        .build();

    adapter.prime_from_plugin_host(&host);

    let mut client = TxSubmitClient::new(adapter.clone(), adapter.clone())
        .with_rpc_transport(Arc::new(sof_tx::submit::JsonRpcTransport::new(
            "https://api.mainnet-beta.solana.com",
        )?));

    let payer = Keypair::new();
    let recipient = Keypair::new();

    let builder = TxBuilder::new(payer.pubkey()).add_instruction(
        system_instruction::transfer(&payer.pubkey(), &recipient.pubkey(), 1),
    );

    let _ = (builder, &mut client, SubmitMode::Hybrid);

    let _ = ObserverRuntime::new().with_plugin_host(host).run().await;

    Ok(())
}
```

## Why This Shape Is Useful

The main benefit is local control:

- `sof` sees live traffic directly
- the adapter turns that into local control-plane inputs
- `sof-tx` can use those inputs for direct or hybrid submission decisions

This avoids depending on a separate internal control-plane service for the first version.

The caveat is the same one everywhere else in SOF: local freshness still depends on ingress
freshness first.

## What You Usually Add Next

- `Hybrid` mode with direct transport once TPU target quality is good enough
- flow-safety checks before submit
- your own strategy loop around `TxBuilder`
- metrics around stale control plane, blockhash freshness, and submit outcomes

## When Not To Use This Shape First

Do not start here if:

- you only need RPC-based transaction submission
- you need replay/recovery before you need low-latency local freshness
- you have not yet proved your observer logic and your submit logic independently
