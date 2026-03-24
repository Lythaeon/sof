//! SOF runtime example with inline transaction dispatch and a no-op HFT plugin.
#![doc(hidden)]

use async_trait::async_trait;
use sof::{
    framework::{
        Plugin, PluginConfig, PluginContext, PluginHost, PluginSetupError, TransactionDispatchMode,
        TransactionEvent,
    },
    runtime::{ObserverRuntime, RuntimeError},
};

#[derive(Debug, Clone, Copy, Default)]
struct NoopHftTransactionPlugin;

#[async_trait]
impl Plugin for NoopHftTransactionPlugin {
    fn name(&self) -> &'static str {
        "noop-hft-transaction-plugin"
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), RuntimeError> {
    let host = PluginHost::builder()
        .add_plugin(NoopHftTransactionPlugin)
        .build();

    tracing::info!(
        plugins = ?host.plugin_names(),
        "starting SOF HFT transaction observer with noop plugin"
    );
    ObserverRuntime::new()
        .with_plugin_host(host)
        .run_until_termination_signal()
        .await
}
