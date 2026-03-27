//! Processed provider-stream ingress surfaces for SOF.
//!
//! Use this module when the upstream source is already beyond raw shreds, for
//! example Yellowstone gRPC or LaserStream-style processed transaction feeds.
//! These updates bypass SOF's packet, shred, FEC, and reconstruction stages and
//! enter directly at the plugin/derived-state transaction layer.
//!
//! Built-in mode capability summary:
//!
//! - `YellowstoneGrpc`: built-in Yellowstone transaction feed
//! - `YellowstoneGrpcTransactionStatus`: built-in Yellowstone transaction-status feed
//! - `YellowstoneGrpcAccounts`: built-in Yellowstone account-update feed
//! - `YellowstoneGrpcBlockMeta`: built-in Yellowstone block-meta feed
//! - `YellowstoneGrpcSlots`: built-in Yellowstone slot feed
//! - `LaserStream`: built-in LaserStream transaction feed
//! - `LaserStreamTransactionStatus`: built-in LaserStream transaction-status feed
//! - `LaserStreamAccounts`: built-in LaserStream account-update feed
//! - `LaserStreamBlockMeta`: built-in LaserStream block-meta feed
//! - `LaserStreamSlots`: built-in LaserStream slot feed
//! - `WebsocketTransaction`: built-in websocket `transactionSubscribe`
//! - `WebsocketLogs`: built-in websocket `logsSubscribe`
//! - `WebsocketAccount`: built-in websocket `accountSubscribe`
//! - `WebsocketProgram`: built-in websocket `programSubscribe`
//!
//! Each built-in source config can report its matching runtime mode directly
//! through `runtime_mode()`. `ProviderStreamMode::Generic` remains the typed
//! custom-adapter path and the fan-in mode when you want to combine multiple
//! heterogeneous upstream sources into one runtime ingress.
//!
//! Generic provider producers may still enqueue `TransactionViewBatch`,
//! `BlockMeta`, `RecentBlockhash`, `SlotStatus`, `ClusterTopology`,
//! `LeaderSchedule`, or `Reorg` updates directly.
//!
//! `ProviderStreamMode::Generic` is SOF's typed custom-adapter mode.
//! Your producer ingests an upstream format and maps it into one of the
//! `ProviderStreamUpdate` variants below before handing it to SOF.
//!
//! Built-in provider source configs extend that same typed surface:
//!
//! - websocket:
//!   - [`websocket::WebsocketTransactionConfig`] can target
//!     [`websocket::WebsocketPrimaryStream::Transaction`],
//!     [`websocket::WebsocketPrimaryStream::Account`], or
//!     [`websocket::WebsocketPrimaryStream::Program`]
//!   - [`websocket::WebsocketLogsConfig`] targets `logsSubscribe`
//! - Yellowstone:
//!   - [`yellowstone::YellowstoneGrpcConfig`] can target
//!     [`yellowstone::YellowstoneGrpcStream::Transaction`],
//!     [`yellowstone::YellowstoneGrpcStream::TransactionStatus`],
//!     [`yellowstone::YellowstoneGrpcStream::Accounts`], or
//!     [`yellowstone::YellowstoneGrpcStream::BlockMeta`]
//!   - [`yellowstone::YellowstoneGrpcSlotsConfig`] targets slot updates
//! - LaserStream:
//!   - [`laserstream::LaserStreamConfig`] can target
//!     [`laserstream::LaserStreamStream::Transaction`],
//!     [`laserstream::LaserStreamStream::TransactionStatus`],
//!     [`laserstream::LaserStreamStream::Accounts`], or
//!     [`laserstream::LaserStreamStream::BlockMeta`]
//!   - [`laserstream::LaserStreamSlotsConfig`] targets slot updates
//!
//! Those source selectors do not create a second runtime API. They extend the
//! existing provider config objects and emit the matching `ProviderStreamUpdate`
//! variants into the same runtime dispatch path.
//!
//! Built-in configs may also set:
//!
//! - a stable source instance label for observability via `with_source_instance(...)`
//! - whether one source is readiness-gating or optional via `with_readiness(...)`
//!
//! Variant-to-runtime mapping:
//!
//! - `Transaction`:
//!   - drives `on_transaction`
//!   - drives derived-state transaction apply when enabled
//!   - synthesizes `on_recent_blockhash` from the transaction message when that
//!     hook is requested
//! - `SerializedTransaction`:
//!   - same transaction-family path, but lets SOF prefilter before full decode
//! - `TransactionLog`:
//!   - drives `on_transaction_log`
//! - `TransactionStatus`:
//!   - drives `on_transaction_status`
//! - `TransactionViewBatch`:
//!   - drives `on_transaction_view_batch`
//! - `AccountUpdate`:
//!   - drives `on_account_update`
//! - `BlockMeta`:
//!   - drives `on_block_meta`
//! - `RecentBlockhash`:
//!   - drives `on_recent_blockhash`
//! - `SlotStatus`:
//!   - drives `on_slot_status`
//! - `ClusterTopology`:
//!   - drives `on_cluster_topology`
//! - `LeaderSchedule`:
//!   - drives `on_leader_schedule`
//! - `Reorg`:
//!   - drives `on_reorg`
//! - `Health`:
//!   - updates provider health/readiness/observability
//!   - does not dispatch into plugin hooks
//!
//! # Feed Provider Transactions Into SOF
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use solana_hash::Hash;
//! use solana_keypair::Keypair;
//! use solana_message::{Message, VersionedMessage};
//! use solana_signer::Signer;
//! use solana_transaction::versioned::VersionedTransaction;
//! use sof::{
//!     event::{TxCommitmentStatus, TxKind},
//!     framework::TransactionEvent,
//!     provider_stream::{
//!         create_provider_stream_queue, ProviderStreamMode, ProviderStreamUpdate,
//!     },
//!     runtime::ObserverRuntime,
//! };
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let (tx, rx) = create_provider_stream_queue(128);
//! let signer = Keypair::new();
//! let message = Message::new(&[], Some(&signer.pubkey()));
//! let transaction = VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[&signer])?;
//!
//! tx.send(ProviderStreamUpdate::Transaction(TransactionEvent {
//!     slot: 1,
//!     commitment_status: TxCommitmentStatus::Processed,
//!     confirmed_slot: None,
//!     finalized_slot: None,
//!     signature: transaction.signatures.first().copied(),
//!     tx: Arc::new(transaction),
//!     kind: TxKind::NonVote,
//! }))
//! .await?;
//!
//! ObserverRuntime::new()
//!     .with_provider_stream_ingress(ProviderStreamMode::Generic, rx)
//!     .run_until(async {})
//!     .await?;
//! # Ok(())
//! # }
//! ```

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;

