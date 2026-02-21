//! Optional adapter layer for integrating `sof` runtime signals with `sof-tx`.

mod plugin_host;

pub use plugin_host::{PluginHostTxProviderAdapter, PluginHostTxProviderAdapterConfig};
