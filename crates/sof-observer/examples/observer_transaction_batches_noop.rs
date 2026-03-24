//! SOF runtime example with completed-dataset transaction-batch callbacks and a no-op plugin.
#![doc(hidden)]

use async_trait::async_trait;
use sof::{
    framework::{
        Plugin, PluginConfig, PluginContext, PluginHost, PluginSetupError, TransactionBatchEvent,
    },
    runtime::{ObserverRuntime, RuntimeError},
};

#[derive(Debug, Clone, Copy, Default)]
struct NoopTransactionBatchPlugin;

#[async_trait]
impl Plugin for NoopTransactionBatchPlugin {
    fn name(&self) -> &'static str {
        "noop-transaction-batch-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction_batch()
    }

    async fn setup(&self, ctx: PluginContext) -> Result<(), PluginSetupError> {
        tracing::info!(plugin = ctx.plugin_name, "plugin startup completed");
        Ok(())
    }

    async fn on_transaction_batch(&self, _event: &TransactionBatchEvent) {}

    async fn shutdown(&self, ctx: PluginContext) {
        tracing::info!(plugin = ctx.plugin_name, "plugin shutdown completed");
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), RuntimeError> {
    let host = PluginHost::builder()
        .add_plugin(NoopTransactionBatchPlugin)
        .build();

    tracing::info!(
        plugins = ?host.plugin_names(),
        "starting SOF transaction-batch observer with noop plugin"
    );
    ObserverRuntime::new()
        .with_plugin_host(host)
        .run_until_termination_signal()
        .await
}
