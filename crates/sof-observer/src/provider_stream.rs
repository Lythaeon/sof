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
//! - an operational role via `with_source_role(...)`
//! - an explicit arbitration priority via `with_source_priority(...)`
//! - duplicate handling via `with_source_arbitration(...)`
//!   - `EmitAll` keeps the historical behavior and forwards overlapping
//!     provider events from every source
//!   - `FirstSeen` suppresses later overlapping duplicates across sources
//!   - `FirstSeenThenPromote` keeps the first event immediate, but allows one
//!     later higher-priority duplicate to promote through
//!
//! Generic multi-source producers can reserve one stable source identity with
//! [`ProviderStreamFanIn::sender_for_source`]. The returned
//! [`ReservedProviderStreamSender`] automatically attributes every update it
//! sends to that reserved provider source.
//! Generic producers can set the same source policy directly on
//! [`ProviderSourceIdentity`] with `with_role(...)`, `with_priority(...)`, and
//! `with_arbitration(...)`.
//!
//! Generic readiness becomes source-aware only after a custom producer emits
//! [`ProviderStreamUpdate::Health`] for that reserved source. Until then,
//! `ProviderStreamMode::Generic` falls back to progress-based readiness and
//! only knows that typed updates are flowing at all.
//!
//! Fan-in duplicate arbitration is source-aware and keyed by the logical event,
//! not just by queue position. That means two sources carrying the same
//! transaction or control-plane item can now either:
//!
//! - both dispatch (`EmitAll`, the default)
//! - dispatch once (`FirstSeen`)
//! - dispatch once immediately and then allow one later higher-priority source
//!   to promote (`FirstSeenThenPromote`)
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
//!     framework::{SignatureBytes, TransactionEvent},
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
//!     signature: transaction
//!         .signatures
//!         .first()
//!         .copied()
//!         .map(SignatureBytes::from_solana),
//!     tx: Arc::new(transaction),
//!     kind: TxKind::NonVote,
//!     provider_source: None,
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
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::SendError;

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

    /// Tags one provider-origin update with one shared source reference.
    #[must_use]
    #[cfg_attr(not(feature = "provider-grpc"), allow(dead_code))]
    pub(crate) fn with_provider_source_ref(mut self, source: &Arc<ProviderSourceIdentity>) -> Self {
        match &mut self {
            Self::Transaction(event) => event.provider_source = Some(Arc::clone(source)),
            Self::SerializedTransaction(event) => event.provider_source = Some(Arc::clone(source)),
            Self::TransactionLog(event) => event.provider_source = Some(Arc::clone(source)),
            Self::TransactionStatus(event) => event.provider_source = Some(Arc::clone(source)),
            Self::TransactionViewBatch(event) => event.provider_source = Some(Arc::clone(source)),
            Self::AccountUpdate(event) => event.provider_source = Some(Arc::clone(source)),
            Self::BlockMeta(event) => event.provider_source = Some(Arc::clone(source)),
            Self::RecentBlockhash(event) => event.provider_source = Some(Arc::clone(source)),
            Self::SlotStatus(event) => event.provider_source = Some(Arc::clone(source)),
            Self::ClusterTopology(event) => event.provider_source = Some(Arc::clone(source)),
            Self::LeaderSchedule(event) => event.provider_source = Some(Arc::clone(source)),
            Self::Reorg(event) => event.provider_source = Some(Arc::clone(source)),
            Self::Health(event) => event.source = (**source).clone(),
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSourceIdentity {
    /// Stable source kind, for example `WebsocketTransaction`.
    pub kind: ProviderSourceId,
    /// Runtime-unique or user-supplied source instance label.
    pub instance: Arc<str>,
    /// Relative arbitration priority for this source.
    pub priority: u16,
    /// Operational role for this source within one multi-source fan-in.
    pub role: ProviderSourceRole,
    /// Duplicate arbitration policy for this source.
    pub arbitration: ProviderSourceArbitrationMode,
}

impl ProviderSourceIdentity {
    /// Creates one provider source identity from a stable kind and instance label.
    #[must_use]
    pub fn new(kind: ProviderSourceId, instance: impl Into<Arc<str>>) -> Self {
        Self {
            kind,
            instance: instance.into(),
            priority: ProviderSourceRole::Primary.default_priority(),
            role: ProviderSourceRole::Primary,
            arbitration: ProviderSourceArbitrationMode::EmitAll,
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

    /// Returns the relative arbitration priority for this source.
    #[must_use]
    pub const fn priority(&self) -> u16 {
        self.priority
    }

    /// Returns the configured source role.
    #[must_use]
    pub const fn role(&self) -> ProviderSourceRole {
        self.role
    }

    /// Returns the configured duplicate arbitration policy.
    #[must_use]
    pub const fn arbitration(&self) -> ProviderSourceArbitrationMode {
        self.arbitration
    }

    /// Sets one explicit arbitration priority.
    #[must_use]
    pub const fn with_priority(mut self, priority: u16) -> Self {
        self.priority = priority;
        self
    }

    /// Sets one source role and updates the default priority when the source still uses
    /// that role's previous default.
    #[must_use]
    pub const fn with_role(mut self, role: ProviderSourceRole) -> Self {
        if self.priority == self.role.default_priority() {
            self.priority = role.default_priority();
        }
        self.role = role;
        self
    }

    /// Sets one duplicate arbitration policy.
    #[must_use]
    pub const fn with_arbitration(mut self, arbitration: ProviderSourceArbitrationMode) -> Self {
        self.arbitration = arbitration;
        self
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

impl std::fmt::Display for ProviderSourceIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.kind.as_str(), self.instance)
    }
}

impl PartialEq for ProviderSourceIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.instance == other.instance
    }
}

impl Eq for ProviderSourceIdentity {}

impl Hash for ProviderSourceIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.kind.hash(state);
        self.instance.hash(state);
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

/// Relative operational role for one provider source inside a fan-in graph.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum ProviderSourceRole {
    /// Lowest-latency or best-effort primary feed.
    Primary,
    /// Secondary feed expected to be healthy and often overlap the primary.
    Secondary,
    /// Lower-priority fallback feed.
    Fallback,
    /// Confirmation-only feed used mainly for richer or more trusted overlap.
    ConfirmOnly,
}

