//! SOF runtime example showing owner-only vs shared stream behavior between RuntimeExtensions.
#![doc(hidden)]

use std::{net::SocketAddr, str::FromStr};

use async_trait::async_trait;
use sof::framework::{
    ExtensionCapability, ExtensionManifest, ExtensionResourceSpec, ExtensionShutdownContext,
    ExtensionStartupContext, ExtensionStreamVisibility, PacketSubscription, RuntimeExtension,
    RuntimeExtensionHost, RuntimePacketEvent, RuntimePacketSourceKind, UdpListenerSpec,
};
use thiserror::Error;
use tokio::net::UdpSocket;

const DEFAULT_SHARED_BIND_ADDR: &str = "127.0.0.1:21012";
const SHARED_RESOURCE_ID: &str = "shared-udp-feed";
const SHARED_TAG: &str = "demo.shared.feed";

#[derive(Debug, Clone)]
struct SharedFeedOwnerExtension {
    bind_addr: SocketAddr,
}

#[async_trait]
impl RuntimeExtension for SharedFeedOwnerExtension {
    fn name(&self) -> &'static str {
        "shared-feed-owner-extension"
    }

    async fn on_startup(
        &self,
        _ctx: ExtensionStartupContext,
    ) -> Result<ExtensionManifest, sof::framework::extension::ExtensionStartupError> {
        Ok(ExtensionManifest {
            capabilities: vec![ExtensionCapability::BindUdp],
            resources: vec![ExtensionResourceSpec::UdpListener(UdpListenerSpec {
                resource_id: SHARED_RESOURCE_ID.to_owned(),
                bind_addr: self.bind_addr,
                visibility: ExtensionStreamVisibility::Shared {
                    tag: SHARED_TAG.to_owned(),
                },
                read_buffer_bytes: 2_048,
            })],
            subscriptions: vec![PacketSubscription {
                source_kind: Some(RuntimePacketSourceKind::ExtensionResource),
                owner_extension: Some(self.name().to_owned()),
                resource_id: Some(SHARED_RESOURCE_ID.to_owned()),
                ..PacketSubscription::default()
            }],
        })
    }

    async fn on_packet_received(&self, event: RuntimePacketEvent) {
        tracing::info!(
            extension = self.name(),
            bytes = event.bytes.len(),
            remote = ?event.source.remote_addr,
            "owner extension observed packet"
        );
    }

    async fn on_shutdown(&self, _ctx: ExtensionShutdownContext) {
        tracing::info!(extension = self.name(), "owner extension shutdown");
    }
}

#[derive(Debug, Clone, Copy)]
struct SharedFeedConsumerExtension;

#[async_trait]
impl RuntimeExtension for SharedFeedConsumerExtension {
    fn name(&self) -> &'static str {
        "shared-feed-consumer-extension"
    }

    async fn on_startup(
        &self,
        _ctx: ExtensionStartupContext,
    ) -> Result<ExtensionManifest, sof::framework::extension::ExtensionStartupError> {
        Ok(ExtensionManifest {
            capabilities: vec![ExtensionCapability::ObserveSharedExtensionStream],
            resources: Vec::new(),
            subscriptions: vec![PacketSubscription {
                source_kind: Some(RuntimePacketSourceKind::ExtensionResource),
                shared_tag: Some(SHARED_TAG.to_owned()),
                ..PacketSubscription::default()
            }],
        })
    }

    async fn on_packet_received(&self, event: RuntimePacketEvent) {
        tracing::info!(
            extension = self.name(),
            bytes = event.bytes.len(),
            source_owner = ?event.source.owner_extension,
            source_resource = ?event.source.resource_id,
            "consumer extension observed shared-tag packet"
        );
    }
}

#[derive(Debug, Error)]
enum RuntimeExtensionSharedStreamExampleError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error("invalid SOF_EXTENSION_SHARED_BIND address `{value}`: {source}")]
    InvalidBindAddress {
        value: String,
        source: std::net::AddrParseError,
    },
    #[error(transparent)]
    Runtime(#[from] sof::runtime::RuntimeError),
}

const fn require_release_mode() -> Result<(), RuntimeExtensionSharedStreamExampleError> {
    if cfg!(debug_assertions) {
        return Err(
            RuntimeExtensionSharedStreamExampleError::ReleaseModeRequired {
                command: "cargo run --release -p sof --example runtime_extension_shared_stream",
            },
        );
    }
    Ok(())
}

fn read_bind_addr() -> Result<SocketAddr, RuntimeExtensionSharedStreamExampleError> {
    std::env::var("SOF_EXTENSION_SHARED_BIND").map_or_else(
        |_| {
            SocketAddr::from_str(DEFAULT_SHARED_BIND_ADDR).map_err(|source| {
                RuntimeExtensionSharedStreamExampleError::InvalidBindAddress {
                    value: DEFAULT_SHARED_BIND_ADDR.to_owned(),
                    source,
                }
            })
        },
        |value| {
            SocketAddr::from_str(&value).map_err(|source| {
                RuntimeExtensionSharedStreamExampleError::InvalidBindAddress { value, source }
            })
        },
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), RuntimeExtensionSharedStreamExampleError> {
    require_release_mode()?;
    let bind_addr = read_bind_addr()?;
    let extension_host = RuntimeExtensionHost::builder()
        .add_extension(SharedFeedOwnerExtension { bind_addr })
        .add_extension(SharedFeedConsumerExtension)
        .build();

    tracing::info!(
        bind_addr = %bind_addr,
        shared_tag = SHARED_TAG,
        "starting shared stream runtime extension example"
    );

    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        let Ok(sender) = UdpSocket::bind("127.0.0.1:0").await else {
            tracing::warn!("failed to bind shared-stream demo sender");
            return;
        };
        loop {
            if sender
                .send_to(b"shared-stream-demo", bind_addr)
                .await
                .is_err()
            {
                tracing::warn!("failed to send shared-stream demo packet");
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        }
    });

    Ok(sof::runtime::run_async_with_extension_host(extension_host).await?)
}
