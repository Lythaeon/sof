//! SOF runtime example with one RuntimeExtension that consumes full WebSocket messages.
#![doc(hidden)]

use async_trait::async_trait;
use futures_util::SinkExt;
use sof::framework::{
    ExtensionCapability, ExtensionManifest, ExtensionResourceSpec, ExtensionStartupContext,
    ExtensionStreamVisibility, PacketSubscription, RuntimeExtension, RuntimeExtensionHost,
    RuntimePacketEvent, RuntimePacketEventClass, RuntimePacketSourceKind, WsConnectorSpec,
};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};

const WS_RESOURCE_ID: &str = "demo-websocket-feed";

#[derive(Debug, Clone)]
struct WebSocketConsumerExtension {
    url: String,
}

#[async_trait]
impl RuntimeExtension for WebSocketConsumerExtension {
    fn name(&self) -> &'static str {
        "websocket-consumer-extension"
    }

    async fn on_startup(
        &self,
        _ctx: ExtensionStartupContext,
    ) -> Result<ExtensionManifest, sof::framework::extension::ExtensionStartupError> {
        Ok(ExtensionManifest {
            capabilities: vec![ExtensionCapability::ConnectWebSocket],
            resources: vec![ExtensionResourceSpec::WsConnector(WsConnectorSpec {
                resource_id: WS_RESOURCE_ID.to_owned(),
                url: self.url.clone(),
                visibility: ExtensionStreamVisibility::Private,
                read_buffer_bytes: 2_048,
            })],
            subscriptions: vec![PacketSubscription {
                source_kind: Some(RuntimePacketSourceKind::ExtensionResource),
                owner_extension: Some(self.name().to_owned()),
                resource_id: Some(WS_RESOURCE_ID.to_owned()),
                ..PacketSubscription::default()
            }],
        })
    }

    async fn on_packet_received(&self, event: RuntimePacketEvent) {
        if event.source.event_class == RuntimePacketEventClass::ConnectionClosed {
            tracing::info!(
                source = ?event.source.remote_addr,
                "runtime extension observed websocket connection closed"
            );
            return;
        }
        let preview = String::from_utf8_lossy(event.bytes.as_ref());
        tracing::info!(
            frame_type = ?event.source.websocket_frame_type,
            source = ?event.source.remote_addr,
            bytes = event.bytes.len(),
            preview = %preview,
            "runtime extension received websocket message payload"
        );
    }
}

#[derive(Debug, Error)]
enum RuntimeExtensionWebSocketConnectorExampleError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error("failed to bind local websocket demo server: {source}")]
    BindServer { source: std::io::Error },
    #[error(transparent)]
    Runtime(#[from] sof::runtime::RuntimeError),
}

const fn require_release_mode() -> Result<(), RuntimeExtensionWebSocketConnectorExampleError> {
    if cfg!(debug_assertions) {
        return Err(
            RuntimeExtensionWebSocketConnectorExampleError::ReleaseModeRequired {
                command: "cargo run --release -p sof --example runtime_extension_websocket_connector",
            },
        );
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), RuntimeExtensionWebSocketConnectorExampleError> {
    require_release_mode()?;

    let server = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|source| RuntimeExtensionWebSocketConnectorExampleError::BindServer { source })?;
    let server_addr = server
        .local_addr()
        .map_err(|source| RuntimeExtensionWebSocketConnectorExampleError::BindServer { source })?;
    let ws_url = format!("ws://{server_addr}/feed");
    tracing::info!(ws_url = %ws_url, "starting local websocket demo server");

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = server.accept().await else {
                tracing::warn!("websocket demo server accept failed");
                break;
            };
            tokio::spawn(async move {
                let Ok(mut websocket) = accept_async(stream).await else {
                    tracing::warn!("websocket demo handshake failed");
                    return;
                };
                if let Err(error) = websocket
                    .send(Message::Text("runtime-extension-ws-text".into()))
                    .await
                {
                    tracing::warn!(error = %error, "websocket demo text send failed");
                    return;
                }
                if let Err(error) = websocket
                    .send(Message::Binary(vec![1_u8, 2_u8, 3_u8, 4_u8].into()))
                    .await
                {
                    tracing::warn!(error = %error, "websocket demo binary send failed");
                }
            });
        }
    });

    let extension_host = RuntimeExtensionHost::builder()
        .add_extension(WebSocketConsumerExtension { url: ws_url })
        .build();

    Ok(sof::runtime::run_async_with_extension_host(extension_host).await?)
}