#[cfg(any(feature = "provider-grpc", feature = "provider-websocket"))]
use std::sync::atomic::{AtomicU64, Ordering};

use crate::event::TxCommitmentStatus;
use crate::event::TxKind;
use crate::framework::{
    AccountUpdateEvent, BlockMetaEvent, ClusterTopologyEvent, LeaderScheduleEvent,
    ObservedRecentBlockhashEvent, ReorgEvent, SlotStatusEvent, TransactionEvent,
    TransactionLogEvent, TransactionStatusEvent, TransactionViewBatchEvent,
};
use agave_transaction_view::{
    transaction_data::TransactionData, transaction_view::SanitizedTransactionView,
};
use sof_types::SignatureBytes;
use solana_sdk_ids::{compute_budget, vote};
use solana_transaction::versioned::VersionedTransaction;

/// Default queue capacity for processed provider-stream ingress.
pub const DEFAULT_PROVIDER_STREAM_QUEUE_CAPACITY: usize = 8_192;

/// Identifies the processed provider family driving SOF's direct plugin ingress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderStreamMode {
    /// Generic custom provider-stream ingress supplied by the embedding application.
    ///
    /// This mode is for producers that push typed `ProviderStreamUpdate` items
    /// directly into SOF. A custom adapter converts upstream provider data into
    /// SOF's transaction, control-plane, or health updates.
    ///
    /// Use it when:
    ///
    /// - you have a processed provider that is not one of the built-in adapters
    /// - you need richer control-plane signals than the built-in processed
    ///   providers expose
    /// - you want a bounded replay/batch producer to feed SOF directly
    ///
    /// The runtime dispatch contract is:
    ///
    /// - `Transaction` / `SerializedTransaction` -> transaction-family hooks
    /// - `TransactionLog` -> `on_transaction_log`
    /// - `TransactionStatus` -> `on_transaction_status`
    /// - `TransactionViewBatch` -> `on_transaction_view_batch`
    /// - `AccountUpdate` -> `on_account_update`
    /// - `BlockMeta` -> `on_block_meta`
    /// - `RecentBlockhash` -> `on_recent_blockhash`
    /// - `SlotStatus` -> `on_slot_status`
    /// - `ClusterTopology` -> `on_cluster_topology`
    /// - `LeaderSchedule` -> `on_leader_schedule`
    /// - `Reorg` -> `on_reorg`
    /// - `Health` -> runtime health/readiness only
    Generic,
    /// Yellowstone gRPC / Geyser-style processed transaction feeds.
    YellowstoneGrpc,
    /// Yellowstone gRPC transaction-status feeds.
    YellowstoneGrpcTransactionStatus,
    /// Yellowstone gRPC account-update feeds.
    YellowstoneGrpcAccounts,
    /// Yellowstone gRPC block-meta feeds.
    YellowstoneGrpcBlockMeta,
    /// Yellowstone gRPC slot feeds.
    YellowstoneGrpcSlots,
    /// LaserStream-style processed transaction feeds.
    LaserStream,
    /// LaserStream transaction-status feeds.
    LaserStreamTransactionStatus,
    /// LaserStream account-update feeds.
    LaserStreamAccounts,
    /// LaserStream block-meta feeds.
    LaserStreamBlockMeta,
    /// LaserStream slot feeds.
    LaserStreamSlots,
    #[cfg(feature = "provider-websocket")]
    /// Websocket `transactionSubscribe` processed transaction feeds.
    WebsocketTransaction,
    #[cfg(feature = "provider-websocket")]
    /// Websocket `logsSubscribe` processed log feeds.
    WebsocketLogs,
    #[cfg(feature = "provider-websocket")]
    /// Websocket `accountSubscribe` processed account feeds.
    WebsocketAccount,
    #[cfg(feature = "provider-websocket")]
    /// Websocket `programSubscribe` processed account feeds.
    WebsocketProgram,
}

