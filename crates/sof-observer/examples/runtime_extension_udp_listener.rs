//! SOF runtime example with one RuntimeExtension that provisions a private UDP listener.
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

const DEFAULT_EXTENSION_BIND_ADDR: &str = "127.0.0.1:21011";
const RESOURCE_ID: &str = "demo-local-udp";

#[derive(Debug, Clone)]
struct LocalUdpListenerExtension {
    bind_addr: SocketAddr,
}

#[async_trait]
impl RuntimeExtension for LocalUdpListenerExtension {
    fn name(&self) -> &'static str {
        "local-udp-listener-extension"
    }

    async fn on_startup(
        &self,
        _ctx: ExtensionStartupContext,
    ) -> Result<ExtensionManifest, sof::framework::extension::ExtensionStartupError> {
        Ok(ExtensionManifest {
            capabilities: vec![ExtensionCapability::BindUdp],
            resources: vec![ExtensionResourceSpec::UdpListener(UdpListenerSpec {
                resource_id: RESOURCE_ID.to_owned(),
                bind_addr: self.bind_addr,
                visibility: ExtensionStreamVisibility::Private,
                read_buffer_bytes: 2_048,
            })],
            subscriptions: vec![PacketSubscription {
                source_kind: Some(RuntimePacketSourceKind::ExtensionResource),
                owner_extension: Some(self.name().to_owned()),
                resource_id: Some(RESOURCE_ID.to_owned()),
                ..PacketSubscription::default()
            }],
        })
    }

    async fn on_packet_received(&self, event: RuntimePacketEvent) {
        tracing::info!(
            local = ?event.source.local_addr,
            remote = ?event.source.remote_addr,
            bytes = event.bytes.len(),
            "runtime extension received packet on extension-owned udp listener"
        );
    }

    async fn on_shutdown(&self, _ctx: ExtensionShutdownContext) {
        tracing::info!("local udp listener runtime extension shutting down");
    }
}

#[derive(Debug, Error)]
enum RuntimeExtensionUdpListenerExampleError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error("invalid SOF_EXTENSION_UDP_BIND address `{value}`: {source}")]
    InvalidBindAddress {
        value: String,
        source: std::net::AddrParseError,
    },
    #[error("failed to bind local udp sender socket: {source}")]
    BindSenderSocket { source: std::io::Error },
    #[error("failed to send local udp demo packet to {target}: {source}")]
    SendDemoPacket {
        target: SocketAddr,
        source: std::io::Error,
    },
    #[error(transparent)]
    Runtime(#[from] sof::runtime::RuntimeError),
}

const fn require_release_mode() -> Result<(), RuntimeExtensionUdpListenerExampleError> {
    if cfg!(debug_assertions) {
        return Err(
            RuntimeExtensionUdpListenerExampleError::ReleaseModeRequired {
                command: "cargo run --release -p sof --example runtime_extension_udp_listener",
            },
        );
    }
    Ok(())
}

fn read_extension_bind_addr() -> Result<SocketAddr, RuntimeExtensionUdpListenerExampleError> {
    std::env::var("SOF_EXTENSION_UDP_BIND").map_or_else(
        |_| {
            SocketAddr::from_str(DEFAULT_EXTENSION_BIND_ADDR).map_err(|source| {
                RuntimeExtensionUdpListenerExampleError::InvalidBindAddress {
                    value: DEFAULT_EXTENSION_BIND_ADDR.to_owned(),
                    source,
                }
            })
        },
        |value| {
            SocketAddr::from_str(&value).map_err(|source| {
                RuntimeExtensionUdpListenerExampleError::InvalidBindAddress { value, source }
            })
        },
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), RuntimeExtensionUdpListenerExampleError> {
    require_release_mode()?;
    let bind_addr = read_extension_bind_addr()?;
    let extension_host = RuntimeExtensionHost::builder()
        .add_extension(LocalUdpListenerExtension { bind_addr })
        .build();
    tracing::info!(
        bind_addr = %bind_addr,
        env = "SOF_EXTENSION_UDP_BIND",
        "starting runtime extension udp listener example"
    );

    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        let sender = UdpSocket::bind("127.0.0.1:0")
            .await
            .map_err(|source| RuntimeExtensionUdpListenerExampleError::BindSenderSocket { source });
        let Ok(sender) = sender else {
            tracing::warn!("failed to bind local demo sender socket");
            return;
        };
        loop {
            let payload = b"runtime-extension-udp-demo";
            if let Err(source) = sender.send_to(payload, bind_addr).await {
                let error = RuntimeExtensionUdpListenerExampleError::SendDemoPacket {
                    target: bind_addr,
                    source,
                };
                tracing::warn!(error = %error, "failed to send udp demo packet");
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        }
    });

    Ok(sof::runtime::run_async_with_extension_host(extension_host).await?)
}
