//! Optional adapter layer for integrating `sof` runtime signals with `sof-tx`.

mod common;
mod derived_state;
mod plugin_host;

pub use common::{
    TxProviderAdapterConfig, TxProviderAdapterSnapshot, TxProviderControlPlaneSnapshot,
    TxProviderFlowSafetyIssue, TxProviderFlowSafetyPolicy, TxProviderFlowSafetyReport,
    TxProviderIngressSnapshot,
};
pub use derived_state::{
    DerivedStateTxProviderAdapter, DerivedStateTxProviderAdapterConfig,
    DerivedStateTxProviderAdapterPersistence,
};
pub use plugin_host::{PluginHostTxProviderAdapter, PluginHostTxProviderAdapterConfig};
