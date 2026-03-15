# Build One Process That Observes And Submits

Start here when one low-latency service should both observe live traffic and submit transactions
using that same local control plane.

This is the normal SOF product shape for execution services.

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

## Minimal Integration Skeleton

```rust
use std::sync::Arc;

use sof::framework::{ObserverPlugin, PluginHost};
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

    // Run `sof` with this plugin host, then use the client from your strategy task.
    let _ = sof::runtime::run_async_with_plugin_host(host).await;

    Ok(())
}
```

That snippet shows the important wiring, not the final service orchestration. In a real service,
you usually run:

- the SOF runtime task
- one strategy/execution task that owns the submit client
- one application-specific decision loop that decides when to build and submit

## Why This Shape Is Useful

The main benefit is freshness:

- `sof` sees live traffic directly
- the adapter turns that into local control-plane inputs
- `sof-tx` can use those inputs for direct or hybrid submission decisions

This avoids depending on a separate internal control-plane service for the first version.

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

In those cases, start with either the pure observer service or the pure submitter first.

## Real Example Files

- [`observer_with_non_vote_plugin.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/observer_with_non_vote_plugin.rs)
- [`tpu_leader_logger.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/tpu_leader_logger.rs)
- [`crates/sof-tx/README.md`](https://github.com/Lythaeon/sof/blob/main/crates/sof-tx/README.md)