impl ProviderSourceRole {
    /// Returns the stable string label used in logs and docs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Secondary => "secondary",
            Self::Fallback => "fallback",
            Self::ConfirmOnly => "confirm_only",
        }
    }

    /// Returns the default arbitration priority for this role.
    #[must_use]
    pub const fn default_priority(self) -> u16 {
        match self {
            Self::Primary => 300,
            Self::Secondary => 200,
            Self::Fallback => 100,
            Self::ConfirmOnly => 400,
        }
    }
}

/// Duplicate arbitration mode for one provider source inside fan-in.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum ProviderSourceArbitrationMode {
    /// Keep the current SOF behavior and emit overlapping duplicates from every source.
    EmitAll,
    /// First source wins for one logical event key; later duplicates are dropped.
    FirstSeen,
    /// First source wins immediately, but a later higher-priority duplicate may promote and emit.
    FirstSeenThenPromote,
}

impl ProviderSourceArbitrationMode {
    /// Returns the stable string label used in logs and docs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EmitAll => "emit_all",
            Self::FirstSeen => "first_seen",
            Self::FirstSeenThenPromote => "first_seen_then_promote",
        }
    }
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
    /// Source registration was withdrawn and should be removed from tracking.
    ///
    /// This is a lifecycle control event, not a persistent steady-state health
    /// value. Runtime health and observability prune removed sources instead of
    /// surfacing them as active tracked sources.
    Removed,
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
    /// Source task stopped and its registration was pruned from tracking.
    SourceRemoved,
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
            Self::SourceRemoved => "source_removed",
        }
    }
}

/// Sender type for processed provider-stream ingress.
pub type ProviderStreamSender = mpsc::Sender<ProviderStreamUpdate>;
/// Receiver type for processed provider-stream ingress.
pub type ProviderStreamReceiver = mpsc::Receiver<ProviderStreamUpdate>;
/// Shared provider source identity carried by provider-origin events.
pub type ProviderSourceRef = Arc<ProviderSourceIdentity>;

/// Duplicate provider source identity registration failure for multi-source fan-in.
#[derive(Debug, Error)]
#[error("provider fan-in already contains source identity {0}")]
pub struct ProviderSourceIdentityRegistrationError(pub ProviderSourceIdentity);

