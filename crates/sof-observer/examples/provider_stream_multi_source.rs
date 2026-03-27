//! Demonstrates fanning multiple provider sources into one SOF generic ingress.

#[cfg(not(feature = "provider-websocket"))]
fn main() {}

#[cfg(feature = "provider-websocket")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use sof::{
        provider_stream::{
            ProviderStreamMode, create_provider_stream_fan_in,
            websocket::{WebsocketLogsConfig, WebsocketLogsFilter, WebsocketTransactionConfig},
        },
        runtime::ObserverRuntime,
    };
    use solana_pubkey::Pubkey;

    let (fan_in, rx) = create_provider_stream_fan_in(1024);

    let transaction_source = fan_in
        .spawn_websocket_transaction_source(&WebsocketTransactionConfig::new(
            "wss://mainnet.helius-rpc.com/?api-key=example",
        ))
        .await?;

    let logs_source = fan_in
        .spawn_websocket_logs_source(
            &WebsocketLogsConfig::new("wss://mainnet.helius-rpc.com/?api-key=example")
                .with_filter(WebsocketLogsFilter::Mentions(Pubkey::new_unique())),
        )
        .await?;

    ObserverRuntime::new()
        .with_provider_stream_ingress(ProviderStreamMode::Generic, rx)
        .run_until(async move {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            transaction_source.abort();
            logs_source.abort();
        })
        .await?;

    Ok(())
}
