//! SOF runtime example with inline transaction dispatch for low-latency transaction plugins.
#![doc(hidden)]

use async_trait::async_trait;
use sof::{
    event::TxKind,
    framework::{
        Plugin, PluginConfig, PluginContext, PluginHost, PluginSetupError, TransactionDispatchMode,
        TransactionEvent,
    },
    runtime::{ObserverRuntime, RuntimeError},
};

#[derive(Debug, Clone, Copy, Default)]
struct HftTransactionLoggerPlugin;

#[async_trait]
impl Plugin for HftTransactionLoggerPlugin {
    fn name(&self) -> &'static str {
        "hft-transaction-logger"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction_mode(TransactionDispatchMode::Inline)
    }

    async fn setup(&self, ctx: PluginContext) -> Result<(), PluginSetupError> {
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
            "inline transaction observed"
        );
    }

    async fn shutdown(&self, ctx: PluginContext) {
        tracing::info!(plugin = ctx.plugin_name, "plugin shutdown completed");
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), RuntimeError> {
    let host = PluginHost::builder()
        .add_plugin(HftTransactionLoggerPlugin)
        .build();

    tracing::info!(plugins = ?host.plugin_names(), "starting SOF HFT transaction observer");
    ObserverRuntime::new()
        .with_plugin_host(host)
        .run_until_termination_signal()
        .await
}
