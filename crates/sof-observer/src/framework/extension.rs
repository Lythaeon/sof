use std::{net::SocketAddr, sync::Arc};

use async_trait::async_trait;
use thiserror::Error;

/// Declarative capability required by a [`RuntimeExtension`] manifest.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum ExtensionCapability {
    /// Bind one or more UDP listeners.
    BindUdp,
    /// Bind one or more TCP listeners.
    BindTcp,
    /// Open one or more TCP client connectors.
    ConnectTcp,
    /// Open one or more WebSocket-style connectors.
    ConnectWebSocket,
    /// Receive packets from observer ingress sources.
    ObserveObserverIngress,
    /// Receive packets from other extensions that expose shared streams.
    ObserveSharedExtensionStream,
}

impl ExtensionCapability {
    /// Returns all known capabilities for allow-all defaults.
    #[must_use]
    pub const fn all() -> [Self; 6] {
        [
            Self::BindUdp,
            Self::BindTcp,
            Self::ConnectTcp,
            Self::ConnectWebSocket,
            Self::ObserveObserverIngress,
            Self::ObserveSharedExtensionStream,
        ]
    }
}

/// Visibility policy for extension-owned packet streams.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ExtensionStreamVisibility {
    /// Stream is visible only to the owning extension.
    Private,
    /// Stream is visible to other extensions that subscribe to this tag.
    Shared {
        /// Shared stream namespace used by cross-extension subscriptions.
        tag: String,
    },
}

impl Default for ExtensionStreamVisibility {
    fn default() -> Self {
        Self::Private
    }
}

/// Declarative UDP listener resource requested at startup.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct UdpListenerSpec {
    /// Stable resource identifier scoped to one extension manifest.
    pub resource_id: String,
    /// Socket bind address.
    pub bind_addr: SocketAddr,
    /// Visibility policy for packets emitted from this socket.
    pub visibility: ExtensionStreamVisibility,
    /// Read buffer size for one datagram read operation.
    pub read_buffer_bytes: usize,
}

/// Declarative TCP listener resource requested at startup.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TcpListenerSpec {
    /// Stable resource identifier scoped to one extension manifest.
    pub resource_id: String,
    /// Socket bind address.
    pub bind_addr: SocketAddr,
    /// Visibility policy for packets emitted from this socket.
    pub visibility: ExtensionStreamVisibility,
    /// Read buffer size for one socket read operation.
    pub read_buffer_bytes: usize,
}

/// Declarative TCP connector resource requested at startup.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TcpConnectorSpec {
    /// Stable resource identifier scoped to one extension manifest.
    pub resource_id: String,
    /// Remote endpoint to connect to.
    pub remote_addr: SocketAddr,
    /// Visibility policy for packets emitted from this socket.
    pub visibility: ExtensionStreamVisibility,
    /// Read buffer size for one socket read operation.
    pub read_buffer_bytes: usize,
}

/// Declarative WebSocket connector resource requested at startup.
///
/// Supports `ws://` and `wss://` URLs with full WebSocket handshake and frame decoding.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct WsConnectorSpec {
    /// Stable resource identifier scoped to one extension manifest.
    pub resource_id: String,
    /// WebSocket URL string.
    pub url: String,
    /// Visibility policy for packets emitted from this socket.
    pub visibility: ExtensionStreamVisibility,
    /// Read buffer size for one socket read operation.
    pub read_buffer_bytes: usize,
}

/// Resource declarations accepted from [`RuntimeExtension::on_startup`].
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ExtensionResourceSpec {
    /// UDP listener resource.
    UdpListener(UdpListenerSpec),
    /// TCP listener resource.
    TcpListener(TcpListenerSpec),
    /// TCP client connector resource.
    TcpConnector(TcpConnectorSpec),
    /// WebSocket-style client connector resource.
    WsConnector(WsConnectorSpec),
}

/// Packet event source category used by runtime filters.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum RuntimePacketSourceKind {
    /// Packet originated from the observer runtime ingress path.
    ObserverIngress,
    /// Packet originated from one extension-owned resource.
    ExtensionResource,
}

/// Transport class used by runtime packet source metadata.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum RuntimePacketTransport {
    /// UDP datagram transport.
    Udp,
    /// TCP stream transport.
    Tcp,
    /// WebSocket-style stream transport.
    WebSocket,
}

/// Runtime packet event classification.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum RuntimePacketEventClass {
    /// Generic packet bytes delivered from one source.
    Packet,
    /// Transport connection was closed by remote or runtime teardown.
    ConnectionClosed,
}

/// WebSocket frame category metadata for decoded WebSocket events.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum RuntimeWebSocketFrameType {
    /// Text frame payload.
    Text,
    /// Binary frame payload.
    Binary,
    /// Ping control frame payload.
    Ping,
    /// Pong control frame payload.
    Pong,
}

/// Packet source metadata shared across runtime extension dispatch.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RuntimePacketSource {
    /// Source category.
    pub kind: RuntimePacketSourceKind,
    /// Transport category.
    pub transport: RuntimePacketTransport,
    /// Event class.
    pub event_class: RuntimePacketEventClass,
    /// Owning extension name for extension resource events.
    pub owner_extension: Option<String>,
    /// Owning extension resource identifier.
    pub resource_id: Option<String>,
    /// Shared stream tag when stream visibility is shared.
    pub shared_tag: Option<String>,
    /// WebSocket frame type when `transport` is `WebSocket`.
    pub websocket_frame_type: Option<RuntimeWebSocketFrameType>,
    /// Local socket address where available.
    pub local_addr: Option<SocketAddr>,
    /// Remote socket address where available.
    pub remote_addr: Option<SocketAddr>,
}

