//! SOF runtime example for Yellowstone gRPC processed provider-stream ingress.
#![allow(clippy::missing_docs_in_private_items)]

use async_trait::async_trait;
use solana_pubkey::Pubkey;
use std::{env, str::FromStr};

use sof::{
    event::TxKind,
    framework::{ObserverPlugin, PluginConfig, PluginHost, TransactionEvent},
    provider_stream::{
        ProviderStreamMode, create_provider_stream_queue,
        yellowstone::{
            YellowstoneGrpcCommitment, YellowstoneGrpcConfig,
            spawn_yellowstone_grpc_transaction_source,
        },
    },
    runtime::ObserverRuntime,
};

#[derive(Clone, Copy, Debug, Default)]
struct YellowstoneTransactionLogger;

#[async_trait]
impl ObserverPlugin for YellowstoneTransactionLogger {
    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction()
    }

    async fn on_transaction(&self, event: &TransactionEvent) {
        if event.kind == TxKind::VoteOnly {
            return;
        }
        tracing::info!(
            slot = event.slot,
            signature = ?event.signature,
            kind = ?event.kind,
            "processed provider transaction observed",
        );
    }
}

fn maybe_parse_pubkeys(var: &str) -> Result<Vec<Pubkey>, Box<dyn std::error::Error>> {
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

    let endpoint = env::var("SOF_YELLOWSTONE_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:10000".to_owned());
    let x_token = env::var("SOF_YELLOWSTONE_X_TOKEN").ok();
    let account_include = maybe_parse_pubkeys("SOF_YELLOWSTONE_ACCOUNT_INCLUDE")?;
    let account_required = maybe_parse_pubkeys("SOF_YELLOWSTONE_ACCOUNT_REQUIRED")?;
    let (provider_tx, provider_rx) = create_provider_stream_queue(4_096);

    let mut config =
        YellowstoneGrpcConfig::new(endpoint).with_commitment(YellowstoneGrpcCommitment::Processed);
    if let Some(x_token) = x_token {
        config = config.with_x_token(x_token);
    }
    if !account_include.is_empty() {
        config = config.with_account_include(account_include);
    }
    if !account_required.is_empty() {
        config = config.with_account_required(account_required);
    }

    let source = spawn_yellowstone_grpc_transaction_source(config, provider_tx).await?;
    let host = PluginHost::builder()
        .add_plugin(YellowstoneTransactionLogger)
        .build();
    let runtime_result = ObserverRuntime::new()
        .with_plugin_host(host)
        .with_provider_stream_ingress(ProviderStreamMode::YellowstoneGrpc, provider_rx)
        .run_until_termination_signal()
        .await;
    source.abort();
    runtime_result.map_err(Into::into)
}
