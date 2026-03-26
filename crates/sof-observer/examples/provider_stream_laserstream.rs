//! SOF runtime example for Helius LaserStream processed provider-stream ingress.
#![allow(clippy::missing_docs_in_private_items)]

use async_trait::async_trait;
use solana_pubkey::Pubkey;
use std::{env, str::FromStr};

use sof::{
    event::TxKind,
    framework::{ObserverPlugin, PluginConfig, PluginHost, TransactionEvent},
    provider_stream::{
        ProviderStreamMode, create_provider_stream_queue,
        laserstream::{
            LaserStreamCommitment, LaserStreamConfig, spawn_laserstream_transaction_source,
        },
    },
    runtime::ObserverRuntime,
};

#[derive(Clone, Copy, Debug, Default)]
struct LaserStreamTransactionLogger;

#[async_trait]
impl ObserverPlugin for LaserStreamTransactionLogger {
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
            "laserstream provider transaction observed",
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

    let endpoint = env::var("SOF_LASERSTREAM_ENDPOINT")
        .unwrap_or_else(|_| "https://laserstream-mainnet-fra.helius-rpc.com".to_owned());
    let api_key = env::var("SOF_LASERSTREAM_API_KEY")?;
    let account_include = maybe_parse_pubkeys("SOF_LASERSTREAM_ACCOUNT_INCLUDE")?;
    let account_required = maybe_parse_pubkeys("SOF_LASERSTREAM_ACCOUNT_REQUIRED")?;
    let (provider_tx, provider_rx) = create_provider_stream_queue(4_096);

    let mut config =
        LaserStreamConfig::new(endpoint, api_key).with_commitment(LaserStreamCommitment::Processed);
    if !account_include.is_empty() {
        config = config.with_account_include(account_include);
    }
    if !account_required.is_empty() {
        config = config.with_account_required(account_required);
    }

    let source = spawn_laserstream_transaction_source(config, provider_tx).await?;
    let host = PluginHost::builder()
        .add_plugin(LaserStreamTransactionLogger)
        .build();
    let runtime_result = ObserverRuntime::new()
        .with_plugin_host(host)
        .with_provider_stream_ingress(ProviderStreamMode::LaserStream, provider_rx)
        .run_until_termination_signal()
        .await;
    source.abort();
    runtime_result.map_err(Into::into)
}
