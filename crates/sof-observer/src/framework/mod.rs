//! Public plugin framework surface for embedding custom observers into SOF runtime.

/// Framework event payload types delivered to plugins.
pub mod events;
/// Runtime extension trait and manifest/filter/resource types.
pub mod extension;
/// Runtime extension host builder and dispatcher.
pub mod extension_host;
/// Plugin host builder and dispatcher.
pub mod host;
/// Plugin trait implemented by user extensions.
pub mod plugin;

pub use crate::event::{ForkSlotStatus, TxCommitmentStatus};
pub use events::{
    ClusterNodeInfo, ClusterTopologyEvent, ControlPlaneSource, DatasetEvent, LeaderScheduleEntry,
    LeaderScheduleEvent, ObservedRecentBlockhashEvent, RawPacketEvent, ReorgEvent, ShredEvent,
    SlotStatusEvent, TransactionEvent,
};
pub use extension::{
    ExtensionCapability, ExtensionManifest, ExtensionResourceSpec, ExtensionShutdownContext,
    ExtensionStartupContext, ExtensionStreamVisibility, PacketSubscription, RuntimeExtension,
    RuntimePacketEvent, RuntimePacketEventClass, RuntimePacketSource, RuntimePacketSourceKind,
    RuntimePacketTransport, RuntimeWebSocketFrameType, TcpConnectorSpec, TcpListenerSpec,
    UdpListenerSpec, WsConnectorSpec,
};
pub use extension_host::{
    RuntimeExtensionCapabilityPolicy, RuntimeExtensionDispatchMetrics, RuntimeExtensionHost,
    RuntimeExtensionHostBuilder, RuntimeExtensionStartupFailure, RuntimeExtensionStartupReport,
};
pub use host::{PluginDispatchMode, PluginHost, PluginHostBuilder};
pub use plugin::ObserverPlugin;
pub use plugin::ObserverPlugin as Plugin;
