//! SOF runtime example with deferred transaction dispatch and a no-op plugin.
#![doc(hidden)]

use async_trait::async_trait;
use sof::{
    framework::{
        Plugin, PluginConfig, PluginContext, PluginHost, PluginSetupError, TransactionEvent,
    },
    runtime::{ObserverRuntime, RuntimeError},
};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Default)]
struct NoopTransactionPlugin;

#[async_trait]
impl Plugin for NoopTransactionPlugin {
    fn name(&self) -> &'static str {
        "noop-transaction-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction()
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

#[derive(Debug, Error)]
enum ObserverTransactionsNoopError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

const fn require_release_mode() -> Result<(), ObserverTransactionsNoopError> {
    if cfg!(debug_assertions) {
        return Err(ObserverTransactionsNoopError::ReleaseModeRequired {
            command: "cargo run --release -p sof --example observer_transactions_noop",
        });
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), ObserverTransactionsNoopError> {
    require_release_mode()?;

    let host = PluginHost::builder()
        .add_plugin(NoopTransactionPlugin)
        .build();

    tracing::info!(
        plugins = ?host.plugin_names(),
        "starting SOF deferred transaction observer with noop plugin"
    );
    ObserverRuntime::new()
        .with_plugin_host(host)
        .run_until_termination_signal()
        .await?;
    Ok(())
}
