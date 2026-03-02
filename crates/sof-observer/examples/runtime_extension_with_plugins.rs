//! SOF runtime example running ObserverPlugin and RuntimeExtension hosts together.
#![doc(hidden)]

use async_trait::async_trait;
use sof::{
    event::TxKind,
    framework::{
        ExtensionCapability, ExtensionManifest, ExtensionStartupContext, PacketSubscription,
        Plugin, PluginHost, RuntimeExtension, RuntimeExtensionHost, RuntimePacketEvent,
        RuntimePacketSourceKind, TransactionEvent,
    },
};
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
struct NonVoteTxPlugin;

#[async_trait]
impl Plugin for NonVoteTxPlugin {
    fn name(&self) -> &'static str {
        "coexistence-non-vote-plugin"
    }

    async fn on_transaction(&self, event: TransactionEvent) {
        if event.kind == TxKind::VoteOnly {
            return;
        }
        tracing::info!(
            slot = event.slot,
            kind = ?event.kind,
            "plugin host observed non-vote transaction"
        );
    }
}

#[derive(Debug, Clone, Copy)]
struct ObserverIngressExtension;

#[async_trait]
impl RuntimeExtension for ObserverIngressExtension {
    fn name(&self) -> &'static str {
        "coexistence-observer-ingress-extension"
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
                ..PacketSubscription::default()
            }],
        })
    }

    async fn on_packet_received(&self, event: RuntimePacketEvent) {
        tracing::debug!(
            source = ?event.source.remote_addr,
            bytes = event.bytes.len(),
            "runtime extension host observed observer ingress packet"
        );
    }
}

#[derive(Debug, Error)]
enum RuntimeExtensionWithPluginsExampleError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error(transparent)]
    Runtime(#[from] sof::runtime::RuntimeError),
}

const fn require_release_mode() -> Result<(), RuntimeExtensionWithPluginsExampleError> {
    if cfg!(debug_assertions) {
        return Err(
            RuntimeExtensionWithPluginsExampleError::ReleaseModeRequired {
                command: "cargo run --release -p sof --example runtime_extension_with_plugins",
            },
        );
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), RuntimeExtensionWithPluginsExampleError> {
    require_release_mode()?;

    let plugin_host = PluginHost::builder().add_plugin(NonVoteTxPlugin).build();
    let extension_host = RuntimeExtensionHost::builder()
        .add_extension(ObserverIngressExtension)
        .build();

    tracing::info!(
        plugins = ?plugin_host.plugin_names(),
        extensions = ?extension_host.extension_names(),
        "starting SOF runtime with separate plugin + runtime extension hosts"
    );

    Ok(sof::runtime::run_async_with_hosts(plugin_host, extension_host).await?)
}
