#![cfg_attr(not(feature = "sof-adapters"), allow(unused))]
//! Full multi-route submit example using SOF control-plane adapters.

#[cfg(not(feature = "sof-adapters"))]
fn main() {
    eprintln!("This example requires --features sof-adapters");
}

#[cfg(feature = "sof-adapters")]
use std::{sync::Arc, time::Duration};

#[cfg(feature = "sof-adapters")]
use sof::framework::PluginHost;
#[cfg(feature = "sof-adapters")]
use sof_solana_compat::{TxBuilder, TxSubmitClientSolanaExt};
#[cfg(feature = "sof-adapters")]
use sof_tx::{
    JitoJsonRpcTransport, JitoSubmitConfig, RpcRecentBlockhashProvider, RpcSubmitConfig,
    SubmitPlan, SubmitReliability, SubmitRoute, TxSubmitClient,
    adapters::PluginHostTxProviderAdapter,
    submit::{DirectSubmitConfig, JsonRpcTransport, UdpDirectTransport},
};
#[cfg(feature = "sof-adapters")]
use solana_keypair::Keypair;
#[cfg(feature = "sof-adapters")]
use solana_signer::Signer;
#[cfg(feature = "sof-adapters")]
use solana_system_interface::instruction as system_instruction;

#[cfg(feature = "sof-adapters")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let adapter = Arc::new(PluginHostTxProviderAdapter::default());
    let mut host = PluginHost::builder()
        .add_shared_plugin(adapter.clone())
        .build();

    // If the host already contains snapshots from a running SOF runtime, seed the adapter first.
    adapter.prime_from_plugin_host(&mut host);

    // In a real service, SOF should already be driving recent-blockhash, topology, and leader
    // events into the adapter. This RPC-backed blockhash provider gives the unsigned path a
    // usable blockhash source even before local SOF control-plane state is fully warm.
    let rpc_url = "https://api.mainnet-beta.solana.com";
    let blockhash_provider = Arc::new(RpcRecentBlockhashProvider::new(rpc_url)?);

    let mut client = TxSubmitClient::new(blockhash_provider.clone(), adapter.clone())
        .with_rpc_blockhash_provider(blockhash_provider)
        .with_flow_safety_source(adapter.clone())
        .with_reliability(SubmitReliability::Balanced)
        .with_rpc_transport(Arc::new(JsonRpcTransport::new(rpc_url)?))
        .with_rpc_config(RpcSubmitConfig {
            skip_preflight: true,
            preflight_commitment: None,
        })
        .with_jito_transport(Arc::new(JitoJsonRpcTransport::new()?))
        .with_jito_config(JitoSubmitConfig { bundle_only: true })
        .with_direct_transport(Arc::new(UdpDirectTransport::new()))
        .with_direct_config(DirectSubmitConfig {
            global_timeout: Duration::from_millis(250),
            rebroadcast_interval: Duration::from_millis(15),
            hybrid_rpc_broadcast: false,
            ..DirectSubmitConfig::default()
        });

    let payer = Keypair::new();
    let recipient = Keypair::new();
    let builder = TxBuilder::new(payer.pubkey())
        .with_compute_unit_limit(450_000)
        .with_priority_fee_micro_lamports(100_000)
        .tip_developer()
        .add_instruction(system_instruction::transfer(
            &payer.pubkey(),
            &recipient.pubkey(),
            1,
        ));

    let plan = SubmitPlan::all_at_once(vec![
        SubmitRoute::Direct,
        SubmitRoute::Rpc,
        SubmitRoute::Jito,
    ]);

    // If the plan includes Jito, make sure the transaction includes the fee and tip shape that
    // path expects. Jito documents a 1000-lamport minimum bundle tip, and competitive periods
    // often require more than the floor.
    let result = client.submit_unsigned_via(builder, &[&payer], plan).await?;

    println!(
        "submit accepted: first_success={:?} successful_routes={:?} sig={:?} rpc_sig={:?} jito_bundle_id={:?}",
        result.first_success_route,
        result.successful_routes,
        result.signature,
        result.rpc_signature,
        result.jito_bundle_id,
    );

    Ok(())
}