/// One reserved source identity held for the lifetime of one producer.
#[derive(Debug)]
pub(crate) struct ProviderSourceReservation {
    /// Fan-in that owns the reserved identity set.
    fan_in: ProviderStreamFanIn,
    /// Stable source identity held until the reservation is dropped.
    source: ProviderSourceIdentity,
    /// Sender used to prune generic source health when the reservation drops.
    removal_sender: Option<ProviderStreamSender>,
}

/// Deferred source-identity release that runs only after a terminal removal event is queued.
#[derive(Debug)]
struct ProviderSourceDeferredRelease {
    /// Fan-in that owns the reserved identity set.
    fan_in: ProviderStreamFanIn,
    /// Stable source identity released when the guard drops.
    source: ProviderSourceIdentity,
}

impl Drop for ProviderSourceDeferredRelease {
    fn drop(&mut self) {
        self.fan_in.release_source_identity(&self.source);
    }
}

/// One owned release guard kept alive until terminal source cleanup completes.
#[derive(Debug, Default)]
struct ProviderSourceReleaseGuard {
    /// Release one raw reserved identity directly.
    _identity: Option<ProviderSourceDeferredRelease>,
    /// Keep one built-in reservation alive until the delayed removal send completes.
    _reservation: Option<Arc<ProviderSourceReservation>>,
}

impl ProviderSourceReleaseGuard {
    /// Creates one guard that releases a reserved identity directly on drop.
    const fn for_identity(release: ProviderSourceDeferredRelease) -> Self {
        Self {
            _identity: Some(release),
            _reservation: None,
        }
    }

    #[cfg(any(feature = "provider-grpc", feature = "provider-websocket"))]
    /// Creates one guard that keeps a built-in reservation alive until drop.
    const fn for_reservation(reservation: Arc<ProviderSourceReservation>) -> Self {
        Self {
            _identity: None,
            _reservation: Some(reservation),
        }
    }
}

/// Maximum no-runtime retries for one terminal `Removed` event before releasing anyway.
const SOURCE_REMOVED_NO_RUNTIME_RETRY_ATTEMPTS: usize = 64;
/// Delay between no-runtime retries for one terminal `Removed` event.
const SOURCE_REMOVED_NO_RUNTIME_RETRY_DELAY: Duration = Duration::from_millis(5);

impl Drop for ProviderSourceReservation {
    fn drop(&mut self) {
        let release = ProviderSourceReleaseGuard::for_identity(ProviderSourceDeferredRelease {
            fan_in: self.fan_in.clone(),
            source: self.source.clone(),
        });
        if let Some(sender) = self.removal_sender.clone() {
            drop(emit_provider_source_removed(
                &sender,
                self.source.clone(),
                ProviderSourceReadiness::Optional,
                "reserved provider source sender dropped and was removed from tracking".to_owned(),
                Some(release),
            ));
        } else {
            drop(release);
        }
    }
}

/// One reserved provider source identity plus a sender bound to that reservation.
#[derive(Clone, Debug)]
pub struct ReservedProviderStreamSender {
    /// Sender bound to one reserved source identity.
    sender: ProviderStreamSender,
    /// Reservation released automatically when the last handle is dropped.
    reservation: Arc<ProviderSourceReservation>,
}

impl ReservedProviderStreamSender {
    /// Returns the reserved provider source identity for this sender.
    #[must_use]
    pub fn source(&self) -> &ProviderSourceIdentity {
        &self.reservation.source
    }

    /// Binds one outgoing update to this sender's reserved provider source.
    fn bind_update(&self, update: ProviderStreamUpdate) -> ProviderStreamUpdate {
        update.with_provider_source(self.reservation.source.clone())
    }

    /// Sends one provider update attributed to this reserved source identity.
    ///
    /// Any provider source already present on `update` is replaced with this
    /// sender's reserved source identity.
    ///
    /// # Errors
    ///
    /// Returns the underlying queue send error when the provider ingress queue
    /// is closed.
    pub async fn send(
        &self,
        update: ProviderStreamUpdate,
    ) -> Result<(), SendError<ProviderStreamUpdate>> {
        if matches!(
            update,
            ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
                status: ProviderSourceHealthStatus::Removed,
                ..
            })
        ) {
            return Err(SendError(self.bind_update(update)));
        }
        self.sender.send(self.bind_update(update)).await
    }
}