impl ProviderStreamMode {
    /// Returns the stable string label used in logs and docs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic_provider",
            Self::YellowstoneGrpc => "yellowstone_grpc",
            Self::YellowstoneGrpcTransactionStatus => "yellowstone_grpc_transaction_status",
            Self::YellowstoneGrpcAccounts => "yellowstone_grpc_accounts",
            Self::YellowstoneGrpcBlockMeta => "yellowstone_grpc_block_meta",
            Self::YellowstoneGrpcSlots => "yellowstone_grpc_slots",
            Self::LaserStream => "laserstream",
            Self::LaserStreamTransactionStatus => "laserstream_transaction_status",
            Self::LaserStreamAccounts => "laserstream_accounts",
            Self::LaserStreamBlockMeta => "laserstream_block_meta",
            Self::LaserStreamSlots => "laserstream_slots",
            #[cfg(feature = "provider-websocket")]
            Self::WebsocketTransaction => "websocket_transaction",
            #[cfg(feature = "provider-websocket")]
            Self::WebsocketLogs => "websocket_logs",
            #[cfg(feature = "provider-websocket")]
            Self::WebsocketAccount => "websocket_account",
            #[cfg(feature = "provider-websocket")]
            Self::WebsocketProgram => "websocket_program",
        }
    }
}

/// Replay policy for processed provider transaction streams.
///
/// # Examples
///
/// ```rust
/// use sof::provider_stream::ProviderReplayMode;
///
/// let live = ProviderReplayMode::Live;
/// let resume = ProviderReplayMode::Resume;
/// let from_slot = ProviderReplayMode::FromSlot(1_000_000);
///
/// assert!(matches!(live, ProviderReplayMode::Live));
/// assert!(matches!(resume, ProviderReplayMode::Resume));
/// assert!(matches!(from_slot, ProviderReplayMode::FromSlot(1_000_000)));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProviderReplayMode {
    /// Start from the provider's live edge and do not request historical replay.
    Live,
    /// Start live and resume from the last tracked slot after reconnects.
    #[default]
    Resume,
    /// Request historical replay starting at one explicit slot, then continue
    /// with tracked resume behavior after reconnects.
    FromSlot(u64),
}