/// Packet event delivered to [`RuntimeExtension::on_packet_received`].
#[derive(Debug, Clone)]
pub struct RuntimePacketEvent {
    /// Source metadata used for immutable filter matching.
    pub source: RuntimePacketSource,
    /// Raw payload bytes (`Packet` events carry data; lifecycle events may be empty).
    pub bytes: Arc<[u8]>,
    /// Millisecond unix timestamp captured at ingest.
    pub observed_unix_ms: u64,
}

/// Immutable packet subscription filter declared in extension startup manifest.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct PacketSubscription {
    /// Match on source kind.
    pub source_kind: Option<RuntimePacketSourceKind>,
    /// Match on transport category.
    pub transport: Option<RuntimePacketTransport>,
    /// Match on event class.
    pub event_class: Option<RuntimePacketEventClass>,
    /// Match on local endpoint exactly.
    pub local_addr: Option<SocketAddr>,
    /// Match on local port.
    pub local_port: Option<u16>,
    /// Match on remote endpoint exactly.
    pub remote_addr: Option<SocketAddr>,
    /// Match on remote port.
    pub remote_port: Option<u16>,
    /// Match on owning extension name.
    pub owner_extension: Option<String>,
    /// Match on extension resource id.
    pub resource_id: Option<String>,
    /// Match on shared stream tag.
    pub shared_tag: Option<String>,
    /// Match on WebSocket frame type.
    pub websocket_frame_type: Option<RuntimeWebSocketFrameType>,
}

impl PacketSubscription {
    /// Returns true when this subscription matches the provided runtime packet event.
    #[must_use]
    pub fn matches(&self, event: &RuntimePacketEvent) -> bool {
        if let Some(source_kind) = self.source_kind
            && source_kind != event.source.kind
        {
            return false;
        }
        if let Some(transport) = self.transport
            && transport != event.source.transport
        {
            return false;
        }
        if let Some(event_class) = self.event_class
            && event_class != event.source.event_class
        {
            return false;
        }
        if let Some(local_addr) = self.local_addr
            && event.source.local_addr != Some(local_addr)
        {
            return false;
        }
        if let Some(local_port) = self.local_port
            && event.source.local_addr.map(|addr| addr.port()) != Some(local_port)
        {
            return false;
        }
        if let Some(remote_addr) = self.remote_addr
            && event.source.remote_addr != Some(remote_addr)
        {
            return false;
        }
        if let Some(remote_port) = self.remote_port
            && event.source.remote_addr.map(|addr| addr.port()) != Some(remote_port)
        {
            return false;
        }
        if let Some(owner_extension) = self.owner_extension.as_ref()
            && event.source.owner_extension.as_ref() != Some(owner_extension)
        {
            return false;
        }
        if let Some(resource_id) = self.resource_id.as_ref()
            && event.source.resource_id.as_ref() != Some(resource_id)
        {
            return false;
        }
        if let Some(shared_tag) = self.shared_tag.as_ref()
            && event.source.shared_tag.as_ref() != Some(shared_tag)
        {
            return false;
        }
        if let Some(websocket_frame_type) = self.websocket_frame_type
            && event.source.websocket_frame_type != Some(websocket_frame_type)
        {
            return false;
        }
        true
    }
}

/// Startup manifest returned by [`RuntimeExtension::on_startup`].
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct ExtensionManifest {
    /// Declared capabilities required by this extension.
    pub capabilities: Vec<ExtensionCapability>,
    /// Declarative runtime resources to provision for this extension.
    pub resources: Vec<ExtensionResourceSpec>,
    /// Immutable packet subscriptions used for dispatch filtering.
    pub subscriptions: Vec<PacketSubscription>,
}

/// Startup context passed to [`RuntimeExtension::on_startup`].
#[derive(Debug, Clone)]
pub struct ExtensionStartupContext {
    /// Extension identifier.
    pub extension_name: &'static str,
}

/// Shutdown context passed to [`RuntimeExtension::on_shutdown`].
#[derive(Debug, Clone)]
pub struct ExtensionShutdownContext {
    /// Extension identifier.
    pub extension_name: &'static str,
}

/// Extension startup failure.
#[derive(Debug, Clone, Error, Eq, PartialEq)]
#[error("{reason}")]
pub struct ExtensionStartupError {
    reason: String,
}

impl ExtensionStartupError {
    /// Creates a startup error with a human-readable reason.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// Separate runtime extension surface from observer plugin hooks.
#[async_trait]
pub trait RuntimeExtension: Send + Sync + 'static {
    /// Stable extension identifier used in startup logs and diagnostics.
    ///
    /// Production extensions should override this with a stable literal identifier.
    /// The default value uses Rust type names and can change when refactoring.
    fn name(&self) -> &'static str {
        core::any::type_name::<Self>()
    }

    /// Returns true when `name()` is overridden with a stable identifier.
    ///
    /// Hosts may use this for startup validation in hardened environments.
    fn has_explicit_name(&self) -> bool {
        self.name() != core::any::type_name::<Self>()
    }

    /// Called once during runtime startup to request capabilities, resources, and subscriptions.
    async fn on_startup(
        &self,
        _ctx: ExtensionStartupContext,
    ) -> Result<ExtensionManifest, ExtensionStartupError> {
        Ok(ExtensionManifest::default())
    }

    /// Called for packet events matching this extension's immutable startup subscriptions.
    async fn on_packet_received(&self, _event: RuntimePacketEvent) {}

    /// Called during runtime shutdown before force-cancel logic runs.
    async fn on_shutdown(&self, _ctx: ExtensionShutdownContext) {}
}