/// One running provider task that should prune its source registration when it stops.
#[cfg(any(feature = "provider-grpc", feature = "provider-websocket"))]
#[derive(Debug)]
pub(crate) struct ProviderSourceTaskGuard {
    /// Sender used to publish the terminal removal event.
    sender: ProviderStreamSender,
    /// Source identity removed when the task stops.
    source: ProviderSourceIdentity,
    /// Readiness class preserved on the terminal removal event.
    readiness: ProviderSourceReadiness,
    /// Reservation kept alive until the task stops.
    reservation: Option<Arc<ProviderSourceReservation>>,
}

#[cfg(any(feature = "provider-grpc", feature = "provider-websocket"))]
impl ProviderSourceTaskGuard {
    /// Creates one guard for a running provider source task.
    #[must_use]
    pub(crate) const fn new(
        sender: ProviderStreamSender,
        source: ProviderSourceIdentity,
        readiness: ProviderSourceReadiness,
        reservation: Option<Arc<ProviderSourceReservation>>,
    ) -> Self {
        Self {
            sender,
            source,
            readiness,
            reservation,
        }
    }
}

#[cfg(any(feature = "provider-grpc", feature = "provider-websocket"))]
impl Drop for ProviderSourceTaskGuard {
    fn drop(&mut self) {
        drop(emit_provider_source_removed(
            &self.sender,
            self.source.clone(),
            self.readiness,
            "provider source task stopped and was removed from tracking".to_owned(),
            self.reservation
                .take()
                .map(ProviderSourceReleaseGuard::for_reservation),
        ));
    }
}

/// Helper for feeding one SOF provider queue from multiple provider sources.
#[derive(Clone, Debug)]
pub struct ProviderStreamFanIn {
    /// Shared sender used to fan multiple provider sources into one ingress queue.
    sender: ProviderStreamSender,
    /// Registered stable source identities reserved for this fan-in.
    identities: Arc<Mutex<HashSet<ProviderSourceIdentity>>>,
}

impl ProviderStreamFanIn {
    /// Reserves one stable source identity and returns a sender bound to that reservation.
    ///
    /// # Errors
    ///
    /// Returns an error when the same full source identity was already reserved
    /// for this fan-in.
    pub fn sender_for_source(
        &self,
        source: ProviderSourceIdentity,
    ) -> Result<ReservedProviderStreamSender, ProviderSourceIdentityRegistrationError> {
        let reservation = self.reserve_source_identity_generic(source)?;
        Ok(ReservedProviderStreamSender {
            sender: self.sender.clone(),
            reservation: Arc::new(reservation),
        })
    }

    /// Returns a cloned sender for built-in helper methods after identity checks.
    #[cfg(any(feature = "provider-grpc", feature = "provider-websocket"))]
    #[must_use]
    pub(crate) fn sender(&self) -> ProviderStreamSender {
        self.sender.clone()
    }

    /// Reserves one stable source identity for this fan-in.
    pub(crate) fn reserve_source_identity(
        &self,
        source: ProviderSourceIdentity,
    ) -> Result<ProviderSourceReservation, ProviderSourceIdentityRegistrationError> {
        let Ok(mut identities) = self.identities.lock() else {
            return Err(ProviderSourceIdentityRegistrationError(source));
        };
        if !identities.insert(source.clone()) {
            return Err(ProviderSourceIdentityRegistrationError(source));
        }
        Ok(ProviderSourceReservation {
            fan_in: self.clone(),
            source,
            removal_sender: None,
        })
    }

    /// Reserves one stable source identity for a generic producer and prunes it on drop.
    fn reserve_source_identity_generic(
        &self,
        source: ProviderSourceIdentity,
    ) -> Result<ProviderSourceReservation, ProviderSourceIdentityRegistrationError> {
        let mut reservation = self.reserve_source_identity(source)?;
        reservation.removal_sender = Some(self.sender.clone());
        Ok(reservation)
    }

