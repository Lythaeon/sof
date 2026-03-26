//! SOF runtime example for trusted raw shred providers feeding external ingress.
//!
//! This example shows the intended production shape for a trusted raw shred feed:
//!
//! - an external producer writes [`sof::ingest::RawPacketBatch`] values into SOF
//! - `SOF_SHRED_TRUST_MODE=trusted_raw_shred_provider` is expressed via typed setup
//! - SOF keeps its normal parse, FEC, reconstruction, and plugin dispatch pipeline
//! - local shred verification defaults are relaxed because the upstream feed is trusted
//!
//! Use `SOF_AF_XDP_IFACE=...` to drive the example from the bundled AF_XDP producer.
#![doc(hidden)]

use async_trait::async_trait;
use sof::{
    framework::{
        Plugin, PluginConfig, PluginContext, PluginHost, PluginSetupError, TransactionDispatchMode,
        TransactionEvent,
    },
    runtime::{ObserverRuntime, RuntimeError, RuntimeSetup, ShredTrustMode},
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
struct TrustedProviderInlinePlugin;

#[async_trait]
impl Plugin for TrustedProviderInlinePlugin {
    fn name(&self) -> &'static str {
        "trusted-provider-inline-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction_mode(TransactionDispatchMode::Inline)
    }

    async fn setup(&self, ctx: PluginContext) -> Result<(), PluginSetupError> {
        tracing::info!(
            plugin = ctx.plugin_name,
            "trusted provider plugin startup completed"
        );
        Ok(())
    }

    async fn on_transaction(&self, event: &TransactionEvent) {
        tracing::debug!(
            slot = event.slot,
            signature = ?event.signature,
            "trusted provider transaction callback"
        );
    }

    async fn shutdown(&self, ctx: PluginContext) {
        tracing::info!(
            plugin = ctx.plugin_name,
            "trusted provider plugin shutdown completed"
        );
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), RuntimeError> {
    let host = PluginHost::builder()
        .add_plugin(TrustedProviderInlinePlugin)
        .build();
    let setup = RuntimeSetup::new().with_shred_trust_mode(ShredTrustMode::TrustedRawShredProvider);

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
            trust_mode = %ShredTrustMode::TrustedRawShredProvider.as_str(),
            "starting trusted raw shred provider example"
        );

        let producer_task = tokio::task::spawn_blocking(move || {
            af_xdp_ingress::run_af_xdp_producer_until(&tx, &config, producer_stop)
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
            .with_setup(setup)
            .with_kernel_bypass_ingress(rx)
            .run_until_termination_signal()
            .await;

        stop.store(true, Ordering::Relaxed);
        let producer_result = producer_wait_task.await.map_err(|error| {
            RuntimeError::Runloop(format!("AF_XDP producer waiter join failed: {error}"))
        })?;
        if let Err(error) = producer_result {
            return Err(error);
        }
        return runtime_result;
    }

    tracing::warn!(
        trust_mode = %ShredTrustMode::TrustedRawShredProvider.as_str(),
        "kernel-bypass producer not configured; falling back to normal SOF runtime bring-up"
    );
    ObserverRuntime::new()
        .with_plugin_host(host)
        .with_setup(setup)
        .run_until_termination_signal()
        .await
}
