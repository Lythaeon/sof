//! Public plugin framework surface for embedding custom observers into SOF runtime.

/// Framework event payload types delivered to plugins.
pub mod events;
/// Plugin host builder and dispatcher.
pub mod host;
/// Plugin trait implemented by user extensions.
pub mod plugin;

pub use events::{
    ClusterNodeInfo, ClusterTopologyEvent, ControlPlaneSource, DatasetEvent, LeaderScheduleEntry,
    LeaderScheduleEvent, RawPacketEvent, ShredEvent, TransactionEvent,
};
pub use host::{PluginDispatchMode, PluginHost, PluginHostBuilder};
pub use plugin::ObserverPlugin;
pub use plugin::ObserverPlugin as Plugin;
