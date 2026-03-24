//! SOF runtime example with completed-dataset transaction-view-batch callbacks and a no-op plugin.
#![doc(hidden)]

use async_trait::async_trait;
use sof::{
    framework::{
        Plugin, PluginConfig, PluginContext, PluginHost, PluginSetupError,
        TransactionViewBatchEvent,
    },
    runtime::{ObserverRuntime, RuntimeError},
};

#[derive(Debug, Clone, Copy, Default)]
struct NoopTransactionViewBatchPlugin;

#[async_trait]
impl Plugin for NoopTransactionViewBatchPlugin {
    fn name(&self) -> &'static str {
        "noop-transaction-view-batch-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction_view_batch()
    }

    async fn setup(&self, ctx: PluginContext) -> Result<(), PluginSetupError> {
        tracing::info!(plugin = ctx.plugin_name, "plugin startup completed");
        Ok(())
    }

    async fn on_transaction_view_batch(&self, _event: &TransactionViewBatchEvent) {}

    async fn shutdown(&self, ctx: PluginContext) {
        tracing::info!(plugin = ctx.plugin_name, "plugin shutdown completed");
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), RuntimeError> {
    let host = PluginHost::builder()
        .add_plugin(NoopTransactionViewBatchPlugin)
        .build();

    tracing::info!(
        plugins = ?host.plugin_names(),
        "starting SOF transaction-view-batch observer with noop plugin"
    );
    ObserverRuntime::new()
        .with_plugin_host(host)
        .run_until_termination_signal()
        .await
}