/// One processed provider-stream update accepted by SOF.
#[derive(Debug, Clone)]
pub enum ProviderStreamUpdate {
    /// One provider transaction mapped onto SOF's transaction hook surface.
    Transaction(TransactionEvent),
    /// One serialized provider transaction that can still be filtered before full decode.
    SerializedTransaction(SerializedTransactionEvent),
    /// One provider/websocket transaction-log notification.
    TransactionLog(TransactionLogEvent),
    /// One provider transaction-status notification.
    TransactionStatus(TransactionStatusEvent),
    /// One provider transaction-view batch mapped onto SOF's view-batch surface.
    TransactionViewBatch(TransactionViewBatchEvent),
    /// One provider account update.
    AccountUpdate(AccountUpdateEvent),
    /// One provider block-meta update.
    BlockMeta(BlockMetaEvent),
    /// One provider recent-blockhash observation.
    RecentBlockhash(ObservedRecentBlockhashEvent),
    /// One provider slot-status update.
    SlotStatus(SlotStatusEvent),
    /// One provider cluster-topology update.
    ClusterTopology(ClusterTopologyEvent),
    /// One provider leader-schedule update.
    LeaderSchedule(LeaderScheduleEvent),
    /// One provider reorg/fork update.
    Reorg(ReorgEvent),
    /// One provider source health transition observed by a built-in or generic source.
    Health(ProviderSourceHealthEvent),
}

impl ProviderStreamUpdate {
    /// Tags one provider-origin update with the source instance that produced it.
    #[must_use]
    pub fn with_provider_source(mut self, source: ProviderSourceIdentity) -> Self {
        let source = Arc::new(source);
        match &mut self {
            Self::Transaction(event) => event.provider_source = Some(Arc::clone(&source)),
            Self::SerializedTransaction(event) => event.provider_source = Some(Arc::clone(&source)),
            Self::TransactionLog(event) => event.provider_source = Some(Arc::clone(&source)),
            Self::TransactionStatus(event) => event.provider_source = Some(Arc::clone(&source)),
            Self::TransactionViewBatch(event) => event.provider_source = Some(Arc::clone(&source)),
            Self::AccountUpdate(event) => event.provider_source = Some(Arc::clone(&source)),
            Self::BlockMeta(event) => event.provider_source = Some(Arc::clone(&source)),
            Self::RecentBlockhash(event) => event.provider_source = Some(Arc::clone(&source)),
            Self::SlotStatus(event) => event.provider_source = Some(Arc::clone(&source)),
            Self::ClusterTopology(event) => event.provider_source = Some(Arc::clone(&source)),
            Self::LeaderSchedule(event) => event.provider_source = Some(Arc::clone(&source)),
            Self::Reorg(event) => event.provider_source = Some(Arc::clone(&source)),
            Self::Health(event) => event.source = Arc::unwrap_or_clone(source),
        }
        self
    }
}

impl From<TransactionEvent> for ProviderStreamUpdate {
    fn from(event: TransactionEvent) -> Self {
        Self::Transaction(event)
    }
}

impl From<SerializedTransactionEvent> for ProviderStreamUpdate {
    fn from(event: SerializedTransactionEvent) -> Self {
        Self::SerializedTransaction(event)
    }
}

impl From<TransactionLogEvent> for ProviderStreamUpdate {
    fn from(event: TransactionLogEvent) -> Self {
        Self::TransactionLog(event)
    }
}

impl From<TransactionStatusEvent> for ProviderStreamUpdate {
    fn from(event: TransactionStatusEvent) -> Self {
        Self::TransactionStatus(event)
    }
}

impl From<TransactionViewBatchEvent> for ProviderStreamUpdate {
    fn from(event: TransactionViewBatchEvent) -> Self {
        Self::TransactionViewBatch(event)
    }
}

impl From<AccountUpdateEvent> for ProviderStreamUpdate {
    fn from(event: AccountUpdateEvent) -> Self {
        Self::AccountUpdate(event)
    }
}

impl From<BlockMetaEvent> for ProviderStreamUpdate {
    fn from(event: BlockMetaEvent) -> Self {
        Self::BlockMeta(event)
    }
}

impl From<ObservedRecentBlockhashEvent> for ProviderStreamUpdate {
    fn from(event: ObservedRecentBlockhashEvent) -> Self {
        Self::RecentBlockhash(event)
    }
}

impl From<SlotStatusEvent> for ProviderStreamUpdate {
    fn from(event: SlotStatusEvent) -> Self {
        Self::SlotStatus(event)
    }
}

impl From<ClusterTopologyEvent> for ProviderStreamUpdate {
    fn from(event: ClusterTopologyEvent) -> Self {
        Self::ClusterTopology(event)
    }
}

impl From<LeaderScheduleEvent> for ProviderStreamUpdate {
    fn from(event: LeaderScheduleEvent) -> Self {
        Self::LeaderSchedule(event)
    }
}

impl From<ReorgEvent> for ProviderStreamUpdate {
    fn from(event: ReorgEvent) -> Self {
        Self::Reorg(event)
    }
}

