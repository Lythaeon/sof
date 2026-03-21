//! SOF runtime example with transaction and dataset plugins.
#![doc(hidden)]

use async_trait::async_trait;
use sof::{
    event::TxKind,
    framework::{
        DatasetEvent, Plugin, PluginConfig, PluginHost, PluginShutdownContext,
        PluginStartupContext, PluginStartupError, TransactionEvent,
    },
};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Default)]
struct NonVoteTxLoggerPlugin;

#[async_trait]
impl Plugin for NonVoteTxLoggerPlugin {
    fn name(&self) -> &'static str {
        "non-vote-tx-logger"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction()
    }

    async fn on_startup(&self, ctx: PluginStartupContext) -> Result<(), PluginStartupError> {
        tracing::info!(plugin = ctx.plugin_name, "plugin startup completed");
        Ok(())
    }

    async fn on_transaction(&self, event: &TransactionEvent) {
        if event.kind == TxKind::VoteOnly {
            return;
        }

        let signature = event
            .signature
            .map(|signature| signature.to_string())
            .unwrap_or_else(|| "NO_SIGNATURE".to_owned());

        tracing::info!(
            slot = event.slot,
            signature = %signature,
            tx_kind = ?event.kind,
            "non-vote transaction observed"
        );
    }

    async fn on_shutdown(&self, ctx: PluginShutdownContext) {
        tracing::info!(plugin = ctx.plugin_name, "plugin shutdown completed");
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct DatasetLoggerPlugin;

#[async_trait]
impl Plugin for DatasetLoggerPlugin {
    fn name(&self) -> &'static str {
        "dataset-logger"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_dataset()
    }

    async fn on_dataset(&self, event: DatasetEvent) {
        tracing::info!(
            slot = event.slot,
            start = event.start_index,
            end = event.end_index,
            tx_count = event.tx_count,
            payload_len = event.payload_len,
            "dataset reconstructed"
        );
    }
}

#[derive(Debug, Error)]
enum ObserverWithMultiplePluginsError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error(transparent)]
    Runtime(#[from] sof::runtime::RuntimeError),
}

const fn require_release_mode() -> Result<(), ObserverWithMultiplePluginsError> {
    if cfg!(debug_assertions) {
        return Err(ObserverWithMultiplePluginsError::ReleaseModeRequired {
            command: "cargo run --release -p sof --example observer_with_multiple_plugins",
        });
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), ObserverWithMultiplePluginsError> {
    require_release_mode()?;

    let host = PluginHost::builder()
        .add_plugin(NonVoteTxLoggerPlugin)
        .add_plugin(DatasetLoggerPlugin)
        .build();

    tracing::info!(plugins = ?host.plugin_names(), "starting SOF runtime with plugin host");
    Ok(sof::runtime::run_async_with_plugin_host(host).await?)
}
