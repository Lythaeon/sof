//! SOF runtime example with inline transactions plus a dataset consumer.
#![doc(hidden)]

use async_trait::async_trait;
use sof::{
    framework::{
        DatasetEvent, Plugin, PluginConfig, PluginContext, PluginHost, PluginSetupError,
        TransactionDispatchMode, TransactionEvent,
    },
    runtime::{ObserverRuntime, RuntimeError},
};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Default)]
struct NoopInlineTransactionPlugin;

#[async_trait]
impl Plugin for NoopInlineTransactionPlugin {
    fn name(&self) -> &'static str {
        "noop-inline-transaction-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction_mode(TransactionDispatchMode::Inline)
    }

    async fn setup(&self, ctx: PluginContext) -> Result<(), PluginSetupError> {
        tracing::info!(plugin = ctx.plugin_name, "plugin startup completed");
        Ok(())
    }

    async fn on_transaction(&self, _event: &TransactionEvent) {}

    async fn shutdown(&self, ctx: PluginContext) {
        tracing::info!(plugin = ctx.plugin_name, "plugin shutdown completed");
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct NoopDatasetPlugin;

#[async_trait]
impl Plugin for NoopDatasetPlugin {
    fn name(&self) -> &'static str {
        "noop-dataset-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_dataset()
    }

    async fn setup(&self, ctx: PluginContext) -> Result<(), PluginSetupError> {
        tracing::info!(plugin = ctx.plugin_name, "plugin startup completed");
        Ok(())
    }

    async fn on_dataset(&self, _event: DatasetEvent) {}

    async fn shutdown(&self, ctx: PluginContext) {
        tracing::info!(plugin = ctx.plugin_name, "plugin shutdown completed");
    }
}

#[derive(Debug, Error)]
enum ObserverInlineTransactionsWithDatasetNoopError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

const fn require_release_mode() -> Result<(), ObserverInlineTransactionsWithDatasetNoopError> {
    if cfg!(debug_assertions) {
        return Err(
            ObserverInlineTransactionsWithDatasetNoopError::ReleaseModeRequired {
                command: "cargo run --release -p sof --example observer_inline_transactions_with_dataset_noop",
            },
        );
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), ObserverInlineTransactionsWithDatasetNoopError> {
    require_release_mode()?;

    let host = PluginHost::builder()
        .add_plugin(NoopInlineTransactionPlugin)
        .add_plugin(NoopDatasetPlugin)
        .build();

    tracing::info!(
        plugins = ?host.plugin_names(),
        "starting SOF runtime with mixed inline transaction and dataset consumers"
    );
    ObserverRuntime::new()
        .with_plugin_host(host)
        .run_until_termination_signal()
        .await?;
    Ok(())
}