impl From<ProviderSourceHealthEvent> for ProviderStreamUpdate {
    fn from(event: ProviderSourceHealthEvent) -> Self {
        Self::Health(event)
    }
}

/// One provider source health transition observed by SOF.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProviderSourceHealthEvent {
    /// Stable source instance identifier, for example one websocket program feed.
    pub source: ProviderSourceIdentity,
    /// Whether this source participates in readiness gating.
    pub readiness: ProviderSourceReadiness,
    /// Current health state for this source.
    pub status: ProviderSourceHealthStatus,
    /// Typed reason for the transition.
    pub reason: ProviderSourceHealthReason,
    /// Human-readable message attached to the transition.
    pub message: String,
}

/// Stable provider source instance identity used in runtime health and provider-origin events.
#[derive(Debug, Clone, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ProviderSourceIdentity {
    /// Stable source kind, for example `WebsocketTransaction`.
    pub kind: ProviderSourceId,
    /// Runtime-unique or user-supplied source instance label.
    pub instance: Arc<str>,
}

impl ProviderSourceIdentity {
    /// Creates one provider source identity from a stable kind and instance label.
    #[must_use]
    pub fn new(kind: ProviderSourceId, instance: impl Into<Arc<str>>) -> Self {
        Self {
            kind,
            instance: instance.into(),
        }
    }

    /// Returns the source kind label, for example `websocket_transaction`.
    #[must_use]
    pub fn kind_str(&self) -> &str {
        self.kind.as_str()
    }

    /// Returns the source instance label.
    #[must_use]
    pub fn instance_str(&self) -> &str {
        self.instance.as_ref()
    }

    /// Builds a generated provider-source identity with a unique instance suffix.
    #[cfg(any(feature = "provider-grpc", feature = "provider-websocket"))]
    #[must_use]
    pub(crate) fn generated(kind: ProviderSourceId, label: Option<&str>) -> Self {
        static NEXT_PROVIDER_SOURCE_INSTANCE: AtomicU64 = AtomicU64::new(1);
        match label {
            Some(label) => Self::new(kind, label),
            None => {
                let instance = NEXT_PROVIDER_SOURCE_INSTANCE.fetch_add(1, Ordering::Relaxed);
                Self::new(kind.clone(), format!("{}-{instance}", kind.as_str()))
            }
        }
    }
}

/// Stable provider source identifier used in runtime health reporting.
#[derive(Debug, Clone, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum ProviderSourceId {
    /// Generic custom provider source label supplied by the embedding app.
    Generic(Arc<str>),
    /// Built-in Yellowstone gRPC source.
    YellowstoneGrpc,
    /// Built-in Yellowstone gRPC transaction-status source.
    YellowstoneGrpcTransactionStatus,
    /// Built-in Yellowstone gRPC account source.
    YellowstoneGrpcAccounts,
    /// Built-in Yellowstone gRPC block-meta source.
    YellowstoneGrpcBlockMeta,
    /// Built-in Yellowstone gRPC slot source.
    YellowstoneGrpcSlots,
    /// Built-in LaserStream source.
    LaserStream,
    /// Built-in LaserStream transaction-status source.
    LaserStreamTransactionStatus,
    /// Built-in LaserStream account source.
    LaserStreamAccounts,
    /// Built-in LaserStream block-meta source.
    LaserStreamBlockMeta,
    /// Built-in LaserStream slot source.
    LaserStreamSlots,
    #[cfg(feature = "provider-websocket")]
    /// Built-in websocket `transactionSubscribe` source.
    WebsocketTransaction,
    #[cfg(feature = "provider-websocket")]
    /// Built-in websocket `logsSubscribe` source.
    WebsocketLogs,
    #[cfg(feature = "provider-websocket")]
    /// Built-in websocket `accountSubscribe` source.
    WebsocketAccount,
    #[cfg(feature = "provider-websocket")]
    /// Built-in websocket `programSubscribe` source.
    WebsocketProgram,
}

