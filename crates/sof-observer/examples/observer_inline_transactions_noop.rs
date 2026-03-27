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
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
#[path = "support/af_xdp_ingress.rs"]
mod af_xdp_ingress;

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

    #[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
    if std::env::var_os("SOF_AF_XDP_IFACE").is_some() {
        let config = af_xdp_ingress::read_af_xdp_config();
        let stop = Arc::new(AtomicBool::new(false));
        let producer_stop = Arc::clone(&stop);
        let (tx, rx) = sof::runtime::create_kernel_bypass_ingress_queue();

        tracing::info!(
            interface = %config.interface,
            queue_id = config.queue_id,
            ring_depth = config.ring_depth,
            batch_size = config.batch_size,
            "starting noop observer with AF_XDP kernel-bypass ingress"
        );

        let producer_task = tokio::task::spawn_blocking(move || {
            af_xdp_ingress::run_af_xdp_producer_until(&tx, &config, &producer_stop)
        });
        let producer_wait_task = tokio::spawn(async move {
            let result = producer_task.await.map_err(|error| {
                RuntimeError::Runloop(format!("AF_XDP producer task join failed: {error}"))
            })?;
            result
                .map_err(|error| RuntimeError::Runloop(format!("AF_XDP producer failed: {error}")))
        });

        let runtime_result = ObserverRuntime::new()
            .with_plugin_host(host)
            .with_kernel_bypass_ingress(rx)
            .run_until_termination_signal()
            .await;

        stop.store(true, Ordering::Relaxed);
        let producer_result = producer_wait_task.await.map_err(|error| {
            RuntimeError::Runloop(format!("AF_XDP producer waiter join failed: {error}"))
        })?;
        producer_result?;
        return runtime_result;
    }

    ObserverRuntime::new()
        .with_plugin_host(host)
        .run_until_termination_signal()
        .await
}
