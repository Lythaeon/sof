//! Public plugin framework surface for embedding custom observers into SOF runtime.

/// Experimental derived-state feed scaffold for official stateful extensions.
pub mod derived_state;
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
pub use derived_state::{
    AccountTouchObservedEvent, BranchReorgedEvent, CheckpointBarrierEvent, CheckpointBarrierReason,
    DerivedStateCheckpoint, DerivedStateCheckpointStore, DerivedStateConsumer,
    DerivedStateConsumerConfig, DerivedStateConsumerContext, DerivedStateConsumerFault,
    DerivedStateConsumerFaultKind, DerivedStateConsumerRecoveryState,
    DerivedStateConsumerSetupError, DerivedStateConsumerTelemetry, DerivedStateControlPlaneQuality,
    DerivedStateControlPlaneStateEvent, DerivedStateFeedEnvelope, DerivedStateFeedEvent,
    DerivedStateFreshnessState, DerivedStateHost, DerivedStateHostBuilder,
    DerivedStateInputFreshness, DerivedStateInvalidationEvent, DerivedStateInvalidationReason,
    DerivedStatePersistedCheckpoint, DerivedStateRecoveryReport, DerivedStateReplayBackend,
    DerivedStateReplayDurability, DerivedStateReplayError, DerivedStateReplaySource,
    DerivedStateReplayTelemetry, DerivedStateTxOutcomeEvent, DerivedStateTxOutcomeKind,
    DiskDerivedStateReplaySource, FeedSequence, FeedSessionId, FeedWatermarks,
    InMemoryDerivedStateReplaySource, SlotStatusChangedEvent, TransactionAppliedEvent,
};
pub use events::{
    AccountTouchEvent, AccountTouchEventRef, ClusterNodeInfo, ClusterTopologyEvent,
    ControlPlaneSource, DatasetEvent, LeaderScheduleEntry, LeaderScheduleEvent,
    ObservedRecentBlockhashEvent, RawPacketEvent, ReorgEvent, SerializedTransactionRange,
    ShredEvent, SlotStatusEvent, TransactionBatchEvent, TransactionEvent, TransactionEventRef,
    TransactionLogEvent, TransactionViewBatchEvent,
};
pub use extension::{
    ExtensionCapability, ExtensionContext, ExtensionManifest, ExtensionResourceSpec,
    ExtensionSetupError, ExtensionStreamVisibility, PacketSubscription, RuntimeExtension,
    RuntimePacketEvent, RuntimePacketEventClass, RuntimePacketSource, RuntimePacketSourceKind,
    RuntimePacketTransport, RuntimeWebSocketFrameType, TcpConnectorSpec, TcpListenerSpec,
    UdpListenerSpec, WsConnectorSpec,
};
pub use extension_host::{
    RuntimeExtensionCapabilityPolicy, RuntimeExtensionDispatchMetrics, RuntimeExtensionHost,
    RuntimeExtensionHostBuilder, RuntimeExtensionStartupFailure, RuntimeExtensionStartupReport,
};
pub use host::{PluginDispatchMode, PluginHost, PluginHostBuilder, PluginHostStartupError};
pub use plugin::ObserverPlugin as Plugin;
pub use plugin::{
    ObserverPlugin, PluginConfig, PluginContext, PluginSetupError, TransactionCommitmentSelector,
    TransactionDispatchMode, TransactionInterest, TransactionPrefilter,
};