impl ProviderSourceId {
    /// Returns the stable string label used in logs and error messages.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Generic(label) => label.as_ref(),
            Self::YellowstoneGrpc => "yellowstone_grpc",
            Self::YellowstoneGrpcTransactionStatus => "yellowstone_grpc_transaction_status",
            Self::YellowstoneGrpcAccounts => "yellowstone_grpc_accounts",
            Self::YellowstoneGrpcBlockMeta => "yellowstone_grpc_block_meta",
            Self::YellowstoneGrpcSlots => "yellowstone_grpc_slots",
            Self::LaserStream => "laserstream",
            Self::LaserStreamTransactionStatus => "laserstream_transaction_status",
            Self::LaserStreamAccounts => "laserstream_accounts",
            Self::LaserStreamBlockMeta => "laserstream_block_meta",
            Self::LaserStreamSlots => "laserstream_slots",
            #[cfg(feature = "provider-websocket")]
            Self::WebsocketTransaction => "websocket_transaction",
            #[cfg(feature = "provider-websocket")]
            Self::WebsocketLogs => "websocket_logs",
            #[cfg(feature = "provider-websocket")]
            Self::WebsocketAccount => "websocket_account",
            #[cfg(feature = "provider-websocket")]
            Self::WebsocketProgram => "websocket_program",
        }
    }
}

/// Health state for one provider source feeding SOF.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ProviderSourceHealthStatus {
    /// Source is healthy and delivering updates.
    Healthy,
    /// Source is reconnecting or recovering after a disruption.
    Reconnecting,
    /// Source exhausted recovery and is no longer healthy.
    Unhealthy,
}

/// Readiness class for one provider source observed by SOF.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub enum ProviderSourceReadiness {
    /// This source is required for the runtime to report ready.
    Required,
    /// This source is advisory or redundant and does not gate readiness.
    Optional,
}

impl ProviderSourceReadiness {
    /// Returns the stable string label used in metrics and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
        }
    }
}

/// Typed reason for one provider source health transition.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub enum ProviderSourceHealthReason {
    /// Source is configured and waiting for its first live session acknowledgement.
    InitialConnectPending,
    /// Subscription/setup completed and the provider acknowledged the stream.
    SubscriptionAckReceived,
    /// The provider stream ended without an explicit terminal error.
    UpstreamStreamClosedUnexpectedly,
    /// Transport-layer failure such as websocket or tonic I/O.
    UpstreamTransportFailure,
    /// Protocol or payload-shape failure.
    UpstreamProtocolFailure,
    /// Replay/backfill recovery failed.
    ReplayBackfillFailure,
    /// Reconnect budget was exhausted.
    ReconnectBudgetExhausted,
}

impl ProviderSourceHealthReason {
    /// Returns the stable string label used in logs and runtime errors.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InitialConnectPending => "initial_connect_pending",
            Self::SubscriptionAckReceived => "subscription_ack_received",
            Self::UpstreamStreamClosedUnexpectedly => "upstream_stream_closed_unexpectedly",
            Self::UpstreamTransportFailure => "upstream_transport_failure",
            Self::UpstreamProtocolFailure => "upstream_protocol_failure",
            Self::ReplayBackfillFailure => "replay_backfill_failure",
            Self::ReconnectBudgetExhausted => "reconnect_budget_exhausted",
        }
    }
}

/// Sender type for processed provider-stream ingress.
pub type ProviderStreamSender = mpsc::Sender<ProviderStreamUpdate>;
/// Receiver type for processed provider-stream ingress.
pub type ProviderStreamReceiver = mpsc::Receiver<ProviderStreamUpdate>;
/// Shared provider source identity carried by provider-origin events.
pub type ProviderSourceRef = Arc<ProviderSourceIdentity>;

/// Helper for feeding one SOF provider queue from multiple provider sources.
#[derive(Clone, Debug)]
pub struct ProviderStreamFanIn {
    /// Shared sender used to fan multiple provider sources into one ingress queue.
    sender: ProviderStreamSender,
}

impl ProviderStreamFanIn {
    /// Returns a cloned sender for one provider source.
    #[must_use]
    pub fn sender(&self) -> ProviderStreamSender {
        self.sender.clone()
    }
}

/// One serialized provider-fed transaction that has not yet been materialized.
#[derive(Debug, Clone)]
pub struct SerializedTransactionEvent {
    /// Slot containing this transaction.
    pub slot: u64,
    /// Commitment status for this transaction slot when event was emitted.
    pub commitment_status: TxCommitmentStatus,
    /// Latest observed confirmed slot watermark when event was emitted.
    pub confirmed_slot: Option<u64>,
    /// Latest observed finalized slot watermark when event was emitted.
    pub finalized_slot: Option<u64>,
    /// Transaction signature if present.
    pub signature: Option<SignatureBytes>,
    /// Provider source instance when this transaction came from provider ingress.
    pub provider_source: Option<ProviderSourceRef>,
    /// Serialized transaction bytes.
    pub bytes: Box<[u8]>,
}