    /// Releases one previously reserved stable source identity.
    pub(crate) fn release_source_identity(&self, source: &ProviderSourceIdentity) {
        if let Ok(mut identities) = self.identities.lock() {
            identities.remove(source);
        }
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
    (
        ProviderStreamFanIn {
            sender,
            identities: Arc::new(Mutex::new(HashSet::new())),
        },
        receiver,
    )
}

/// Classifies provider-fed transactions consistently across built-in adapters.
pub(crate) fn classify_provider_transaction_kind(tx: &VersionedTransaction) -> TxKind {
    let mut has_vote = false;
    let mut has_non_vote_non_budget = false;
    let keys = tx.message.static_account_keys();
    let vote_program = vote::id();
    let compute_budget_program = compute_budget::id();
    for instruction in tx.message.instructions() {
        if let Some(program_id) = keys.get(usize::from(instruction.program_id_index)) {
            if *program_id == vote_program {
                has_vote = true;
                if has_non_vote_non_budget {
                    return TxKind::Mixed;
                }
                continue;
            }
            if *program_id != compute_budget_program {
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

/// Classifies provider-fed transaction views consistently across built-in adapters.
pub(crate) fn classify_provider_transaction_kind_view<D: TransactionData>(
    view: &SanitizedTransactionView<D>,
) -> TxKind {
    let mut has_vote = false;
    let mut has_non_vote_non_budget = false;
    let vote_program = vote::id();
    let compute_budget_program = compute_budget::id();
    for (program_id, _) in view.program_instructions_iter() {
        if *program_id == vote_program {
            has_vote = true;
            if has_non_vote_non_budget {
                return TxKind::Mixed;
            }
            continue;
        }
        if *program_id != compute_budget_program {
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

/// Emits one terminal provider-source removal event and prunes runtime tracking.
fn emit_provider_source_removed(
    sender: &ProviderStreamSender,
    source: ProviderSourceIdentity,
    readiness: ProviderSourceReadiness,
    message: String,
    release: Option<ProviderSourceReleaseGuard>,
) -> Option<ProviderSourceReleaseGuard> {
    let event = ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
        source,
        readiness,
        status: ProviderSourceHealthStatus::Removed,
        reason: ProviderSourceHealthReason::SourceRemoved,
        message,
    });
    match sender.try_send(event) {
        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => release,
        Err(mpsc::error::TrySendError::Full(update)) => {
            let sender = sender.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    drop(sender.send(update).await);
                    drop(release);
                });
                None
            } else {
                std::thread::spawn(move || {
                    let mut pending_update = update;
                    for _ in 0..SOURCE_REMOVED_NO_RUNTIME_RETRY_ATTEMPTS {
                        match sender.try_send(pending_update) {
                            Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {
                                drop(release);
                                return;
                            }
                            Err(mpsc::error::TrySendError::Full(retried_update)) => {
                                pending_update = retried_update;
                                std::thread::sleep(SOURCE_REMOVED_NO_RUNTIME_RETRY_DELAY);
                            }
                        }
                    }
                    drop(release);
                });
                None
            }
        }
    }
}

#[cfg(any(feature = "provider-grpc", feature = "provider-websocket"))]
/// Emits one terminal provider-source removal event for startup failure or early cleanup.
pub(crate) fn emit_provider_source_removed_with_reservation(
    sender: &ProviderStreamSender,
    source: ProviderSourceIdentity,
    readiness: ProviderSourceReadiness,
    message: String,
    reservation: Option<Arc<ProviderSourceReservation>>,
) {
    drop(emit_provider_source_removed(
        sender,
        source,
        readiness,
        message,
        reservation.map(ProviderSourceReleaseGuard::for_reservation),
    ));
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
    use sof_support::bench::{avg_ns_per_iteration, profile_iterations};
    use solana_instruction::Instruction;
    use solana_keypair::Keypair;
    use solana_message::{Message, VersionedMessage};
    use solana_sdk_ids::system_program;
    use solana_signer::Signer;
    use std::{sync::Arc, time::Instant};
    use tokio::runtime::Runtime;
    use tokio::time::{Duration, sleep, timeout};

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

    fn sample_recent_blockhash_update() -> ProviderStreamUpdate {
        ProviderStreamUpdate::RecentBlockhash(ObservedRecentBlockhashEvent {
            slot: 7,
            recent_blockhash: solana_hash::Hash::new_unique().to_bytes(),
            dataset_tx_count: 0,
            provider_source: None,
        })
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

    fn classify_provider_transaction_kind_pre_hoist(tx: &VersionedTransaction) -> TxKind {
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

    #[test]
    fn classify_provider_transaction_kind_detects_mixed() {
        let tx = sample_mixed_transaction();
        assert_eq!(classify_provider_transaction_kind(&tx), TxKind::Mixed);
    }

    #[test]
    fn reserved_provider_sender_binds_reserved_source_identity() {
        let (fan_in, mut rx) = create_provider_stream_fan_in(4);
        let sender = fan_in
            .sender_for_source(ProviderSourceIdentity::new(
                ProviderSourceId::Generic(Arc::<str>::from("custom")),
                "source-a",
            ))
            .expect("reserve source");
        let other_source = ProviderSourceIdentity::new(
            ProviderSourceId::Generic(Arc::<str>::from("other")),
            "source-b",
        );

        Runtime::new().expect("runtime").block_on(async move {
            sender
                .send(ProviderStreamUpdate::RecentBlockhash(
                    ObservedRecentBlockhashEvent {
                        slot: 7,
                        recent_blockhash: solana_hash::Hash::new_unique().to_bytes(),
                        dataset_tx_count: 0,
                        provider_source: Some(Arc::new(other_source)),
                    },
                ))
                .await
                .expect("send");

            let update = rx.recv().await.expect("provider update");
            let ProviderStreamUpdate::RecentBlockhash(event) = update else {
                panic!("expected recent blockhash update");
            };
            let source = event.provider_source.expect("bound provider source");
            assert_eq!(source.kind_str(), "custom");
            assert_eq!(source.instance_str(), "source-a");
        });
    }

    #[test]
    fn reserved_provider_sender_emits_removed_health_on_drop() {
        let (fan_in, mut rx) = create_provider_stream_fan_in(4);
        let source = ProviderSourceIdentity::new(
            ProviderSourceId::Generic(Arc::<str>::from("custom")),
            "source-a",
        );
        let sender = fan_in
            .sender_for_source(source.clone())
            .expect("reserve source");

        Runtime::new().expect("runtime").block_on(async move {
            sender
                .send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
                    source: source.clone(),
                    readiness: ProviderSourceReadiness::Required,
                    status: ProviderSourceHealthStatus::Healthy,
                    reason: ProviderSourceHealthReason::SubscriptionAckReceived,
                    message: "source subscription acknowledged".to_owned(),
                }))
                .await
                .expect("send health");
            drop(sender);

            let first = rx.recv().await.expect("health update");
            let ProviderStreamUpdate::Health(first) = first else {
                panic!("expected first health update");
            };
            assert_eq!(first.source, source);
            assert_eq!(first.status, ProviderSourceHealthStatus::Healthy);

            let second = rx.recv().await.expect("removed update");
            let ProviderStreamUpdate::Health(second) = second else {
                panic!("expected removed health update");
            };
            assert_eq!(second.source, source);
            assert_eq!(second.status, ProviderSourceHealthStatus::Removed);
            assert_eq!(second.reason, ProviderSourceHealthReason::SourceRemoved);
        });
    }

    #[test]
    fn reserved_provider_sender_defers_identity_reuse_until_removed_is_enqueued() {
        let (fan_in, mut rx) = create_provider_stream_fan_in(1);
        let source = ProviderSourceIdentity::new(
            ProviderSourceId::Generic(Arc::<str>::from("custom")),
            "source-a",
        );
        let sender = fan_in
            .sender_for_source(source.clone())
            .expect("reserve source");

        Runtime::new().expect("runtime").block_on(async move {
            sender
                .send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
                    source: source.clone(),
                    readiness: ProviderSourceReadiness::Required,
                    status: ProviderSourceHealthStatus::Healthy,
                    reason: ProviderSourceHealthReason::SubscriptionAckReceived,
                    message: "source subscription acknowledged".to_owned(),
                }))
                .await
                .expect("send health");
            drop(sender);

            assert!(
                fan_in.sender_for_source(source.clone()).is_err(),
                "identity should stay reserved until removed is queued"
            );

            let first = rx.recv().await.expect("health update");
            let ProviderStreamUpdate::Health(first) = first else {
                panic!("expected first health update");
            };
            assert_eq!(first.status, ProviderSourceHealthStatus::Healthy);

            let second = timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("removed update should arrive")
                .expect("removed health update");
            let ProviderStreamUpdate::Health(second) = second else {
                panic!("expected removed health update");
            };
            assert_eq!(second.status, ProviderSourceHealthStatus::Removed);
            assert_eq!(second.reason, ProviderSourceHealthReason::SourceRemoved);

            timeout(Duration::from_secs(1), async {
                loop {
                    if fan_in.sender_for_source(source.clone()).is_ok() {
                        break;
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("identity released after removed is enqueued");
        });
    }

    #[test]
    fn reserved_provider_sender_drop_outside_runtime_does_not_panic_when_queue_is_full() {
        let (fan_in, mut rx) = create_provider_stream_fan_in(1);
        let source = ProviderSourceIdentity::new(
            ProviderSourceId::Generic(Arc::<str>::from("custom")),
            "source-a",
        );
        let sender = fan_in
            .sender_for_source(source.clone())
            .expect("reserve source");

        Runtime::new().expect("runtime").block_on(async {
            sender
                .send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
                    source: source.clone(),
                    readiness: ProviderSourceReadiness::Required,
                    status: ProviderSourceHealthStatus::Healthy,
                    reason: ProviderSourceHealthReason::SubscriptionAckReceived,
                    message: "source subscription acknowledged".to_owned(),
                }))
                .await
                .expect("send health");
        });

        let mut sender = Some(sender);
        let drop_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(sender.take().expect("reserved sender"));
        }));
        assert!(
            drop_result.is_ok(),
            "dropping a reserved generic sender outside a tokio runtime should not panic"
        );

        Runtime::new().expect("runtime").block_on(async move {
            let first = rx.recv().await.expect("health update");
            let ProviderStreamUpdate::Health(first) = first else {
                panic!("expected first health update");
            };
            assert_eq!(first.status, ProviderSourceHealthStatus::Healthy);

            let second = timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("removed update should arrive")
                .expect("removed health update");
            let ProviderStreamUpdate::Health(second) = second else {
                panic!("expected removed health update");
            };
            assert_eq!(second.status, ProviderSourceHealthStatus::Removed);
            assert_eq!(second.reason, ProviderSourceHealthReason::SourceRemoved);

            timeout(Duration::from_secs(1), async {
                loop {
                    if fan_in.sender_for_source(source.clone()).is_ok() {
                        break;
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("identity released after background removal send");
        });
    }

    #[test]
    fn reserved_provider_sender_rejects_removed_health_while_alive() {
        let (fan_in, mut rx) = create_provider_stream_fan_in(4);
        let source = ProviderSourceIdentity::new(
            ProviderSourceId::Generic(Arc::<str>::from("custom")),
            "source-a",
        );
        let sender = fan_in
            .sender_for_source(source.clone())
            .expect("reserve source");

        Runtime::new().expect("runtime").block_on(async move {
            let error = sender
                .send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
                    source: source.clone(),
                    readiness: ProviderSourceReadiness::Required,
                    status: ProviderSourceHealthStatus::Removed,
                    reason: ProviderSourceHealthReason::SourceRemoved,
                    message: "should be rejected".to_owned(),
                }))
                .await
                .expect_err("reserved sender should reject removed health while alive");
            let removed_update = error.0;
            let ProviderStreamUpdate::Health(removed) = removed_update else {
                panic!("expected removed health update in send error");
            };
            assert_eq!(removed.source, source);
            assert_eq!(removed.status, ProviderSourceHealthStatus::Removed);

            assert!(
                timeout(Duration::from_millis(50), rx.recv()).await.is_err(),
                "rejected removed update must not reach the provider queue"
            );
        });
    }

    #[test]
    fn reserved_provider_sender_releases_identity_after_bounded_no_runtime_cleanup_retry() {
        let (fan_in, _rx) = create_provider_stream_fan_in(1);
        let source = ProviderSourceIdentity::new(
            ProviderSourceId::Generic(Arc::<str>::from("custom")),
            "source-a",
        );
        let sender = fan_in
            .sender_for_source(source.clone())
            .expect("reserve source");

        fan_in
            .sender()
            .try_send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
                source: ProviderSourceIdentity::new(
                    ProviderSourceId::Generic(Arc::<str>::from("other")),
                    "other-source",
                ),
                readiness: ProviderSourceReadiness::Optional,
                status: ProviderSourceHealthStatus::Healthy,
                reason: ProviderSourceHealthReason::SubscriptionAckReceived,
                message: "occupied".to_owned(),
            }))
            .expect("fill provider queue");

        let drop_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(sender);
        }));
        assert!(drop_result.is_ok(), "drop outside runtime should not panic");

        std::thread::sleep(Duration::from_millis(
            (SOURCE_REMOVED_NO_RUNTIME_RETRY_ATTEMPTS as u64 * 5) + 100,
        ));

        fan_in
            .sender_for_source(source)
            .expect("identity released after bounded no-runtime cleanup retry");
    }

    #[test]
    fn provider_source_task_guard_emits_removed_health_on_drop() {
        let (tx, mut rx) = create_provider_stream_queue(4);
        let source = ProviderSourceIdentity::new(
            ProviderSourceId::Generic(Arc::<str>::from("custom")),
            "source-a",
        );

        {
            let _guard = ProviderSourceTaskGuard::new(
                tx,
                source.clone(),
                ProviderSourceReadiness::Required,
                None,
            );
        }

        Runtime::new().expect("runtime").block_on(async move {
            let update = rx.recv().await.expect("provider update");
            let ProviderStreamUpdate::Health(event) = update else {
                panic!("expected health update");
            };
            assert_eq!(event.source, source);
            assert_eq!(event.status, ProviderSourceHealthStatus::Removed);
            assert_eq!(event.reason, ProviderSourceHealthReason::SourceRemoved);
        });
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
    #[ignore = "profiling fixture for provider tx kind hoisted id comparison"]
    fn provider_transaction_kind_hoist_ids_profile_fixture() {
        let iterations = profile_iterations(1_000_000);

        let tx = sample_mixed_transaction();

        let baseline_started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(classify_provider_transaction_kind_pre_hoist(&tx));
        }
        let baseline_elapsed = baseline_started.elapsed();

        let optimized_started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(classify_provider_transaction_kind(&tx));
        }
        let optimized_elapsed = optimized_started.elapsed();

        eprintln!(
            "provider_transaction_kind_hoist_ids_profile_fixture iterations={} baseline_us={} optimized_us={} baseline_avg_ns_per_iteration={} optimized_avg_ns_per_iteration={} baseline_avg_us_per_iteration={:.3} optimized_avg_us_per_iteration={:.3}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
            avg_ns_per_iteration(baseline_elapsed, iterations),
            avg_ns_per_iteration(optimized_elapsed, iterations),
            avg_ns_per_iteration(baseline_elapsed, iterations) as f64 / 1_000.0,
            avg_ns_per_iteration(optimized_elapsed, iterations) as f64 / 1_000.0,
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

    #[test]
    #[ignore = "profiling fixture for provider source attachment path"]
    fn provider_update_source_attachment_profile_fixture() {
        let iterations = profile_iterations(1_000_000);
        let source = ProviderSourceIdentity::new(
            ProviderSourceId::Generic(Arc::<str>::from("custom")),
            "source-a",
        );
        let source_ref = Arc::new(source.clone());
        let update = sample_recent_blockhash_update();

        let baseline_started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(update.clone().with_provider_source(source.clone()));
        }
        let baseline_elapsed = baseline_started.elapsed();

        let optimized_started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(update.clone().with_provider_source_ref(&source_ref));
        }
        let optimized_elapsed = optimized_started.elapsed();
        let baseline_avg_ns = avg_ns_per_iteration(baseline_elapsed, iterations);
        let optimized_avg_ns = avg_ns_per_iteration(optimized_elapsed, iterations);

        eprintln!(
            "provider_update_source_attachment_profile_fixture iterations={} baseline_us={} optimized_us={} baseline_avg_ns_per_iteration={} optimized_avg_ns_per_iteration={} baseline_avg_us_per_iteration={:.3} optimized_avg_us_per_iteration={:.3}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
            baseline_avg_ns,
            optimized_avg_ns,
            baseline_avg_ns as f64 / 1_000.0,
            optimized_avg_ns as f64 / 1_000.0,
        );
    }
}
