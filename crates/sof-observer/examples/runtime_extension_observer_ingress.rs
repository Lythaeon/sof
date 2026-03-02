//! SOF runtime example with one RuntimeExtension subscribed to observer ingress packets.
#![doc(hidden)]

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use async_trait::async_trait;
use sof::framework::{
    ExtensionCapability, ExtensionManifest, ExtensionShutdownContext, ExtensionStartupContext,
    PacketSubscription, RuntimeExtension, RuntimeExtensionHost, RuntimePacketEvent,
    RuntimePacketSourceKind, RuntimePacketTransport,
};
use thiserror::Error;

#[derive(Debug, Default, Clone)]
struct ObserverIngressExtension {
    packets: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
}

#[async_trait]
impl RuntimeExtension for ObserverIngressExtension {
    fn name(&self) -> &'static str {
        "observer-ingress-extension"
    }

    async fn on_startup(
        &self,
        _ctx: ExtensionStartupContext,
    ) -> Result<ExtensionManifest, sof::framework::extension::ExtensionStartupError> {
        Ok(ExtensionManifest {
            capabilities: vec![ExtensionCapability::ObserveObserverIngress],
            resources: Vec::new(),
            subscriptions: vec![PacketSubscription {
                source_kind: Some(RuntimePacketSourceKind::ObserverIngress),
                transport: Some(RuntimePacketTransport::Udp),
                ..PacketSubscription::default()
            }],
        })
    }

    async fn on_packet_received(&self, event: RuntimePacketEvent) {
        let packets = self
            .packets
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        self.bytes.fetch_add(
            u64::try_from(event.bytes.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        if packets <= 5 || packets.is_multiple_of(10_000) {
            tracing::info!(
                packets,
                source = ?event.source.remote_addr,
                bytes = event.bytes.len(),
                "observer ingress packet matched runtime extension filter"
            );
        }
    }

    async fn on_shutdown(&self, _ctx: ExtensionShutdownContext) {
        tracing::info!(
            packets = self.packets.load(Ordering::Relaxed),
            bytes = self.bytes.load(Ordering::Relaxed),
            "runtime extension shutdown summary"
        );
    }
}

#[derive(Debug, Error)]
enum RuntimeExtensionObserverIngressExampleError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error(transparent)]
    Runtime(#[from] sof::runtime::RuntimeError),
}

const fn require_release_mode() -> Result<(), RuntimeExtensionObserverIngressExampleError> {
    if cfg!(debug_assertions) {
        return Err(
            RuntimeExtensionObserverIngressExampleError::ReleaseModeRequired {
                command: "cargo run --release -p sof --example runtime_extension_observer_ingress",
            },
        );
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), RuntimeExtensionObserverIngressExampleError> {
    require_release_mode()?;
    let extension_host = RuntimeExtensionHost::builder()
        .add_extension(ObserverIngressExtension::default())
        .build();
    tracing::info!(
        extensions = ?extension_host.extension_names(),
        "starting SOF runtime with runtime extension host"
    );
    Ok(sof::runtime::run_async_with_extension_host(extension_host).await?)
}