#[cfg(any(feature = "provider-grpc", feature = "provider-websocket"))]
#[derive(Clone, Copy, Debug, Default)]
/// Watermark tracker for provider commitments used by built-in adapters.
pub(crate) struct ProviderCommitmentWatermarks {
    /// Latest confirmed slot watermark observed from the provider.
    pub(crate) confirmed_slot: Option<u64>,
    /// Latest finalized slot watermark observed from the provider.
    pub(crate) finalized_slot: Option<u64>,
}

#[cfg(any(feature = "provider-grpc", feature = "provider-websocket"))]
impl ProviderCommitmentWatermarks {
    /// Updates commitment watermarks based on one transaction slot and commitment.
    #[inline]
    pub(crate) fn observe_transaction_commitment(
        &mut self,
        slot: u64,
        commitment_status: TxCommitmentStatus,
    ) {
        match commitment_status {
            TxCommitmentStatus::Processed => {}
            TxCommitmentStatus::Confirmed => {
                self.confirmed_slot = Some(self.confirmed_slot.unwrap_or(slot).max(slot));
            }
            TxCommitmentStatus::Finalized => {
                self.confirmed_slot = Some(self.confirmed_slot.unwrap_or(slot).max(slot));
                self.finalized_slot = Some(self.finalized_slot.unwrap_or(slot).max(slot));
            }
        }
    }

    /// Advances the confirmed watermark to include one slot.
    #[inline]
    pub(crate) fn observe_confirmed_slot(&mut self, slot: u64) {
        self.confirmed_slot = Some(self.confirmed_slot.unwrap_or(slot).max(slot));
    }

    /// Advances the finalized watermark to include one slot.
    #[inline]
    pub(crate) fn observe_finalized_slot(&mut self, slot: u64) {
        self.observe_confirmed_slot(slot);
        self.finalized_slot = Some(self.finalized_slot.unwrap_or(slot).max(slot));
    }
}

/// Creates one bounded queue for processed provider-stream updates.
///
/// # Examples
///
/// ```rust
/// use sof::provider_stream::{create_provider_stream_queue, DEFAULT_PROVIDER_STREAM_QUEUE_CAPACITY};
///
/// let (_tx, _rx) = create_provider_stream_queue(DEFAULT_PROVIDER_STREAM_QUEUE_CAPACITY);
/// ```
#[must_use]
pub fn create_provider_stream_queue(
    capacity: usize,
) -> (ProviderStreamSender, ProviderStreamReceiver) {
    mpsc::channel(capacity.max(1))
}

/// Creates one bounded queue plus a typed helper for multi-source provider fan-in.
///
/// # Examples
///
/// ```rust
/// use sof::provider_stream::{create_provider_stream_fan_in, ProviderStreamMode};
///
/// let (_fan_in, _rx) = create_provider_stream_fan_in(256);
/// let _mode = ProviderStreamMode::Generic;
/// ```
#[must_use]
pub fn create_provider_stream_fan_in(
    capacity: usize,
) -> (ProviderStreamFanIn, ProviderStreamReceiver) {
    let (sender, receiver) = create_provider_stream_queue(capacity);
    (ProviderStreamFanIn { sender }, receiver)
}

/// Classifies provider-fed transactions consistently across built-in adapters.
pub(crate) fn classify_provider_transaction_kind(tx: &VersionedTransaction) -> TxKind {
    let mut has_vote = false;
    let mut has_non_vote_non_budget = false;
    let keys = tx.message.static_account_keys();
    for instruction in tx.message.instructions() {
        if let Some(program_id) = keys.get(usize::from(instruction.program_id_index)) {
            if *program_id == vote::id() {
                has_vote = true;
                if has_non_vote_non_budget {
                    return TxKind::Mixed;
                }
                continue;
            }
            if *program_id != compute_budget::id() {
                has_non_vote_non_budget = true;
                if has_vote {
                    return TxKind::Mixed;
                }
            }
        }
    }
    if has_vote && !has_non_vote_non_budget {
        TxKind::VoteOnly
    } else if has_vote {
        TxKind::Mixed
    } else {
        TxKind::NonVote
    }
}

