//! SOF runtime example for websocket `transactionSubscribe` provider-stream ingress.
#![allow(clippy::missing_docs_in_private_items)]

use async_trait::async_trait;
use solana_pubkey::Pubkey;
use std::{env, str::FromStr};

use sof::{
    framework::{ObserverPlugin, PluginConfig, PluginHost, TransactionEvent},
    provider_stream::{
        ProviderStreamMode, create_provider_stream_queue,
        websocket::{
            WebsocketTransactionCommitment, WebsocketTransactionConfig,
            spawn_websocket_transaction_source,
        },
    },
    runtime::ObserverRuntime,
};

#[derive(Clone, Copy, Debug, Default)]
struct WebsocketTransactionPlugin;

#[async_trait]
impl ObserverPlugin for WebsocketTransactionPlugin {
    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction()
    }

    async fn on_transaction(&self, event: &TransactionEvent) {
        tracing::info!(
            slot = event.slot,
            signature = ?event.signature,
            kind = ?event.kind,
            "websocket provider transaction observed",
        );
    }
}

fn parse_pubkeys(var: &str) -> Result<Vec<Pubkey>, Box<dyn std::error::Error>> {
    env::var(var).map_or(Ok(Vec::new()), |value| {
        value
            .split(',')
            .filter(|segment| !segment.trim().is_empty())
            .map(|segment| Pubkey::from_str(segment.trim()).map_err(Into::into))
            .collect()
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let endpoint = env::var("SOF_WEBSOCKET_TRANSACTION_ENDPOINT")
        .unwrap_or_else(|_| "wss://mainnet.helius-rpc.com/?api-key=example".to_owned());
    let include = parse_pubkeys("SOF_WEBSOCKET_TRANSACTION_ACCOUNT_INCLUDE")?;
    let exclude = parse_pubkeys("SOF_WEBSOCKET_TRANSACTION_ACCOUNT_EXCLUDE")?;
    let required = parse_pubkeys("SOF_WEBSOCKET_TRANSACTION_ACCOUNT_REQUIRED")?;
    let include_votes = env::var("SOF_WEBSOCKET_TRANSACTION_INCLUDE_VOTES")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "on"));
    let include_failed = env::var("SOF_WEBSOCKET_TRANSACTION_INCLUDE_FAILED")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "on"));
    let (provider_tx, provider_rx) = create_provider_stream_queue(4_096);

    let mut config = WebsocketTransactionConfig::new(endpoint)
        .with_commitment(WebsocketTransactionCommitment::Processed)
        .with_vote(include_votes)
        .with_failed(include_failed);
    if !include.is_empty() {
        config = config.with_account_include(include);
    }
    if !exclude.is_empty() {
        config = config.with_account_exclude(exclude);
    }
    if !required.is_empty() {
        config = config.with_account_required(required);
    }

    let source = spawn_websocket_transaction_source(&config, provider_tx).await?;
    let host = PluginHost::builder()
        .add_plugin(WebsocketTransactionPlugin)
        .build();
    let runtime_result = ObserverRuntime::new()
        .with_plugin_host(host)
        .with_provider_stream_ingress(ProviderStreamMode::WebsocketTransaction, provider_rx)
        .run_until_termination_signal()
        .await;
    source.abort();
    runtime_result.map_err(Into::into)
}