#[cfg_attr(not(test), allow(dead_code))]
/// Classifies provider-fed transaction views consistently across built-in adapters.
pub(crate) fn classify_provider_transaction_kind_view<D: TransactionData>(
    view: &SanitizedTransactionView<D>,
) -> TxKind {
    let mut has_vote = false;
    let mut has_non_vote_non_budget = false;
    for (program_id, _) in view.program_instructions_iter() {
        if *program_id == vote::id() {
            has_vote = true;
            if has_non_vote_non_budget {
                return TxKind::Mixed;
            }
            continue;
        }
        if *program_id != compute_budget::id() {
            has_non_vote_non_budget = true;
            if has_vote {
                return TxKind::Mixed;
            }
        }
    }
    if has_vote && !has_non_vote_non_budget {
        TxKind::VoteOnly
    } else if has_vote {
        TxKind::Mixed
    } else {
        TxKind::NonVote
    }
}

#[cfg(feature = "provider-grpc")]
/// Yellowstone gRPC adapter helpers.
pub mod yellowstone;

#[cfg(feature = "provider-grpc")]
/// Helius LaserStream adapter helpers.
pub mod laserstream;

#[cfg(feature = "provider-websocket")]
/// Websocket `transactionSubscribe` adapter helpers.
pub mod websocket;

#[cfg(all(test, any(feature = "provider-grpc", feature = "provider-websocket")))]
mod tests {
    use super::*;
    use solana_instruction::Instruction;
    use solana_keypair::Keypair;
    use solana_message::{Message, VersionedMessage};
    use solana_sdk_ids::system_program;
    use solana_signer::Signer;
    use std::time::Instant;

    fn profile_iterations(default: usize) -> usize {
        std::env::var("SOF_PROFILE_ITERATIONS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(default)
    }

    fn sample_mixed_transaction() -> VersionedTransaction {
        let signer = Keypair::new();
        let mut instructions = Vec::with_capacity(34);
        instructions.push(Instruction::new_with_bytes(vote::id(), &[], vec![]));
        instructions.push(Instruction::new_with_bytes(
            system_program::id(),
            &[],
            vec![],
        ));
        for _ in 0..32 {
            instructions.push(Instruction::new_with_bytes(
                compute_budget::id(),
                &[],
                vec![],
            ));
        }
        let message = Message::new(&instructions, Some(&signer.pubkey()));
        VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[&signer]).expect("tx")
    }

    fn classify_provider_transaction_kind_baseline(tx: &VersionedTransaction) -> TxKind {
        let mut has_vote = false;
        let mut has_non_vote_non_budget = false;
        let keys = tx.message.static_account_keys();
        for instruction in tx.message.instructions() {
            if let Some(program_id) = keys.get(usize::from(instruction.program_id_index)) {
                if *program_id == vote::id() {
                    has_vote = true;
                    continue;
                }
                if *program_id != compute_budget::id() {
                    has_non_vote_non_budget = true;
                }
            }
        }
        if has_vote && !has_non_vote_non_budget {
            TxKind::VoteOnly
        } else if has_vote {
            TxKind::Mixed
        } else {
            TxKind::NonVote
        }
    }

    #[test]
    fn classify_provider_transaction_kind_detects_mixed() {
        let tx = sample_mixed_transaction();
        assert_eq!(classify_provider_transaction_kind(&tx), TxKind::Mixed);
    }

    #[test]
    #[ignore = "profiling fixture for provider transaction kind classification A/B"]
    fn provider_transaction_kind_profile_fixture() {
        let iterations = profile_iterations(1_000_000);

        let tx = sample_mixed_transaction();

        let baseline_started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(classify_provider_transaction_kind_baseline(&tx));
        }
        let baseline_elapsed = baseline_started.elapsed();

        let optimized_started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(classify_provider_transaction_kind(&tx));
        }
        let optimized_elapsed = optimized_started.elapsed();

        eprintln!(
            "provider_transaction_kind_profile_fixture iterations={} baseline_us={} optimized_us={}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
        );
    }

    #[test]
    #[ignore = "profiling fixture for baseline provider tx kind classification"]
    fn provider_transaction_kind_baseline_profile_fixture() {
        let iterations = profile_iterations(1_000_000);

        let tx = sample_mixed_transaction();
        for _ in 0..iterations {
            std::hint::black_box(classify_provider_transaction_kind_baseline(&tx));
        }
    }

    #[test]
    #[ignore = "profiling fixture for optimized provider tx kind classification"]
    fn provider_transaction_kind_optimized_profile_fixture() {
        let iterations = profile_iterations(1_000_000);

        let tx = sample_mixed_transaction();
        for _ in 0..iterations {
            std::hint::black_box(classify_provider_transaction_kind(&tx));
        }
    }
}
