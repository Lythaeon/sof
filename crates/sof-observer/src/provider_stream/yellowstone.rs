#![allow(clippy::missing_docs_in_private_items)]

//! Yellowstone gRPC adapters for SOF processed provider-stream ingress.

#[cfg(test)]
use std::hint::black_box;
use std::{
    collections::HashMap,
    fmt,
    pin::Pin,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use sof_support::bytes::{pubkey_bytes_from_slice, signature_bytes_from_slice};
use sof_support::collections_support::prune_recent_slots;
use sof_types::SignatureBytes;
use solana_hash::Hash;
use solana_message::{
    Message, MessageHeader, VersionedMessage,
    compiled_instruction::CompiledInstruction,
    v0::{Message as MessageV0, MessageAddressTableLookup},
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_system_interface::MAX_PERMITTED_DATA_LENGTH;
use solana_transaction::versioned::VersionedTransaction;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use yellowstone_grpc_client::{GeyserGrpcBuilderError, GeyserGrpcClient, GeyserGrpcClientError};
use yellowstone_grpc_proto::prelude::{
    CommitmentLevel, SlotStatus, SubscribeRequest, SubscribeRequestFilterAccounts,
    SubscribeRequestFilterBlocksMeta, SubscribeRequestFilterSlots,
    SubscribeRequestFilterTransactions, SubscribeRequestPing, SubscribeUpdate,
    subscribe_update::UpdateOneof,
};

use crate::{
    event::{ForkSlotStatus, TxCommitmentStatus, TxKind},
    framework::{
        AccountUpdateEvent, BlockMetaEvent, SlotStatusEvent, TransactionEvent,
        TransactionStatusEvent,
    },
    provider_stream::{
        ProviderCommitmentWatermarks, ProviderReplayMode, ProviderSourceArbitrationMode,
        ProviderSourceHealthEvent, ProviderSourceHealthReason, ProviderSourceHealthStatus,
        ProviderSourceId, ProviderSourceIdentity, ProviderSourceIdentityRegistrationError,
        ProviderSourceReadiness, ProviderSourceReservation, ProviderSourceRole,
        ProviderSourceTaskGuard, ProviderStreamFanIn, ProviderStreamMode, ProviderStreamSender,
        ProviderStreamUpdate, classify_provider_transaction_kind,
        emit_provider_source_removed_with_reservation, keepalive_interval,
    },
};

const INTERNAL_SLOT_FILTER: &str = "__sof_internal_slots";
const MAX_ACCOUNT_DATA_LEN: usize = MAX_PERMITTED_DATA_LENGTH as usize;
const SLOT_STATUS_RETAINED_LAG: u64 = 4_096;
const SLOT_STATUS_PRUNE_THRESHOLD: usize = SLOT_STATUS_RETAINED_LAG as usize * 2;
const DEFAULT_MAX_DECODING_MESSAGE_SIZE: usize = 64 * 1024 * 1024;

/// Yellowstone subscription commitment used for provider-stream transaction updates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum YellowstoneGrpcCommitment {
    /// Processed commitment.
    #[default]
    Processed,
    /// Confirmed commitment.
    Confirmed,
    /// Finalized commitment.
    Finalized,
}

impl YellowstoneGrpcCommitment {
    const fn as_proto(self) -> CommitmentLevel {
        match self {
            Self::Processed => CommitmentLevel::Processed,
            Self::Confirmed => CommitmentLevel::Confirmed,
            Self::Finalized => CommitmentLevel::Finalized,
        }
    }

    const fn as_tx_commitment(self) -> TxCommitmentStatus {
        match self {
            Self::Processed => TxCommitmentStatus::Processed,
            Self::Confirmed => TxCommitmentStatus::Confirmed,
            Self::Finalized => TxCommitmentStatus::Finalized,
        }
    }
}

/// Connection and filter config for Yellowstone processed-provider subscriptions.
#[derive(Clone, Debug)]
pub struct YellowstoneGrpcConfig {
    endpoint: String,
    x_token: Option<String>,
    source_instance: Option<Arc<str>>,
    readiness: ProviderSourceReadiness,
    source_role: ProviderSourceRole,
    source_priority: u16,
    source_arbitration: ProviderSourceArbitrationMode,
    stream: YellowstoneGrpcStream,
    commitment: YellowstoneGrpcCommitment,
    commitment_explicit: bool,
    vote: Option<bool>,
    failed: Option<bool>,
    signature: Option<Signature>,
    account_include: Vec<Pubkey>,
    account_exclude: Vec<Pubkey>,
    account_required: Vec<Pubkey>,
    accounts: Vec<Pubkey>,
    owners: Vec<Pubkey>,
    require_txn_signature: bool,
    max_decoding_message_size: usize,
    connect_timeout: Option<Duration>,
    stall_timeout: Option<Duration>,
    ping_interval: Option<Duration>,
    reconnect_delay: Duration,
    max_reconnect_attempts: Option<u32>,
    replay_mode: ProviderReplayMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// One Yellowstone stream-specific config option that may be invalid for a selected stream kind.
pub enum YellowstoneGrpcConfigOption {
    /// Vote transaction filter.
    VoteFilter,
    /// Failed transaction filter.
    FailedFilter,
    /// Single-signature filter.
    SignatureFilter,
    /// Account-include filter.
    AccountIncludeFilter,
    /// Account-exclude filter.
    AccountExcludeFilter,
    /// Account-required filter.
    AccountRequiredFilter,
    /// Explicit account pubkey filter.
    Accounts,
    /// Explicit owner/program filter.
    Owners,
    /// Require account updates to carry a transaction signature.
    RequireTransactionSignature,
}

impl fmt::Display for YellowstoneGrpcConfigOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VoteFilter => f.write_str("vote filter"),
            Self::FailedFilter => f.write_str("failed filter"),
            Self::SignatureFilter => f.write_str("signature filter"),
            Self::AccountIncludeFilter => f.write_str("account-include filter"),
            Self::AccountExcludeFilter => f.write_str("account-exclude filter"),
            Self::AccountRequiredFilter => f.write_str("account-required filter"),
            Self::Accounts => f.write_str("accounts filter"),
            Self::Owners => f.write_str("owners filter"),
            Self::RequireTransactionSignature => f.write_str("require-transaction-signature"),
        }
    }
}

#[derive(Debug, Error)]
/// Typed Yellowstone config validation failures for built-in stream selectors.
pub enum YellowstoneGrpcConfigError {
    /// The selected Yellowstone stream kind does not support one configured option.
    #[error("{option} is not supported for yellowstone {stream} streams")]
    UnsupportedOption {
        /// Built-in Yellowstone stream family receiving the invalid option.
        stream: YellowstoneGrpcStreamKind,
        /// Typed config option that is unsupported for that stream family.
        option: YellowstoneGrpcConfigOption,
    },
}

impl YellowstoneGrpcConfig {
    /// Creates a transaction-stream config for one Yellowstone endpoint.
    ///
    /// By default no vote/failed filter is applied, so the stream remains
    /// inclusive unless you narrow it explicitly.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sof::provider_stream::yellowstone::YellowstoneGrpcConfig;
    ///
    /// let config = YellowstoneGrpcConfig::new("http://127.0.0.1:10000");
    /// assert_eq!(config.endpoint(), "http://127.0.0.1:10000");
    /// ```
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            x_token: None,
            source_instance: None,
            readiness: ProviderSourceReadiness::Required,
            source_role: ProviderSourceRole::Primary,
            source_priority: ProviderSourceRole::Primary.default_priority(),
            source_arbitration: ProviderSourceArbitrationMode::EmitAll,
            stream: YellowstoneGrpcStream::Transaction,
            commitment: YellowstoneGrpcCommitment::Processed,
            commitment_explicit: false,
            vote: None,
            failed: None,
            signature: None,
            account_include: Vec::new(),
            account_exclude: Vec::new(),
            account_required: Vec::new(),
            accounts: Vec::new(),
            owners: Vec::new(),
            require_txn_signature: false,
            max_decoding_message_size: DEFAULT_MAX_DECODING_MESSAGE_SIZE,
            connect_timeout: Some(Duration::from_secs(10)),
            stall_timeout: Some(Duration::from_secs(30)),
            ping_interval: Some(Duration::from_secs(30)),
            reconnect_delay: Duration::from_secs(1),
            max_reconnect_attempts: None,
            replay_mode: ProviderReplayMode::Resume,
        }
    }

    /// Returns the configured endpoint.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Sets one stable source instance label for observability and redundancy intent.
    #[must_use]
    pub fn with_source_instance(mut self, instance: impl Into<Arc<str>>) -> Self {
        self.source_instance = Some(instance.into());
        self
    }

    /// Sets whether this source participates in runtime readiness gating.
    #[must_use]
    pub const fn with_readiness(mut self, readiness: ProviderSourceReadiness) -> Self {
        self.readiness = readiness;
        self
    }

    /// Sets the operational role for this source inside one fan-in graph.
    #[must_use]
    pub const fn with_source_role(mut self, role: ProviderSourceRole) -> Self {
        self.source_role = role;
        self.source_priority = role.default_priority();
        self
    }

    /// Sets one explicit arbitration priority for this source.
    #[must_use]
    pub const fn with_source_priority(mut self, priority: u16) -> Self {
        self.source_priority = priority;
        self
    }

    /// Sets duplicate arbitration policy for this source.
    #[must_use]
    pub const fn with_source_arbitration(
        mut self,
        arbitration: ProviderSourceArbitrationMode,
    ) -> Self {
        self.source_arbitration = arbitration;
        self
    }

    /// Selects the Yellowstone stream family this config should subscribe to.
    #[must_use]
    pub const fn with_stream(mut self, stream: YellowstoneGrpcStream) -> Self {
        if matches!(stream, YellowstoneGrpcStream::Accounts) && !self.commitment_explicit {
            self.commitment = YellowstoneGrpcCommitment::Finalized;
        }
        self.stream = stream;
        self
    }

    /// Sets the provider x-token.
    #[must_use]
    pub fn with_x_token(mut self, x_token: impl Into<String>) -> Self {
        self.x_token = Some(x_token.into());
        self
    }

    /// Sets the Yellowstone commitment.
    #[must_use]
    pub const fn with_commitment(mut self, commitment: YellowstoneGrpcCommitment) -> Self {
        self.commitment = commitment;
        self.commitment_explicit = true;
        self
    }

    /// Sets the vote filter.
    #[must_use]
    pub const fn with_vote(mut self, vote: bool) -> Self {
        self.vote = Some(vote);
        self
    }

    /// Sets the failed filter.
    #[must_use]
    pub const fn with_failed(mut self, failed: bool) -> Self {
        self.failed = Some(failed);
        self
    }

    /// Narrows the stream to one signature.
    #[must_use]
    pub const fn with_signature(mut self, signature: Signature) -> Self {
        self.signature = Some(signature);
        self
    }

    /// Requires at least one listed account key to appear.
    #[must_use]
    pub fn with_account_include<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = Pubkey>,
    {
        self.account_include.extend(keys);
        self
    }

    /// Rejects listed account keys.
    #[must_use]
    pub fn with_account_exclude<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = Pubkey>,
    {
        self.account_exclude.extend(keys);
        self
    }

    /// Requires all listed account keys to appear.
    #[must_use]
    pub fn with_account_required<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = Pubkey>,
    {
        self.account_required.extend(keys);
        self
    }

    /// Filters account streams to one or more explicit pubkeys.
    #[must_use]
    pub fn with_accounts<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = Pubkey>,
    {
        self.accounts.extend(keys);
        self
    }

    /// Filters account streams to one or more owners/program ids.
    #[must_use]
    pub fn with_owners<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = Pubkey>,
    {
        self.owners.extend(keys);
        self
    }

    /// Requires Yellowstone account updates to carry a transaction signature.
    #[must_use]
    pub const fn require_transaction_signature(mut self) -> Self {
        self.require_txn_signature = true;
        self
    }

    /// Sets the max decoding message size.
    #[must_use]
    pub const fn with_max_decoding_message_size(mut self, bytes: usize) -> Self {
        self.max_decoding_message_size = bytes;
        self
    }

    /// Sets the connect timeout.
    #[must_use]
    pub const fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Sets the idle watchdog timeout for one stream session.
    #[must_use]
    pub const fn with_stall_timeout(mut self, timeout: Duration) -> Self {
        self.stall_timeout = Some(timeout);
        self
    }

    /// Sets the periodic ping interval.
    #[must_use]
    pub const fn with_ping_interval(mut self, interval: Duration) -> Self {
        self.ping_interval = Some(interval);
        self
    }

    /// Sets the reconnect backoff used after stream failures.
    #[must_use]
    pub const fn with_reconnect_delay(mut self, delay: Duration) -> Self {
        self.reconnect_delay = delay;
        self
    }

    fn validate(&self) -> Result<(), YellowstoneGrpcConfigError> {
        match self.stream {
            YellowstoneGrpcStream::Transaction | YellowstoneGrpcStream::TransactionStatus => {
                self.validate_non_account_stream()?;
            }
            YellowstoneGrpcStream::Accounts => {
                self.validate_accounts_stream()?;
            }
            YellowstoneGrpcStream::BlockMeta => {
                self.validate_non_account_stream()?;
            }
        }
        Ok(())
    }

    fn validate_non_account_stream(&self) -> Result<(), YellowstoneGrpcConfigError> {
        if let Some(option) = [
            (!self.accounts.is_empty()).then_some(YellowstoneGrpcConfigOption::Accounts),
            (!self.owners.is_empty()).then_some(YellowstoneGrpcConfigOption::Owners),
            self.require_txn_signature
                .then_some(YellowstoneGrpcConfigOption::RequireTransactionSignature),
        ]
        .into_iter()
        .flatten()
        .next()
        {
            return Err(YellowstoneGrpcConfigError::UnsupportedOption {
                stream: self.stream_kind(),
                option,
            });
        }
        Ok(())
    }

    fn validate_accounts_stream(&self) -> Result<(), YellowstoneGrpcConfigError> {
        if let Some(option) = [
            self.vote.map(|_| YellowstoneGrpcConfigOption::VoteFilter),
            self.failed
                .map(|_| YellowstoneGrpcConfigOption::FailedFilter),
            self.signature
                .map(|_| YellowstoneGrpcConfigOption::SignatureFilter),
            (!self.account_include.is_empty())
                .then_some(YellowstoneGrpcConfigOption::AccountIncludeFilter),
            (!self.account_exclude.is_empty())
                .then_some(YellowstoneGrpcConfigOption::AccountExcludeFilter),
            (!self.account_required.is_empty())
                .then_some(YellowstoneGrpcConfigOption::AccountRequiredFilter),
        ]
        .into_iter()
        .flatten()
        .next()
        {
            return Err(YellowstoneGrpcConfigError::UnsupportedOption {
                stream: self.stream_kind(),
                option,
            });
        }
        Ok(())
    }

    /// Sets the maximum reconnect attempts. `None` keeps retrying forever.
    #[must_use]
    pub const fn with_max_reconnect_attempts(mut self, attempts: u32) -> Self {
        self.max_reconnect_attempts = Some(attempts);
        self
    }

    /// Sets provider replay behavior.
    #[must_use]
    pub const fn with_replay_mode(mut self, mode: ProviderReplayMode) -> Self {
        self.replay_mode = mode;
        self
    }

    #[cfg(test)]
    pub(crate) fn subscribe_request(&self) -> SubscribeRequest {
        self.subscribe_request_with_state(0)
    }

    fn subscribe_request_with_state(&self, tracked_slot: u64) -> SubscribeRequest {
        let mut request = SubscribeRequest {
            slots: HashMap::from([(
                INTERNAL_SLOT_FILTER.to_owned(),
                SubscribeRequestFilterSlots {
                    filter_by_commitment: Some(true),
                    ..SubscribeRequestFilterSlots::default()
                },
            )]),
            commitment: Some(self.commitment.as_proto() as i32),
            ..SubscribeRequest::default()
        };
        match self.stream {
            YellowstoneGrpcStream::Transaction => {
                request.transactions =
                    HashMap::from([("sof".to_owned(), self.transaction_filter())]);
            }
            YellowstoneGrpcStream::TransactionStatus => {
                request.transactions_status =
                    HashMap::from([("sof".to_owned(), self.transaction_filter())]);
            }
            YellowstoneGrpcStream::Accounts => {
                request.accounts = HashMap::from([("sof".to_owned(), self.account_filter())]);
            }
            YellowstoneGrpcStream::BlockMeta => {
                request.blocks_meta = HashMap::from([(
                    "sof".to_owned(),
                    SubscribeRequestFilterBlocksMeta::default(),
                )]);
            }
        }
        if let Some(from_slot) = self.replay_from_slot(tracked_slot) {
            request.from_slot = Some(from_slot);
        }
        request
    }

    const fn replay_from_slot(&self, tracked_slot: u64) -> Option<u64> {
        match self.replay_mode {
            ProviderReplayMode::Live => None,
            ProviderReplayMode::Resume => {
                if tracked_slot == 0 {
                    None
                } else {
                    Some(match self.commitment {
                        YellowstoneGrpcCommitment::Processed => tracked_slot.saturating_sub(31),
                        YellowstoneGrpcCommitment::Confirmed
                        | YellowstoneGrpcCommitment::Finalized => tracked_slot,
                    })
                }
            }
            ProviderReplayMode::FromSlot(slot) => {
                if tracked_slot == 0 {
                    Some(slot)
                } else {
                    Some(match self.commitment {
                        YellowstoneGrpcCommitment::Processed => tracked_slot.saturating_sub(31),
                        YellowstoneGrpcCommitment::Confirmed
                        | YellowstoneGrpcCommitment::Finalized => tracked_slot,
                    })
                }
            }
        }
    }

    fn transaction_filter(&self) -> SubscribeRequestFilterTransactions {
        SubscribeRequestFilterTransactions {
            vote: self.vote,
            failed: self.failed,
            signature: self.signature.map(|signature| signature.to_string()),
            account_include: self
                .account_include
                .iter()
                .map(ToString::to_string)
                .collect(),
            account_exclude: self
                .account_exclude
                .iter()
                .map(ToString::to_string)
                .collect(),
            account_required: self
                .account_required
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }

    fn account_filter(&self) -> SubscribeRequestFilterAccounts {
        SubscribeRequestFilterAccounts {
            account: self.accounts.iter().map(ToString::to_string).collect(),
            owner: self.owners.iter().map(ToString::to_string).collect(),
            nonempty_txn_signature: Some(self.require_txn_signature),
            ..SubscribeRequestFilterAccounts::default()
        }
    }

    const fn source_id(&self) -> ProviderSourceId {
        match self.stream {
            YellowstoneGrpcStream::Transaction => ProviderSourceId::YellowstoneGrpc,
            YellowstoneGrpcStream::TransactionStatus => {
                ProviderSourceId::YellowstoneGrpcTransactionStatus
            }
            YellowstoneGrpcStream::Accounts => ProviderSourceId::YellowstoneGrpcAccounts,
            YellowstoneGrpcStream::BlockMeta => ProviderSourceId::YellowstoneGrpcBlockMeta,
        }
    }

    fn source_instance(&self) -> Option<&str> {
        self.source_instance.as_deref()
    }

    const fn readiness(&self) -> ProviderSourceReadiness {
        self.readiness
    }

    fn source_identity(&self) -> Option<ProviderSourceIdentity> {
        self.source_instance().map(|instance| {
            ProviderSourceIdentity::new(self.source_id(), instance)
                .with_role(self.source_role)
                .with_priority(self.source_priority)
                .with_arbitration(self.source_arbitration)
        })
    }

    fn resolved_source_identity(&self) -> ProviderSourceIdentity {
        ProviderSourceIdentity::generated(self.source_id(), self.source_instance())
            .with_role(self.source_role)
            .with_priority(self.source_priority)
            .with_arbitration(self.source_arbitration)
    }

    /// Returns the runtime mode matching this built-in Yellowstone stream selection.
    #[must_use]
    pub const fn runtime_mode(&self) -> ProviderStreamMode {
        match self.stream {
            YellowstoneGrpcStream::Transaction => ProviderStreamMode::YellowstoneGrpc,
            YellowstoneGrpcStream::TransactionStatus => {
                ProviderStreamMode::YellowstoneGrpcTransactionStatus
            }
            YellowstoneGrpcStream::Accounts => ProviderStreamMode::YellowstoneGrpcAccounts,
            YellowstoneGrpcStream::BlockMeta => ProviderStreamMode::YellowstoneGrpcBlockMeta,
        }
    }

    const fn stream_kind(&self) -> YellowstoneGrpcStreamKind {
        match self.stream {
            YellowstoneGrpcStream::Transaction => YellowstoneGrpcStreamKind::Transaction,
            YellowstoneGrpcStream::TransactionStatus => {
                YellowstoneGrpcStreamKind::TransactionStatus
            }
            YellowstoneGrpcStream::Accounts => YellowstoneGrpcStreamKind::Accounts,
            YellowstoneGrpcStream::BlockMeta => YellowstoneGrpcStreamKind::BlockMeta,
        }
    }
}

/// Primary Yellowstone gRPC stream families supported by the built-in adapter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum YellowstoneGrpcStream {
    /// Full transaction updates mapped onto `on_transaction`.
    #[default]
    Transaction,
    /// Signature-level transaction status updates mapped onto `on_transaction_status`.
    TransactionStatus,
    /// Account updates mapped onto `on_account_update`.
    Accounts,
    /// Block-meta updates mapped onto `on_block_meta`.
    BlockMeta,
}

/// Connection and replay config for Yellowstone slot subscriptions.
#[derive(Clone, Debug)]
pub struct YellowstoneGrpcSlotsConfig {
    endpoint: String,
    x_token: Option<String>,
    source_instance: Option<Arc<str>>,
    readiness: ProviderSourceReadiness,
    source_role: ProviderSourceRole,
    source_priority: u16,
    source_arbitration: ProviderSourceArbitrationMode,
    commitment: YellowstoneGrpcCommitment,
    connect_timeout: Option<Duration>,
    stall_timeout: Option<Duration>,
    ping_interval: Option<Duration>,
    reconnect_delay: Duration,
    max_reconnect_attempts: Option<u32>,
    replay_mode: ProviderReplayMode,
}

impl YellowstoneGrpcSlotsConfig {
    /// Creates a slot-stream config for one Yellowstone endpoint.
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            x_token: None,
            source_instance: None,
            readiness: ProviderSourceReadiness::Optional,
            source_role: ProviderSourceRole::Secondary,
            source_priority: ProviderSourceRole::Secondary.default_priority(),
            source_arbitration: ProviderSourceArbitrationMode::EmitAll,
            commitment: YellowstoneGrpcCommitment::Processed,
            connect_timeout: Some(Duration::from_secs(10)),
            stall_timeout: Some(Duration::from_secs(30)),
            ping_interval: Some(Duration::from_secs(30)),
            reconnect_delay: Duration::from_secs(1),
            max_reconnect_attempts: None,
            replay_mode: ProviderReplayMode::Resume,
        }
    }

    /// Returns the configured endpoint.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Sets one stable source instance label for observability and redundancy intent.
    #[must_use]
    pub fn with_source_instance(mut self, instance: impl Into<Arc<str>>) -> Self {
        self.source_instance = Some(instance.into());
        self
    }

    /// Sets whether this source participates in runtime readiness gating.
    #[must_use]
    pub const fn with_readiness(mut self, readiness: ProviderSourceReadiness) -> Self {
        self.readiness = readiness;
        self
    }

    /// Sets the operational role for this source inside one fan-in graph.
    #[must_use]
    pub const fn with_source_role(mut self, role: ProviderSourceRole) -> Self {
        self.source_role = role;
        self.source_priority = role.default_priority();
        self
    }

    /// Sets one explicit arbitration priority for this source.
    #[must_use]
    pub const fn with_source_priority(mut self, priority: u16) -> Self {
        self.source_priority = priority;
        self
    }

    /// Sets duplicate arbitration policy for this source.
    #[must_use]
    pub const fn with_source_arbitration(
        mut self,
        arbitration: ProviderSourceArbitrationMode,
    ) -> Self {
        self.source_arbitration = arbitration;
        self
    }

    /// Returns the runtime mode matching this built-in Yellowstone slot stream.
    #[must_use]
    pub const fn runtime_mode(&self) -> ProviderStreamMode {
        ProviderStreamMode::YellowstoneGrpcSlots
    }

    fn source_instance(&self) -> Option<&str> {
        self.source_instance.as_deref()
    }

    const fn readiness(&self) -> ProviderSourceReadiness {
        self.readiness
    }

    fn source_identity(&self) -> Option<ProviderSourceIdentity> {
        self.source_instance().map(|instance| {
            ProviderSourceIdentity::new(ProviderSourceId::YellowstoneGrpcSlots, instance)
                .with_role(self.source_role)
                .with_priority(self.source_priority)
                .with_arbitration(self.source_arbitration)
        })
    }

    fn resolved_source_identity(&self) -> ProviderSourceIdentity {
        ProviderSourceIdentity::generated(
            ProviderSourceId::YellowstoneGrpcSlots,
            self.source_instance(),
        )
        .with_role(self.source_role)
        .with_priority(self.source_priority)
        .with_arbitration(self.source_arbitration)
    }

    /// Sets the provider x-token.
    #[must_use]
    pub fn with_x_token(mut self, x_token: impl Into<String>) -> Self {
        self.x_token = Some(x_token.into());
        self
    }

    /// Sets the Yellowstone commitment.
    #[must_use]
    pub const fn with_commitment(mut self, commitment: YellowstoneGrpcCommitment) -> Self {
        self.commitment = commitment;
        self
    }

    /// Sets the connect timeout.
    #[must_use]
    pub const fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Sets the idle watchdog timeout for one stream session.
    #[must_use]
    pub const fn with_stall_timeout(mut self, timeout: Duration) -> Self {
        self.stall_timeout = Some(timeout);
        self
    }

    /// Sets the periodic ping interval.
    #[must_use]
    pub const fn with_ping_interval(mut self, interval: Duration) -> Self {
        self.ping_interval = Some(interval);
        self
    }

    /// Sets the reconnect backoff used after stream failures.
    #[must_use]
    pub const fn with_reconnect_delay(mut self, delay: Duration) -> Self {
        self.reconnect_delay = delay;
        self
    }

    /// Sets the maximum reconnect attempts. `None` keeps retrying forever.
    #[must_use]
    pub const fn with_max_reconnect_attempts(mut self, attempts: u32) -> Self {
        self.max_reconnect_attempts = Some(attempts);
        self
    }

    /// Sets provider replay behavior.
    #[must_use]
    pub const fn with_replay_mode(mut self, mode: ProviderReplayMode) -> Self {
        self.replay_mode = mode;
        self
    }

    fn subscribe_request_with_state(&self, tracked_slot: u64) -> SubscribeRequest {
        let mut request = SubscribeRequest {
            slots: HashMap::from([(
                "sof".to_owned(),
                SubscribeRequestFilterSlots {
                    filter_by_commitment: Some(true),
                    ..SubscribeRequestFilterSlots::default()
                },
            )]),
            commitment: Some(self.commitment.as_proto() as i32),
            ..SubscribeRequest::default()
        };
        if let Some(from_slot) = self.replay_from_slot(tracked_slot) {
            request.from_slot = Some(from_slot);
        }
        request
    }

    const fn replay_from_slot(&self, tracked_slot: u64) -> Option<u64> {
        match self.replay_mode {
            ProviderReplayMode::Live => None,
            ProviderReplayMode::Resume => {
                if tracked_slot == 0 {
                    None
                } else {
                    Some(match self.commitment {
                        YellowstoneGrpcCommitment::Processed => tracked_slot.saturating_sub(31),
                        YellowstoneGrpcCommitment::Confirmed
                        | YellowstoneGrpcCommitment::Finalized => tracked_slot,
                    })
                }
            }
            ProviderReplayMode::FromSlot(slot) => {
                if tracked_slot == 0 {
                    Some(slot)
                } else {
                    Some(match self.commitment {
                        YellowstoneGrpcCommitment::Processed => tracked_slot.saturating_sub(31),
                        YellowstoneGrpcCommitment::Confirmed
                        | YellowstoneGrpcCommitment::Finalized => tracked_slot,
                    })
                }
            }
        }
    }
}

/// Yellowstone transaction-stream error surface.
#[derive(Debug, Error)]
pub enum YellowstoneGrpcError {
    /// Invalid config for the selected Yellowstone stream kind.
    #[error(transparent)]
    Config(#[from] YellowstoneGrpcConfigError),
    /// Builder/connect failure.
    #[error(transparent)]
    Build(#[from] GeyserGrpcBuilderError),
    /// Subscribe or stream error from the client.
    #[error(transparent)]
    Client(#[from] GeyserGrpcClientError),
    /// Stream status error.
    #[error("yellowstone stream status: {0}")]
    Status(#[from] yellowstone_grpc_proto::tonic::Status),
    /// Provider update could not be converted into a SOF event.
    #[error("yellowstone transaction conversion failed: {0}")]
    Convert(&'static str),
    /// Yellowstone protocol/runtime failure.
    #[error(transparent)]
    Protocol(#[from] YellowstoneGrpcProtocolError),
    /// Provider-stream queue is closed.
    #[error("provider-stream queue closed")]
    QueueClosed,
    /// One duplicated stable source identity was registered in the same fan-in.
    #[error(transparent)]
    DuplicateSourceIdentity(#[from] ProviderSourceIdentityRegistrationError),
}

/// Typed Yellowstone protocol/runtime failures.
#[derive(Debug, Error)]
pub enum YellowstoneGrpcProtocolError {
    /// One Yellowstone primary stream stopped making progress.
    #[error("yellowstone {stream} stream stalled without inbound progress")]
    StreamStalled {
        /// Typed Yellowstone stream family that stalled.
        stream: YellowstoneGrpcStreamKind,
    },
    /// One Yellowstone slot stream stopped making progress.
    #[error("yellowstone slot stream stalled without inbound progress")]
    SlotStreamStalled,
    /// Reconnect attempts were exhausted.
    ///
    /// `attempts` is the consecutive-failure count that tripped the budget.
    #[error("exhausted yellowstone reconnect attempts after {attempts} failures")]
    ReconnectBudgetExhausted {
        /// Consecutive-failure count that exhausted the reconnect budget.
        attempts: u32,
    },
}

/// Stable Yellowstone stream kinds used by typed protocol errors.
#[derive(Clone, Copy, Debug)]
pub enum YellowstoneGrpcStreamKind {
    /// Transaction stream.
    Transaction,
    /// Transaction status stream.
    TransactionStatus,
    /// Account stream.
    Accounts,
    /// Block-meta stream.
    BlockMeta,
    /// Slot stream.
    Slots,
}

impl fmt::Display for YellowstoneGrpcStreamKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transaction => f.write_str("transaction"),
            Self::TransactionStatus => f.write_str("transaction-status"),
            Self::Accounts => f.write_str("account"),
            Self::BlockMeta => f.write_str("block-meta"),
            Self::Slots => f.write_str("slot"),
        }
    }
}

type YellowstoneSubscribeSink = Pin<
    Box<dyn futures_util::Sink<SubscribeRequest, Error = futures_channel::mpsc::SendError> + Send>,
>;
type YellowstoneUpdateStream = Pin<
    Box<
        dyn futures_util::Stream<
                Item = Result<SubscribeUpdate, yellowstone_grpc_proto::tonic::Status>,
            > + Send,
    >,
>;

struct YellowstonePrimaryConnectionState<'state> {
    tracked_slot: &'state mut u64,
    watermarks: &'state mut ProviderCommitmentWatermarks,
    session_established: &'state mut bool,
}

struct YellowstoneSlotConnectionState<'state> {
    tracked_slot: &'state mut u64,
    watermarks: &'state mut ProviderCommitmentWatermarks,
    slot_states: &'state mut HashMap<u64, ForkSlotStatus>,
    session_established: &'state mut bool,
}

/// Spawns one Yellowstone gRPC primary source into a SOF provider-stream queue.
///
/// # Examples
///
/// ```no_run
/// use sof::provider_stream::{
///     create_provider_stream_queue,
///     yellowstone::{spawn_yellowstone_grpc_source, YellowstoneGrpcConfig},
/// };
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let (tx, _rx) = create_provider_stream_queue(1024);
/// let handle = spawn_yellowstone_grpc_source(
///     YellowstoneGrpcConfig::new("http://127.0.0.1:10000"),
///     tx,
/// )
/// .await?;
/// handle.abort();
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// Returns any connection/bootstrap error before the forwarder task starts.
pub async fn spawn_yellowstone_grpc_source(
    config: YellowstoneGrpcConfig,
    sender: ProviderStreamSender,
) -> Result<JoinHandle<Result<(), YellowstoneGrpcError>>, YellowstoneGrpcError> {
    spawn_yellowstone_grpc_source_inner(config, sender, None).await
}

#[deprecated(note = "use spawn_yellowstone_grpc_source")]
/// Deprecated compatibility shim for [`spawn_yellowstone_grpc_source`].
///
/// # Errors
///
/// Returns the same startup validation or bootstrap error as
/// [`spawn_yellowstone_grpc_source`].
pub async fn spawn_yellowstone_grpc_transaction_source(
    config: YellowstoneGrpcConfig,
    sender: ProviderStreamSender,
) -> Result<JoinHandle<Result<(), YellowstoneGrpcError>>, YellowstoneGrpcError> {
    spawn_yellowstone_grpc_source(config, sender).await
}

async fn spawn_yellowstone_grpc_source_inner(
    config: YellowstoneGrpcConfig,
    sender: ProviderStreamSender,
    reservation: Option<Arc<ProviderSourceReservation>>,
) -> Result<JoinHandle<Result<(), YellowstoneGrpcError>>, YellowstoneGrpcError> {
    config.validate()?;
    let source = config.resolved_source_identity();
    let initial_health = queue_primary_provider_health(
        &source,
        config.readiness(),
        &sender,
        ProviderSourceHealthStatus::Reconnecting,
        ProviderSourceHealthReason::InitialConnectPending,
        format!(
            "waiting for first yellowstone {} session ack",
            source.kind_str().trim_start_matches("yellowstone_grpc_")
        ),
    )?;
    let first_session = match establish_yellowstone_session(&config, 0).await {
        Ok(session) => session,
        Err(error) => {
            emit_provider_source_removed_with_reservation(
                &sender,
                source,
                config.readiness(),
                error.to_string(),
                reservation,
            );
            return Err(error);
        }
    };
    let source_task = ProviderSourceTaskGuard::new(
        sender.clone(),
        source.clone(),
        config.readiness(),
        reservation,
    );
    Ok(tokio::spawn(async move {
        let _source_task = source_task;
        if let Some(update) = initial_health {
            sender
                .send(update)
                .await
                .map_err(|_error| YellowstoneGrpcError::QueueClosed)?;
        }
        let mut attempts = 0_u32;
        let mut tracked_slot = 0_u64;
        let mut watermarks = ProviderCommitmentWatermarks::default();
        let mut first_session = Some(first_session);
        loop {
            let mut session_established = false;
            let session = match first_session.take() {
                Some(session) => Ok(session),
                None => establish_yellowstone_session(&config, tracked_slot).await,
            };
            match session {
                Ok((subscribe_tx, stream)) => match run_yellowstone_primary_connection(
                    &config,
                    &source,
                    &sender,
                    YellowstonePrimaryConnectionState {
                        tracked_slot: &mut tracked_slot,
                        watermarks: &mut watermarks,
                        session_established: &mut session_established,
                    },
                    subscribe_tx,
                    stream,
                )
                .await
                {
                    Ok(()) => {
                        let detail = format!(
                            "yellowstone {} stream ended unexpectedly",
                            config.stream_kind()
                        );
                        tracing::warn!(
                            endpoint = config.endpoint(),
                            detail,
                            "provider stream yellowstone session ended unexpectedly; reconnecting"
                        );
                        send_primary_provider_health(
                            &source,
                            config.readiness(),
                            &sender,
                            ProviderSourceHealthStatus::Reconnecting,
                            ProviderSourceHealthReason::UpstreamStreamClosedUnexpectedly,
                            detail,
                        )
                        .await?;
                    }
                    Err(YellowstoneGrpcError::QueueClosed) => {
                        return Err(YellowstoneGrpcError::QueueClosed);
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            endpoint = config.endpoint(),
                            "provider stream yellowstone session ended; reconnecting"
                        );
                        send_primary_provider_health(
                            &source,
                            config.readiness(),
                            &sender,
                            ProviderSourceHealthStatus::Reconnecting,
                            yellowstone_health_reason(&error),
                            error.to_string(),
                        )
                        .await?;
                    }
                },
                Err(error) => {
                    tracing::warn!(
                        %error,
                        endpoint = config.endpoint(),
                        "provider stream yellowstone connect/subscribe failed; reconnecting"
                    );
                    send_primary_provider_health(
                        &source,
                        config.readiness(),
                        &sender,
                        ProviderSourceHealthStatus::Reconnecting,
                        yellowstone_health_reason(&error),
                        error.to_string(),
                    )
                    .await?;
                }
            }
            if session_established {
                attempts = 0;
            } else {
                attempts = attempts.saturating_add(1);
            }
            if let Some(max_attempts) = config.max_reconnect_attempts
                && attempts >= max_attempts
            {
                let detail =
                    format!("exhausted yellowstone reconnect attempts after {attempts} failures");
                send_primary_provider_health(
                    &source,
                    config.readiness(),
                    &sender,
                    ProviderSourceHealthStatus::Unhealthy,
                    ProviderSourceHealthReason::ReconnectBudgetExhausted,
                    detail.clone(),
                )
                .await?;
                return Err(
                    YellowstoneGrpcProtocolError::ReconnectBudgetExhausted { attempts }.into(),
                );
            }
            tokio::time::sleep(config.reconnect_delay).await;
        }
    }))
}

/// Spawns one Yellowstone gRPC slot source into a SOF provider-stream queue.
///
/// # Errors
///
/// Returns an error if the initial client connect or subscribe step fails.
pub async fn spawn_yellowstone_grpc_slot_source(
    config: YellowstoneGrpcSlotsConfig,
    sender: ProviderStreamSender,
) -> Result<JoinHandle<Result<(), YellowstoneGrpcError>>, YellowstoneGrpcError> {
    spawn_yellowstone_grpc_slot_source_inner(config, sender, None).await
}

async fn spawn_yellowstone_grpc_slot_source_inner(
    config: YellowstoneGrpcSlotsConfig,
    sender: ProviderStreamSender,
    reservation: Option<Arc<ProviderSourceReservation>>,
) -> Result<JoinHandle<Result<(), YellowstoneGrpcError>>, YellowstoneGrpcError> {
    let source = config.resolved_source_identity();
    let initial_health = queue_provider_slot_health(
        &source,
        config.readiness(),
        &sender,
        ProviderSourceHealthStatus::Reconnecting,
        ProviderSourceHealthReason::InitialConnectPending,
        "waiting for first yellowstone slot session ack".to_owned(),
    )?;
    let first_session = match establish_yellowstone_slot_session(&config, 0).await {
        Ok(session) => session,
        Err(error) => {
            emit_provider_source_removed_with_reservation(
                &sender,
                source,
                config.readiness(),
                error.to_string(),
                reservation,
            );
            return Err(error);
        }
    };
    let source_task = ProviderSourceTaskGuard::new(
        sender.clone(),
        source.clone(),
        config.readiness(),
        reservation,
    );
    Ok(tokio::spawn(async move {
        let _source_task = source_task;
        if let Some(update) = initial_health {
            sender
                .send(update)
                .await
                .map_err(|_error| YellowstoneGrpcError::QueueClosed)?;
        }
        let mut attempts = 0_u32;
        let mut tracked_slot = 0_u64;
        let mut watermarks = ProviderCommitmentWatermarks::default();
        let mut slot_states = HashMap::with_capacity(SLOT_STATUS_PRUNE_THRESHOLD);
        let mut first_session = Some(first_session);
        loop {
            let mut session_established = false;
            let session = match first_session.take() {
                Some(session) => Ok(session),
                None => establish_yellowstone_slot_session(&config, tracked_slot).await,
            };
            match session {
                Ok((subscribe_tx, stream)) => match run_yellowstone_slot_connection(
                    &config,
                    &source,
                    &sender,
                    YellowstoneSlotConnectionState {
                        tracked_slot: &mut tracked_slot,
                        watermarks: &mut watermarks,
                        slot_states: &mut slot_states,
                        session_established: &mut session_established,
                    },
                    subscribe_tx,
                    stream,
                )
                .await
                {
                    Ok(()) => {
                        let detail = "yellowstone slot stream ended unexpectedly".to_owned();
                        tracing::warn!(
                            endpoint = config.endpoint(),
                            detail,
                            "provider stream yellowstone slot session ended unexpectedly; reconnecting"
                        );
                        send_provider_slot_health(
                            &source,
                            config.readiness(),
                            &sender,
                            ProviderSourceHealthStatus::Reconnecting,
                            ProviderSourceHealthReason::UpstreamStreamClosedUnexpectedly,
                            detail,
                        )
                        .await?;
                    }
                    Err(YellowstoneGrpcError::QueueClosed) => {
                        return Err(YellowstoneGrpcError::QueueClosed);
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            endpoint = config.endpoint(),
                            "provider stream yellowstone slot session ended; reconnecting"
                        );
                        send_provider_slot_health(
                            &source,
                            config.readiness(),
                            &sender,
                            ProviderSourceHealthStatus::Reconnecting,
                            yellowstone_health_reason(&error),
                            error.to_string(),
                        )
                        .await?;
                    }
                },
                Err(error) => {
                    tracing::warn!(
                        %error,
                        endpoint = config.endpoint(),
                        "provider stream yellowstone slot connect/subscribe failed; reconnecting"
                    );
                    send_provider_slot_health(
                        &source,
                        config.readiness(),
                        &sender,
                        ProviderSourceHealthStatus::Reconnecting,
                        yellowstone_health_reason(&error),
                        error.to_string(),
                    )
                    .await?;
                }
            }
            if session_established {
                attempts = 0;
            } else {
                attempts = attempts.saturating_add(1);
            }
            if let Some(max_attempts) = config.max_reconnect_attempts
                && attempts >= max_attempts
            {
                let detail = format!(
                    "exhausted yellowstone slot reconnect attempts after {attempts} failures"
                );
                send_provider_slot_health(
                    &source,
                    config.readiness(),
                    &sender,
                    ProviderSourceHealthStatus::Unhealthy,
                    ProviderSourceHealthReason::ReconnectBudgetExhausted,
                    detail.clone(),
                )
                .await?;
                return Err(
                    YellowstoneGrpcProtocolError::ReconnectBudgetExhausted { attempts }.into(),
                );
            }
            tokio::time::sleep(config.reconnect_delay).await;
        }
    }))
}

async fn run_yellowstone_primary_connection(
    config: &YellowstoneGrpcConfig,
    source: &ProviderSourceIdentity,
    sender: &ProviderStreamSender,
    state: YellowstonePrimaryConnectionState<'_>,
    mut subscribe_tx: YellowstoneSubscribeSink,
    mut stream: YellowstoneUpdateStream,
) -> Result<(), YellowstoneGrpcError> {
    let commitment = config.commitment.as_tx_commitment();
    let provider_source = Arc::new(source.clone());
    *state.session_established = false;
    *state.session_established = true;
    send_primary_provider_health(
        source,
        config.readiness(),
        sender,
        ProviderSourceHealthStatus::Healthy,
        ProviderSourceHealthReason::SubscriptionAckReceived,
        PROVIDER_SUBSCRIPTION_ACKNOWLEDGED.to_owned(),
    )
    .await?;
    let mut ping = config.ping_interval.map(keepalive_interval);
    let mut last_progress = Instant::now();
    loop {
        tokio::select! {
            () = async {
                if let Some(interval) = ping.as_mut() {
                    interval.tick().await;
                } else {
                    futures_util::future::pending::<()>().await;
                }
            } => {
                subscribe_tx
                    .send(SubscribeRequest {
                        ping: Some(SubscribeRequestPing { id: 1 }),
                        ..SubscribeRequest::default()
                    })
                    .await
                    .map_err(GeyserGrpcClientError::SubscribeSendError)?;
            }
            () = async {
                if let Some(timeout) = config.stall_timeout {
                    let deadline = last_progress.checked_add(timeout).unwrap_or(last_progress);
                    tokio::time::sleep_until(deadline.into()).await;
                } else {
                    futures_util::future::pending::<()>().await;
                }
            } => {
                return Err(YellowstoneGrpcProtocolError::StreamStalled {
                    stream: config.stream_kind(),
                }
                .into());
            }
            maybe_update = stream.next() => {
                let Some(update) = maybe_update else {
                    return Ok(());
                };
                let update = update?;
                last_progress = Instant::now();
                match update.update_oneof {
                    Some(UpdateOneof::Transaction(tx_update))
                        if config.stream == YellowstoneGrpcStream::Transaction =>
                    {
                        *state.tracked_slot = (*state.tracked_slot).max(tx_update.slot);
                        state
                            .watermarks
                            .observe_transaction_commitment(tx_update.slot, commitment);
                        let event = transaction_event_from_update(
                            tx_update.slot,
                            tx_update.transaction,
                            commitment,
                            *state.watermarks,
                        )?;
                        sender
                            .send(
                                ProviderStreamUpdate::Transaction(event)
                                    .with_provider_source_ref(&provider_source),
                            )
                            .await
                            .map_err(|_error| YellowstoneGrpcError::QueueClosed)?;
                    }
                    Some(UpdateOneof::TransactionStatus(status_update))
                        if config.stream == YellowstoneGrpcStream::TransactionStatus =>
                    {
                        *state.tracked_slot = (*state.tracked_slot).max(status_update.slot);
                        state
                            .watermarks
                            .observe_transaction_commitment(status_update.slot, commitment);
                        let event = transaction_status_event_from_update(
                            commitment,
                            *state.watermarks,
                            status_update,
                        )?;
                        sender
                            .send(
                                ProviderStreamUpdate::TransactionStatus(event)
                                    .with_provider_source_ref(&provider_source),
                            )
                            .await
                            .map_err(|_error| YellowstoneGrpcError::QueueClosed)?;
                    }
                    Some(UpdateOneof::Account(account_update))
                        if config.stream == YellowstoneGrpcStream::Accounts =>
                    {
                        *state.tracked_slot = (*state.tracked_slot).max(account_update.slot);
                        observe_non_transaction_commitment(
                            state.watermarks,
                            account_update.slot,
                            commitment,
                        );
                        let event = account_update_event_from_yellowstone(
                            account_update,
                            commitment,
                            *state.watermarks,
                        )?;
                        sender
                            .send(
                                ProviderStreamUpdate::AccountUpdate(event)
                                    .with_provider_source_ref(&provider_source),
                            )
                            .await
                            .map_err(|_error| YellowstoneGrpcError::QueueClosed)?;
                    }
                    Some(UpdateOneof::BlockMeta(block_meta_update))
                        if config.stream == YellowstoneGrpcStream::BlockMeta =>
                    {
                        *state.tracked_slot = (*state.tracked_slot).max(block_meta_update.slot);
                        observe_non_transaction_commitment(
                            state.watermarks,
                            block_meta_update.slot,
                            commitment,
                        );
                        let event = block_meta_event_from_update(
                            commitment,
                            *state.watermarks,
                            &block_meta_update,
                        )?;
                        sender
                            .send(
                                ProviderStreamUpdate::BlockMeta(event)
                                    .with_provider_source_ref(&provider_source),
                            )
                            .await
                            .map_err(|_error| YellowstoneGrpcError::QueueClosed)?;
                    }
                    Some(UpdateOneof::Slot(slot_update)) => {
                        *state.tracked_slot = (*state.tracked_slot).max(slot_update.slot);
                        match SlotStatus::try_from(slot_update.status).ok() {
                            Some(SlotStatus::SlotConfirmed) => {
                                state.watermarks.observe_confirmed_slot(slot_update.slot);
                            }
                            Some(SlotStatus::SlotFinalized) => {
                                state.watermarks.observe_finalized_slot(slot_update.slot);
                            }
                            _ => {}
                        }
                    }
                    Some(UpdateOneof::Ping(_)) => {
                        subscribe_tx
                            .send(SubscribeRequest {
                                ping: Some(SubscribeRequestPing { id: 1 }),
                                ..SubscribeRequest::default()
                            })
                            .await
                            .map_err(GeyserGrpcClientError::SubscribeSendError)?;
                    }
                    Some(UpdateOneof::Pong(_)) | None => {}
                    _ => {}
                }
            }
        }
    }
}

async fn run_yellowstone_slot_connection(
    config: &YellowstoneGrpcSlotsConfig,
    source: &ProviderSourceIdentity,
    sender: &ProviderStreamSender,
    state: YellowstoneSlotConnectionState<'_>,
    mut subscribe_tx: YellowstoneSubscribeSink,
    mut stream: YellowstoneUpdateStream,
) -> Result<(), YellowstoneGrpcError> {
    let provider_source = Arc::new(source.clone());
    *state.session_established = false;
    *state.session_established = true;
    send_provider_slot_health(
        source,
        config.readiness(),
        sender,
        ProviderSourceHealthStatus::Healthy,
        ProviderSourceHealthReason::SubscriptionAckReceived,
        PROVIDER_SUBSCRIPTION_ACKNOWLEDGED.to_owned(),
    )
    .await?;
    let mut ping = config.ping_interval.map(keepalive_interval);
    let mut last_progress = Instant::now();
    loop {
        tokio::select! {
            () = async {
                if let Some(interval) = ping.as_mut() {
                    interval.tick().await;
                } else {
                    futures_util::future::pending::<()>().await;
                }
            } => {
                subscribe_tx
                    .send(SubscribeRequest {
                        ping: Some(SubscribeRequestPing { id: 1 }),
                        ..SubscribeRequest::default()
                    })
                    .await
                    .map_err(GeyserGrpcClientError::SubscribeSendError)?;
            }
            () = async {
                if let Some(timeout) = config.stall_timeout {
                    let deadline = last_progress.checked_add(timeout).unwrap_or(last_progress);
                    tokio::time::sleep_until(deadline.into()).await;
                } else {
                    futures_util::future::pending::<()>().await;
                }
            } => {
                return Err(YellowstoneGrpcProtocolError::SlotStreamStalled.into());
            }
            maybe_update = stream.next() => {
                let Some(update) = maybe_update else {
                    return Ok(());
                };
                let update = update?;
                last_progress = Instant::now();
                match update.update_oneof {
                    Some(UpdateOneof::Slot(slot_update)) => {
                        *state.tracked_slot = (*state.tracked_slot).max(slot_update.slot);
                        if let Some(event) = slot_status_event_from_update(
                            slot_update.slot,
                            slot_update.parent,
                            SlotStatus::try_from(slot_update.status).ok(),
                            state.watermarks,
                            state.slot_states,
                        ) {
                            sender
                                .send(
                                    ProviderStreamUpdate::SlotStatus(event)
                                        .with_provider_source_ref(&provider_source),
                                )
                                .await
                                .map_err(|_error| YellowstoneGrpcError::QueueClosed)?;
                        }
                    }
                    Some(UpdateOneof::Ping(_)) => {
                        subscribe_tx
                            .send(SubscribeRequest {
                                ping: Some(SubscribeRequestPing { id: 1 }),
                                ..SubscribeRequest::default()
                            })
                            .await
                            .map_err(GeyserGrpcClientError::SubscribeSendError)?;
                    }
                    Some(UpdateOneof::Pong(_)) | None => {}
                    _ => {}
                }
            }
        }
    }
}

async fn establish_yellowstone_session(
    config: &YellowstoneGrpcConfig,
    tracked_slot: u64,
) -> Result<(YellowstoneSubscribeSink, YellowstoneUpdateStream), YellowstoneGrpcError> {
    establish_yellowstone_subscribe_session(
        config.endpoint.clone(),
        config.x_token.clone(),
        config.max_decoding_message_size,
        config.connect_timeout,
        config.subscribe_request_with_state(tracked_slot),
    )
    .await
}

async fn establish_yellowstone_slot_session(
    config: &YellowstoneGrpcSlotsConfig,
    tracked_slot: u64,
) -> Result<(YellowstoneSubscribeSink, YellowstoneUpdateStream), YellowstoneGrpcError> {
    establish_yellowstone_subscribe_session(
        config.endpoint.clone(),
        config.x_token.clone(),
        DEFAULT_MAX_DECODING_MESSAGE_SIZE,
        config.connect_timeout,
        config.subscribe_request_with_state(tracked_slot),
    )
    .await
}

async fn establish_yellowstone_subscribe_session(
    endpoint: String,
    x_token: Option<String>,
    max_decoding_message_size: usize,
    connect_timeout: Option<Duration>,
    request: SubscribeRequest,
) -> Result<(YellowstoneSubscribeSink, YellowstoneUpdateStream), YellowstoneGrpcError> {
    let mut builder = GeyserGrpcClient::build_from_shared(endpoint)?
        .x_token(x_token)?
        .max_decoding_message_size(max_decoding_message_size);
    if let Some(timeout) = connect_timeout {
        builder = builder.connect_timeout(timeout);
    }
    let mut client = builder.connect().await?;
    let (subscribe_tx, stream) = client.subscribe_with_request(Some(request)).await?;
    Ok((Box::pin(subscribe_tx), Box::pin(stream)))
}

async fn send_primary_provider_health(
    source: &ProviderSourceIdentity,
    readiness: ProviderSourceReadiness,
    sender: &ProviderStreamSender,
    status: ProviderSourceHealthStatus,
    reason: ProviderSourceHealthReason,
    message: String,
) -> Result<(), YellowstoneGrpcError> {
    sender
        .send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
            source: source.clone(),
            readiness,
            status,
            reason,
            message,
        }))
        .await
        .map_err(|_error| YellowstoneGrpcError::QueueClosed)
}

fn queue_primary_provider_health(
    source: &ProviderSourceIdentity,
    readiness: ProviderSourceReadiness,
    sender: &ProviderStreamSender,
    status: ProviderSourceHealthStatus,
    reason: ProviderSourceHealthReason,
    message: String,
) -> Result<Option<ProviderStreamUpdate>, YellowstoneGrpcError> {
    queue_provider_health_update(
        source,
        readiness,
        sender,
        status,
        reason,
        message,
        YellowstoneGrpcError::QueueClosed,
    )
}

async fn send_provider_slot_health(
    source: &ProviderSourceIdentity,
    readiness: ProviderSourceReadiness,
    sender: &ProviderStreamSender,
    status: ProviderSourceHealthStatus,
    reason: ProviderSourceHealthReason,
    message: String,
) -> Result<(), YellowstoneGrpcError> {
    sender
        .send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
            source: source.clone(),
            readiness,
            status,
            reason,
            message,
        }))
        .await
        .map_err(|_error| YellowstoneGrpcError::QueueClosed)
}

fn queue_provider_slot_health(
    source: &ProviderSourceIdentity,
    readiness: ProviderSourceReadiness,
    sender: &ProviderStreamSender,
    status: ProviderSourceHealthStatus,
    reason: ProviderSourceHealthReason,
    message: String,
) -> Result<Option<ProviderStreamUpdate>, YellowstoneGrpcError> {
    queue_provider_health_update(
        source,
        readiness,
        sender,
        status,
        reason,
        message,
        YellowstoneGrpcError::QueueClosed,
    )
}

fn queue_provider_health_update<E>(
    source: &ProviderSourceIdentity,
    readiness: ProviderSourceReadiness,
    sender: &ProviderStreamSender,
    status: ProviderSourceHealthStatus,
    reason: ProviderSourceHealthReason,
    message: String,
    queue_closed: E,
) -> Result<Option<ProviderStreamUpdate>, E> {
    let update = ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
        source: source.clone(),
        readiness,
        status,
        reason,
        message,
    });
    match sender.try_send(update) {
        Ok(()) => Ok(None),
        Err(mpsc::error::TrySendError::Closed(_update)) => Err(queue_closed),
        Err(mpsc::error::TrySendError::Full(update)) => Ok(Some(update)),
    }
}

const fn yellowstone_health_reason(error: &YellowstoneGrpcError) -> ProviderSourceHealthReason {
    match error {
        YellowstoneGrpcError::Build(_)
        | YellowstoneGrpcError::Client(_)
        | YellowstoneGrpcError::Status(_) => ProviderSourceHealthReason::UpstreamTransportFailure,
        YellowstoneGrpcError::Config(_)
        | YellowstoneGrpcError::Convert(_)
        | YellowstoneGrpcError::Protocol(_)
        | YellowstoneGrpcError::DuplicateSourceIdentity(_) => {
            ProviderSourceHealthReason::UpstreamProtocolFailure
        }
        YellowstoneGrpcError::QueueClosed => ProviderSourceHealthReason::UpstreamProtocolFailure,
    }
}

const PROVIDER_SUBSCRIPTION_ACKNOWLEDGED: &str = "subscription acknowledged";

fn transaction_event_from_update(
    slot: u64,
    transaction: Option<yellowstone_grpc_proto::prelude::SubscribeUpdateTransactionInfo>,
    commitment_status: TxCommitmentStatus,
    watermarks: ProviderCommitmentWatermarks,
) -> Result<TransactionEvent, YellowstoneGrpcError> {
    let transaction =
        transaction.ok_or(YellowstoneGrpcError::Convert("missing transaction payload"))?;
    let is_vote = transaction.is_vote;
    let signature = signature_bytes_from_slice(transaction.signature.as_slice(), || {
        YellowstoneGrpcError::Convert("invalid signature")
    })?;
    let tx = convert_transaction(
        transaction
            .transaction
            .ok_or(YellowstoneGrpcError::Convert(
                "missing versioned transaction",
            ))?,
        Some(signature),
    )?;
    Ok(TransactionEvent {
        slot,
        commitment_status,
        confirmed_slot: watermarks.confirmed_slot,
        finalized_slot: watermarks.finalized_slot,
        signature: Some(signature),
        provider_source: None,
        kind: if is_vote {
            TxKind::VoteOnly
        } else {
            classify_provider_transaction_kind(&tx)
        },
        tx: Arc::new(tx),
    })
}

fn transaction_status_event_from_update(
    commitment_status: TxCommitmentStatus,
    watermarks: ProviderCommitmentWatermarks,
    update: yellowstone_grpc_proto::prelude::SubscribeUpdateTransactionStatus,
) -> Result<TransactionStatusEvent, YellowstoneGrpcError> {
    let signature = signature_bytes_from_slice(update.signature.as_slice(), || {
        YellowstoneGrpcError::Convert("invalid transaction-status signature")
    })?;
    Ok(TransactionStatusEvent {
        slot: update.slot,
        commitment_status,
        confirmed_slot: watermarks.confirmed_slot,
        finalized_slot: watermarks.finalized_slot,
        signature,
        is_vote: update.is_vote,
        index: Some(update.index),
        err: update.err.map(|error| format!("{error:?}")),
        provider_source: None,
    })
}

fn account_update_event_from_yellowstone(
    update: yellowstone_grpc_proto::prelude::SubscribeUpdateAccount,
    commitment_status: TxCommitmentStatus,
    watermarks: ProviderCommitmentWatermarks,
) -> Result<AccountUpdateEvent, YellowstoneGrpcError> {
    let account = update
        .account
        .ok_or(YellowstoneGrpcError::Convert("missing account payload"))?;
    let pubkey = pubkey_bytes_from_slice(account.pubkey.as_slice(), || {
        YellowstoneGrpcError::Convert("invalid account pubkey")
    })?;
    let owner = pubkey_bytes_from_slice(account.owner.as_slice(), || {
        YellowstoneGrpcError::Convert("invalid account owner")
    })?;
    let txn_signature = match account.txn_signature {
        Some(signature) => Some(signature_bytes_from_slice(signature.as_slice(), || {
            YellowstoneGrpcError::Convert("invalid account txn signature")
        })?),
        None => None,
    };
    if account.data.len() > MAX_ACCOUNT_DATA_LEN {
        return Err(YellowstoneGrpcError::Convert(
            "account data exceeds max permitted size",
        ));
    }
    Ok(AccountUpdateEvent {
        slot: update.slot,
        commitment_status,
        confirmed_slot: watermarks.confirmed_slot,
        finalized_slot: watermarks.finalized_slot,
        pubkey,
        owner,
        lamports: account.lamports,
        executable: account.executable,
        rent_epoch: account.rent_epoch,
        data: account.data.into(),
        write_version: Some(account.write_version),
        txn_signature,
        is_startup: update.is_startup,
        matched_filter: None,
        provider_source: None,
    })
}

fn block_meta_event_from_update(
    commitment_status: TxCommitmentStatus,
    watermarks: ProviderCommitmentWatermarks,
    update: &yellowstone_grpc_proto::prelude::SubscribeUpdateBlockMeta,
) -> Result<BlockMetaEvent, YellowstoneGrpcError> {
    let blockhash = Hash::from_str(&update.blockhash)
        .map_err(|_error| YellowstoneGrpcError::Convert("invalid block-meta blockhash"))?;
    let parent_blockhash = Hash::from_str(&update.parent_blockhash)
        .map_err(|_error| YellowstoneGrpcError::Convert("invalid parent blockhash"))?;
    Ok(BlockMetaEvent {
        slot: update.slot,
        commitment_status,
        confirmed_slot: watermarks.confirmed_slot,
        finalized_slot: watermarks.finalized_slot,
        blockhash: blockhash.to_bytes(),
        parent_slot: update.parent_slot,
        parent_blockhash: parent_blockhash.to_bytes(),
        block_time: update.block_time.map(|value| value.timestamp),
        block_height: update.block_height.map(|value| value.block_height),
        executed_transaction_count: update.executed_transaction_count,
        entries_count: update.entries_count,
        provider_source: None,
    })
}

fn observe_non_transaction_commitment(
    watermarks: &mut ProviderCommitmentWatermarks,
    slot: u64,
    commitment_status: TxCommitmentStatus,
) {
    match commitment_status {
        TxCommitmentStatus::Processed => {}
        TxCommitmentStatus::Confirmed => watermarks.observe_confirmed_slot(slot),
        TxCommitmentStatus::Finalized => watermarks.observe_finalized_slot(slot),
    }
}

fn slot_status_event_from_update(
    slot: u64,
    parent_slot: Option<u64>,
    status: Option<SlotStatus>,
    watermarks: &mut ProviderCommitmentWatermarks,
    slot_states: &mut HashMap<u64, ForkSlotStatus>,
) -> Option<SlotStatusEvent> {
    let mapped = match status? {
        SlotStatus::SlotConfirmed => {
            watermarks.observe_confirmed_slot(slot);
            ForkSlotStatus::Confirmed
        }
        SlotStatus::SlotFinalized => {
            watermarks.observe_finalized_slot(slot);
            ForkSlotStatus::Finalized
        }
        SlotStatus::SlotDead => ForkSlotStatus::Orphaned,
        SlotStatus::SlotProcessed
        | SlotStatus::SlotFirstShredReceived
        | SlotStatus::SlotCompleted
        | SlotStatus::SlotCreatedBank => ForkSlotStatus::Processed,
    };
    let previous_status = slot_states.insert(slot, mapped);
    prune_recent_slots(
        slot_states,
        slot,
        SLOT_STATUS_RETAINED_LAG,
        SLOT_STATUS_PRUNE_THRESHOLD,
    );
    if previous_status == Some(mapped) {
        return None;
    }
    Some(SlotStatusEvent {
        slot,
        parent_slot,
        previous_status,
        status: mapped,
        tip_slot: None,
        confirmed_slot: watermarks.confirmed_slot,
        finalized_slot: watermarks.finalized_slot,
        provider_source: None,
    })
}

impl ProviderStreamFanIn {
    /// Spawns one Yellowstone gRPC primary source into this fan-in.
    ///
    /// # Errors
    ///
    /// Returns an error if the selected config is invalid or the source cannot
    /// be started.
    pub async fn spawn_yellowstone_grpc_source(
        &self,
        config: YellowstoneGrpcConfig,
    ) -> Result<JoinHandle<Result<(), YellowstoneGrpcError>>, YellowstoneGrpcError> {
        let reservation = match config.source_identity() {
            Some(source) => Some(Arc::new(self.reserve_source_identity(source)?)),
            None => None,
        };
        spawn_yellowstone_grpc_source_inner(config, self.sender(), reservation).await
    }

    #[deprecated(note = "use spawn_yellowstone_grpc_source")]
    /// Deprecated compatibility shim for [`ProviderStreamFanIn::spawn_yellowstone_grpc_source`].
    ///
    /// # Errors
    ///
    /// Returns the same source-registration or startup error as
    /// [`ProviderStreamFanIn::spawn_yellowstone_grpc_source`].
    pub async fn spawn_yellowstone_grpc_transaction_source(
        &self,
        config: YellowstoneGrpcConfig,
    ) -> Result<JoinHandle<Result<(), YellowstoneGrpcError>>, YellowstoneGrpcError> {
        self.spawn_yellowstone_grpc_source(config).await
    }

    /// Spawns one Yellowstone gRPC slot source into this fan-in.
    ///
    /// # Errors
    ///
    /// Returns an error if the source cannot be started.
    pub async fn spawn_yellowstone_grpc_slot_source(
        &self,
        config: YellowstoneGrpcSlotsConfig,
    ) -> Result<JoinHandle<Result<(), YellowstoneGrpcError>>, YellowstoneGrpcError> {
        let reservation = match config.source_identity() {
            Some(source) => Some(Arc::new(self.reserve_source_identity(source)?)),
            None => None,
        };
        spawn_yellowstone_grpc_slot_source_inner(config, self.sender(), reservation).await
    }
}

#[inline(always)]
fn convert_transaction(
    tx: yellowstone_grpc_proto::prelude::Transaction,
    first_signature: Option<SignatureBytes>,
) -> Result<VersionedTransaction, YellowstoneGrpcError> {
    let mut signatures = Vec::with_capacity(tx.signatures.len());
    for (index, signature) in tx.signatures.into_iter().enumerate() {
        if index == 0
            && let Some(first_signature) = first_signature
        {
            signatures.push(first_signature.into());
            continue;
        }
        signatures.push(
            signature_bytes_from_slice(signature.as_slice(), || {
                YellowstoneGrpcError::Convert("failed to parse transaction signature")
            })?
            .into(),
        );
    }
    let message = convert_message(
        tx.message
            .ok_or(YellowstoneGrpcError::Convert("missing transaction message"))?,
    )?;
    Ok(VersionedTransaction {
        signatures,
        message,
    })
}

#[inline(always)]
fn convert_message(
    message: yellowstone_grpc_proto::prelude::Message,
) -> Result<VersionedMessage, YellowstoneGrpcError> {
    let header = message
        .header
        .ok_or(YellowstoneGrpcError::Convert("missing message header"))?;
    let header = MessageHeader {
        num_required_signatures: u8::try_from(header.num_required_signatures)
            .map_err(|_error| YellowstoneGrpcError::Convert("invalid num_required_signatures"))?,
        num_readonly_signed_accounts: u8::try_from(header.num_readonly_signed_accounts).map_err(
            |_error| YellowstoneGrpcError::Convert("invalid num_readonly_signed_accounts"),
        )?,
        num_readonly_unsigned_accounts: u8::try_from(header.num_readonly_unsigned_accounts)
            .map_err(|_error| {
                YellowstoneGrpcError::Convert("invalid num_readonly_unsigned_accounts")
            })?,
    };
    let mut account_keys = Vec::with_capacity(message.account_keys.len());
    for key in message.account_keys {
        account_keys.push(
            pubkey_bytes_from_slice(key.as_slice(), || {
                YellowstoneGrpcError::Convert("invalid account key")
            })?
            .into(),
        );
    }
    let recent_blockhash = <[u8; 32]>::try_from(message.recent_blockhash.as_slice())
        .map(Hash::new_from_array)
        .map_err(|_error| YellowstoneGrpcError::Convert("invalid recent blockhash"))?;
    let mut instructions = Vec::with_capacity(message.instructions.len());
    for instruction in message.instructions {
        instructions.push(CompiledInstruction {
            program_id_index: u8::try_from(instruction.program_id_index).map_err(|_error| {
                YellowstoneGrpcError::Convert("invalid compiled instruction program id index")
            })?,
            accounts: instruction.accounts,
            data: instruction.data,
        });
    }
    if message.versioned {
        let mut address_table_lookups = Vec::with_capacity(message.address_table_lookups.len());
        for lookup in message.address_table_lookups {
            address_table_lookups.push(MessageAddressTableLookup {
                account_key: pubkey_bytes_from_slice(lookup.account_key.as_slice(), || {
                    YellowstoneGrpcError::Convert("invalid address table account key")
                })?
                .into(),
                writable_indexes: lookup.writable_indexes,
                readonly_indexes: lookup.readonly_indexes,
            });
        }
        Ok(VersionedMessage::V0(MessageV0 {
            header,
            account_keys,
            recent_blockhash,
            instructions,
            address_table_lookups,
        }))
    } else {
        Ok(VersionedMessage::Legacy(Message {
            header,
            account_keys,
            recent_blockhash,
            instructions,
        }))
    }
}

#[cfg(all(test, feature = "provider-grpc"))]
#[allow(
    clippy::let_underscore_must_use,
    clippy::shadow_unrelated,
    clippy::wildcard_enum_match_arm
)]
mod tests {
    use super::*;
    use crate::{
        event::TxKind, framework::signature_bytes, provider_stream::create_provider_stream_queue,
    };
    use futures_channel::mpsc as futures_mpsc;
    use futures_util::stream::{self, Stream};
    use solana_instruction::Instruction;
    use solana_keypair::Keypair;
    use solana_message::{Message as SolanaMessage, VersionedMessage};
    use solana_sdk_ids::system_program;
    use solana_sdk_ids::{compute_budget, vote};
    use solana_signer::Signer;
    use std::{pin::Pin, time::Instant};

    use sof_support::bench::{avg_ns_per_iteration, profile_iterations};
    use tokio::sync::oneshot;
    use tokio::time::{Duration, timeout};
    use yellowstone_grpc_proto::geyser::geyser_server::{Geyser, GeyserServer};
    use yellowstone_grpc_proto::prelude::{
        CompiledInstruction as ProtoCompiledInstruction, GetBlockHeightRequest,
        GetBlockHeightResponse, GetLatestBlockhashRequest, GetLatestBlockhashResponse,
        GetSlotRequest, GetSlotResponse, GetVersionRequest, GetVersionResponse,
        IsBlockhashValidRequest, IsBlockhashValidResponse, Message as ProtoMessage,
        MessageAddressTableLookup as ProtoMessageAddressTableLookup,
        MessageHeader as ProtoMessageHeader, PingRequest, PongResponse, SubscribeDeshredRequest,
        SubscribeReplayInfoRequest, SubscribeReplayInfoResponse, SubscribeRequest, SubscribeUpdate,
        SubscribeUpdateAccount, SubscribeUpdateAccountInfo, SubscribeUpdateDeshred,
        SubscribeUpdateTransactionInfo, SubscribeUpdateTransactionStatus, Transaction,
    };
    use yellowstone_grpc_proto::tonic::{self, Request, Response, Status, transport::Server};

    #[test]
    fn yellowstone_account_update_rejects_oversized_data() {
        let pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let update = sample_account_update(92, pubkey, owner);
        let account_update = match update.update_oneof {
            Some(UpdateOneof::Account(account_update)) => account_update,
            other => panic!("expected account update, got {other:?}"),
        };

        let mut oversized = account_update;
        oversized.account.as_mut().expect("account payload").data =
            vec![7_u8; MAX_ACCOUNT_DATA_LEN + 1];

        let error = account_update_event_from_yellowstone(
            oversized,
            TxCommitmentStatus::Confirmed,
            ProviderCommitmentWatermarks::default(),
        )
        .expect_err("oversized account payload must fail");

        assert!(matches!(
            error,
            YellowstoneGrpcError::Convert("account data exceeds max permitted size")
        ));
    }

    #[test]
    fn yellowstone_slot_state_pruning_evicts_old_slots() {
        let mut slot_states = HashMap::new();
        for slot in 0..=u64::try_from(SLOT_STATUS_PRUNE_THRESHOLD).unwrap_or(u64::MAX) {
            let _ = slot_states.insert(slot, ForkSlotStatus::Processed);
        }

        prune_recent_slots(
            &mut slot_states,
            10_000,
            SLOT_STATUS_RETAINED_LAG,
            SLOT_STATUS_PRUNE_THRESHOLD,
        );

        assert!(
            !slot_states.contains_key(&0),
            "old tracked slots should be pruned"
        );
        assert!(
            slot_states.contains_key(&10_000_u64.saturating_sub(SLOT_STATUS_RETAINED_LAG)),
            "recent tracked slots should stay resident"
        );
        assert!(
            slot_states.len()
                <= usize::try_from(SLOT_STATUS_RETAINED_LAG + 1).unwrap_or(usize::MAX),
            "tracked slot state should stay bounded"
        );
    }

    fn sample_transaction() -> VersionedTransaction {
        let signer = Keypair::new();
        let instructions = [
            Instruction::new_with_bytes(vote::id(), &[], vec![]),
            Instruction::new_with_bytes(system_program::id(), &[], vec![]),
            Instruction::new_with_bytes(compute_budget::id(), &[], vec![]),
        ];
        let message = SolanaMessage::new(&instructions, Some(&signer.pubkey()));
        VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[&signer]).expect("tx")
    }

    fn sample_vote_transaction() -> VersionedTransaction {
        let signer = Keypair::new();
        let instructions = [
            Instruction::new_with_bytes(vote::id(), &[], vec![]),
            Instruction::new_with_bytes(compute_budget::id(), &[], vec![]),
        ];
        let message = SolanaMessage::new(&instructions, Some(&signer.pubkey()));
        VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[&signer]).expect("tx")
    }

    #[test]
    fn yellowstone_config_defaults_do_not_filter_vote_or_failed() {
        let request = YellowstoneGrpcConfig::new("http://127.0.0.1:10000").subscribe_request();
        let filter = request.transactions.get("sof").expect("sof filter");
        assert_eq!(filter.vote, None);
        assert_eq!(filter.failed, None);
    }

    #[test]
    fn yellowstone_subscribe_request_tracks_slots_and_replay_cursor() {
        let request = YellowstoneGrpcConfig::new("http://127.0.0.1:10000")
            .with_replay_mode(ProviderReplayMode::FromSlot(200))
            .with_commitment(YellowstoneGrpcCommitment::Processed)
            .subscribe_request_with_state(0);
        assert!(request.slots.contains_key(INTERNAL_SLOT_FILTER));
        assert_eq!(request.from_slot, Some(200));
    }

    #[test]
    fn yellowstone_subscribe_request_can_target_transaction_status() {
        let request = YellowstoneGrpcConfig::new("http://127.0.0.1:10000")
            .with_stream(YellowstoneGrpcStream::TransactionStatus)
            .subscribe_request_with_state(0);
        assert!(request.transactions.is_empty());
        assert!(request.transactions_status.contains_key("sof"));
        assert!(request.slots.contains_key(INTERNAL_SLOT_FILTER));
    }

    #[test]
    fn yellowstone_subscribe_request_can_target_accounts() {
        let key = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let request = YellowstoneGrpcConfig::new("http://127.0.0.1:10000")
            .with_stream(YellowstoneGrpcStream::Accounts)
            .with_accounts([key])
            .with_owners([owner])
            .require_transaction_signature()
            .subscribe_request_with_state(0);
        let filter = request.accounts.get("sof").expect("accounts filter");
        assert_eq!(filter.account, vec![key.to_string()]);
        assert_eq!(filter.owner, vec![owner.to_string()]);
        assert_eq!(filter.nonempty_txn_signature, Some(true));
        assert!(request.slots.contains_key(INTERNAL_SLOT_FILTER));
    }

    #[test]
    fn yellowstone_account_stream_defaults_to_finalized_commitment() {
        let request = YellowstoneGrpcConfig::new("http://127.0.0.1:10000")
            .with_stream(YellowstoneGrpcStream::Accounts)
            .subscribe_request_with_state(0);
        assert_eq!(request.commitment, Some(CommitmentLevel::Finalized as i32));
    }

    #[test]
    fn yellowstone_account_stream_preserves_explicit_processed_commitment() {
        let request = YellowstoneGrpcConfig::new("http://127.0.0.1:10000")
            .with_commitment(YellowstoneGrpcCommitment::Processed)
            .with_stream(YellowstoneGrpcStream::Accounts)
            .subscribe_request_with_state(0);
        assert_eq!(request.commitment, Some(CommitmentLevel::Processed as i32));
    }

    #[test]
    fn yellowstone_subscribe_request_can_target_block_meta() {
        let request = YellowstoneGrpcConfig::new("http://127.0.0.1:10000")
            .with_stream(YellowstoneGrpcStream::BlockMeta)
            .subscribe_request_with_state(0);
        assert!(request.transactions.is_empty());
        assert!(request.accounts.is_empty());
        assert!(request.transactions_status.is_empty());
        assert!(request.blocks_meta.contains_key("sof"));
        assert!(request.slots.contains_key(INTERNAL_SLOT_FILTER));
    }

    #[test]
    fn yellowstone_slot_subscribe_request_tracks_slots_and_replay_cursor() {
        let request = YellowstoneGrpcSlotsConfig::new("http://127.0.0.1:10000")
            .with_replay_mode(ProviderReplayMode::FromSlot(200))
            .with_commitment(YellowstoneGrpcCommitment::Processed)
            .subscribe_request_with_state(250);
        assert!(request.slots.contains_key("sof"));
        assert_eq!(request.from_slot, Some(219));
    }

    #[test]
    fn yellowstone_from_slot_reconnect_resumes_from_tracked_slot() {
        let request = YellowstoneGrpcConfig::new("http://127.0.0.1:10000")
            .with_replay_mode(ProviderReplayMode::FromSlot(200))
            .with_commitment(YellowstoneGrpcCommitment::Processed)
            .subscribe_request_with_state(250);
        assert_eq!(request.from_slot, Some(219));
    }

    #[test]
    fn yellowstone_live_mode_starts_at_stream_head() {
        let request = YellowstoneGrpcConfig::new("http://127.0.0.1:10000")
            .with_replay_mode(ProviderReplayMode::Live)
            .subscribe_request_with_state(500);
        assert_eq!(request.from_slot, None);
    }

    #[tokio::test]
    async fn yellowstone_local_source_delivers_transaction_update() {
        let update = SubscribeUpdate {
            filters: vec!["sof".to_owned()],
            created_at: None,
            update_oneof: Some(UpdateOneof::Transaction(
                yellowstone_grpc_proto::prelude::SubscribeUpdateTransaction {
                    transaction: Some(sample_update()),
                    slot: 91,
                },
            )),
        };
        let (addr, shutdown_tx, server) = spawn_yellowstone_test_server(MockYellowstone {
            expected_stream: MockYellowstoneStream::Transaction,
            expected_account: None,
            expected_owner: None,
            update,
        })
        .await;

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = YellowstoneGrpcConfig::new(format!("http://{addr}"))
            .with_stream(YellowstoneGrpcStream::Transaction)
            .with_source_instance("yellowstone-primary")
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10))
            .with_connect_timeout(Duration::from_secs(2));
        let handle = spawn_yellowstone_grpc_source(config, tx)
            .await
            .expect("spawn Yellowstone transaction source");

        let event = loop {
            let update = timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("provider update timeout")
                .expect("provider update");
            match update {
                ProviderStreamUpdate::Transaction(event) => break event,
                ProviderStreamUpdate::Health(_) => continue,
                other => panic!("expected transaction update, got {other:?}"),
            }
        };
        assert_eq!(event.slot, 91);
        assert_eq!(event.kind, TxKind::Mixed);
        assert!(event.signature.is_some());
        assert_eq!(
            event
                .provider_source
                .as_deref()
                .expect("provider source")
                .instance_str(),
            "yellowstone-primary"
        );

        handle.abort();
        handle.await.ok();
        let _ = shutdown_tx.send(());
        server.await.expect("Yellowstone server task");
    }

    #[tokio::test]
    async fn yellowstone_source_emits_initial_health_registration() {
        let update = SubscribeUpdate {
            filters: vec!["sof".to_owned()],
            created_at: None,
            update_oneof: Some(UpdateOneof::Ping(
                yellowstone_grpc_proto::prelude::SubscribeUpdatePing {},
            )),
        };
        let (addr, shutdown_tx, server) = spawn_yellowstone_test_server(MockYellowstone {
            expected_stream: MockYellowstoneStream::Transaction,
            expected_account: None,
            expected_owner: None,
            update,
        })
        .await;

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = YellowstoneGrpcConfig::new(format!("http://{addr}"))
            .with_stream(YellowstoneGrpcStream::Transaction)
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10))
            .with_connect_timeout(Duration::from_secs(2));
        let handle = spawn_yellowstone_grpc_source(config, tx)
            .await
            .expect("spawn Yellowstone transaction source");

        let update = timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("provider update timeout")
            .expect("provider update");
        let ProviderStreamUpdate::Health(event) = update else {
            panic!("expected initial provider health update");
        };
        assert_eq!(event.status, ProviderSourceHealthStatus::Reconnecting);
        assert_eq!(
            event.reason,
            ProviderSourceHealthReason::InitialConnectPending
        );

        handle.abort();
        handle.await.ok();
        let _ = shutdown_tx.send(());
        server.await.expect("Yellowstone server task");
    }

    #[tokio::test]
    async fn yellowstone_local_source_delivers_transaction_status_update() {
        let update = sample_status_update(91);
        let (addr, shutdown_tx, server) = spawn_yellowstone_test_server(MockYellowstone {
            expected_stream: MockYellowstoneStream::TransactionStatus,
            expected_account: None,
            expected_owner: None,
            update,
        })
        .await;

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = YellowstoneGrpcConfig::new(format!("http://{addr}"))
            .with_stream(YellowstoneGrpcStream::TransactionStatus)
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10))
            .with_connect_timeout(Duration::from_secs(2));
        let handle = spawn_yellowstone_grpc_source(config, tx)
            .await
            .expect("spawn Yellowstone status source");

        let event = loop {
            let update = timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("provider update timeout")
                .expect("provider update");
            match update {
                ProviderStreamUpdate::TransactionStatus(event) => break event,
                ProviderStreamUpdate::Health(_) => continue,
                other => panic!("expected transaction-status update, got {other:?}"),
            }
        };
        assert_eq!(event.slot, 91);
        assert!(!event.is_vote);
        assert_eq!(event.index, Some(7));

        handle.abort();
        handle.await.ok();
        let _ = shutdown_tx.send(());
        server.await.expect("Yellowstone server task");
    }

    #[tokio::test]
    async fn yellowstone_local_source_delivers_account_update() {
        let pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let update = sample_account_update(92, pubkey, owner);
        let (addr, shutdown_tx, server) = spawn_yellowstone_test_server(MockYellowstone {
            expected_stream: MockYellowstoneStream::Accounts,
            expected_account: Some(pubkey.to_string()),
            expected_owner: Some(owner.to_string()),
            update,
        })
        .await;

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = YellowstoneGrpcConfig::new(format!("http://{addr}"))
            .with_stream(YellowstoneGrpcStream::Accounts)
            .with_accounts([pubkey])
            .with_owners([owner])
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10))
            .with_connect_timeout(Duration::from_secs(2));
        let handle = spawn_yellowstone_grpc_source(config, tx)
            .await
            .expect("spawn Yellowstone account source");

        let event = loop {
            let update = timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("provider update timeout")
                .expect("provider update");
            match update {
                ProviderStreamUpdate::AccountUpdate(event) => break event,
                ProviderStreamUpdate::Health(_) => continue,
                other => panic!("expected account update, got {other:?}"),
            }
        };
        assert_eq!(event.slot, 92);
        assert_eq!(event.pubkey, pubkey.into());
        assert_eq!(event.owner, owner.into());
        assert_eq!(event.lamports, 42);
        assert_eq!(event.data.as_ref(), &[1, 2, 3, 4]);

        handle.abort();
        handle.await.ok();
        let _ = shutdown_tx.send(());
        server.await.expect("Yellowstone server task");
    }

    #[tokio::test]
    async fn yellowstone_local_slot_source_delivers_slot_status_update() {
        let update = SubscribeUpdate {
            filters: vec!["sof".to_owned()],
            created_at: None,
            update_oneof: Some(UpdateOneof::Slot(
                yellowstone_grpc_proto::prelude::SubscribeUpdateSlot {
                    slot: 93,
                    parent: Some(92),
                    status: SlotStatus::SlotConfirmed as i32,
                    dead_error: Some(String::new()),
                },
            )),
        };
        let (addr, shutdown_tx, server) = spawn_yellowstone_test_server(MockYellowstone {
            expected_stream: MockYellowstoneStream::Slots,
            expected_account: None,
            expected_owner: None,
            update,
        })
        .await;

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = YellowstoneGrpcSlotsConfig::new(format!("http://{addr}"))
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10))
            .with_connect_timeout(Duration::from_secs(2));
        let handle = spawn_yellowstone_grpc_slot_source(config, tx)
            .await
            .expect("spawn Yellowstone slot source");

        let event = loop {
            let update = timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("provider update timeout")
                .expect("provider update");
            match update {
                ProviderStreamUpdate::SlotStatus(event) => break event,
                ProviderStreamUpdate::Health(_) => continue,
                other => panic!("expected slot-status update, got {other:?}"),
            }
        };
        assert_eq!(event.slot, 93);
        assert_eq!(event.parent_slot, Some(92));
        assert_eq!(event.status, ForkSlotStatus::Confirmed);
        assert_eq!(event.confirmed_slot, Some(93));

        handle.abort();
        handle.await.ok();
        let _ = shutdown_tx.send(());
        server.await.expect("Yellowstone server task");
    }

    #[tokio::test]
    async fn yellowstone_local_source_delivers_block_meta_update() {
        let update = sample_block_meta_update(94);
        let (addr, shutdown_tx, server) = spawn_yellowstone_test_server(MockYellowstone {
            expected_stream: MockYellowstoneStream::BlockMeta,
            expected_account: None,
            expected_owner: None,
            update,
        })
        .await;

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = YellowstoneGrpcConfig::new(format!("http://{addr}"))
            .with_stream(YellowstoneGrpcStream::BlockMeta)
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10))
            .with_connect_timeout(Duration::from_secs(2));
        let handle = spawn_yellowstone_grpc_source(config, tx)
            .await
            .expect("spawn Yellowstone block-meta source");

        let event = loop {
            let update = timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("provider update timeout")
                .expect("provider update");
            match update {
                ProviderStreamUpdate::BlockMeta(event) => break event,
                ProviderStreamUpdate::Health(_) => continue,
                other => panic!("expected block-meta update, got {other:?}"),
            }
        };
        assert_eq!(event.slot, 94);
        assert_eq!(event.parent_slot, 93);
        assert_eq!(event.block_time, Some(1_700_000_094));
        assert_eq!(event.block_height, Some(9_094));
        assert_eq!(event.executed_transaction_count, 12);
        assert_eq!(event.entries_count, 5);

        handle.abort();
        handle.await.ok();
        let _ = shutdown_tx.send(());
        server.await.expect("Yellowstone server task");
    }

    fn proto_transaction_from_versioned(tx: &VersionedTransaction) -> Transaction {
        let message = match &tx.message {
            VersionedMessage::Legacy(message) => ProtoMessage {
                header: Some(ProtoMessageHeader {
                    num_required_signatures: u32::from(message.header.num_required_signatures),
                    num_readonly_signed_accounts: u32::from(
                        message.header.num_readonly_signed_accounts,
                    ),
                    num_readonly_unsigned_accounts: u32::from(
                        message.header.num_readonly_unsigned_accounts,
                    ),
                }),
                account_keys: message
                    .account_keys
                    .iter()
                    .map(|key| key.to_bytes().to_vec())
                    .collect(),
                recent_blockhash: message.recent_blockhash.to_bytes().to_vec(),
                instructions: message
                    .instructions
                    .iter()
                    .map(|instruction| ProtoCompiledInstruction {
                        program_id_index: u32::from(instruction.program_id_index),
                        accounts: instruction.accounts.clone(),
                        data: instruction.data.clone(),
                    })
                    .collect(),
                versioned: false,
                address_table_lookups: Vec::new(),
            },
            VersionedMessage::V0(message) => ProtoMessage {
                header: Some(ProtoMessageHeader {
                    num_required_signatures: u32::from(message.header.num_required_signatures),
                    num_readonly_signed_accounts: u32::from(
                        message.header.num_readonly_signed_accounts,
                    ),
                    num_readonly_unsigned_accounts: u32::from(
                        message.header.num_readonly_unsigned_accounts,
                    ),
                }),
                account_keys: message
                    .account_keys
                    .iter()
                    .map(|key| key.to_bytes().to_vec())
                    .collect(),
                recent_blockhash: message.recent_blockhash.to_bytes().to_vec(),
                instructions: message
                    .instructions
                    .iter()
                    .map(|instruction| ProtoCompiledInstruction {
                        program_id_index: u32::from(instruction.program_id_index),
                        accounts: instruction.accounts.clone(),
                        data: instruction.data.clone(),
                    })
                    .collect(),
                versioned: true,
                address_table_lookups: message
                    .address_table_lookups
                    .iter()
                    .map(|lookup| ProtoMessageAddressTableLookup {
                        account_key: lookup.account_key.to_bytes().to_vec(),
                        writable_indexes: lookup.writable_indexes.clone(),
                        readonly_indexes: lookup.readonly_indexes.clone(),
                    })
                    .collect(),
            },
        };
        Transaction {
            signatures: tx
                .signatures
                .iter()
                .map(|sig| sig.as_ref().to_vec())
                .collect(),
            message: Some(message),
        }
    }

    fn sample_update() -> SubscribeUpdateTransactionInfo {
        let tx = sample_transaction();
        SubscribeUpdateTransactionInfo {
            signature: tx.signatures.first().expect("signature").as_ref().to_vec(),
            is_vote: false,
            transaction: Some(proto_transaction_from_versioned(&tx)),
            meta: None,
            index: 0,
        }
    }

    fn sample_vote_update() -> SubscribeUpdateTransactionInfo {
        let tx = sample_vote_transaction();
        SubscribeUpdateTransactionInfo {
            signature: tx.signatures.first().expect("signature").as_ref().to_vec(),
            is_vote: true,
            transaction: Some(proto_transaction_from_versioned(&tx)),
            meta: None,
            index: 0,
        }
    }

    fn sample_status_update(slot: u64) -> SubscribeUpdate {
        let tx = sample_transaction();
        SubscribeUpdate {
            filters: vec!["sof".to_owned()],
            created_at: None,
            update_oneof: Some(UpdateOneof::TransactionStatus(
                SubscribeUpdateTransactionStatus {
                    slot,
                    signature: tx.signatures.first().expect("signature").as_ref().to_vec(),
                    is_vote: false,
                    index: 7,
                    err: None,
                },
            )),
        }
    }

    fn sample_account_update(slot: u64, pubkey: Pubkey, owner: Pubkey) -> SubscribeUpdate {
        SubscribeUpdate {
            filters: vec!["sof".to_owned()],
            created_at: None,
            update_oneof: Some(UpdateOneof::Account(SubscribeUpdateAccount {
                slot,
                is_startup: false,
                account: Some(SubscribeUpdateAccountInfo {
                    pubkey: pubkey.to_bytes().to_vec(),
                    lamports: 42,
                    owner: owner.to_bytes().to_vec(),
                    executable: false,
                    rent_epoch: 9,
                    data: vec![1, 2, 3, 4],
                    write_version: 11,
                    txn_signature: None,
                }),
            })),
        }
    }

    fn sample_block_meta_update(slot: u64) -> SubscribeUpdate {
        SubscribeUpdate {
            filters: vec!["sof".to_owned()],
            created_at: None,
            update_oneof: Some(UpdateOneof::BlockMeta(
                yellowstone_grpc_proto::prelude::SubscribeUpdateBlockMeta {
                    slot,
                    blockhash: Hash::new_unique().to_string(),
                    rewards: None,
                    block_time: Some(yellowstone_grpc_proto::prelude::UnixTimestamp {
                        timestamp: 1_700_000_000 + i64::try_from(slot).unwrap_or(0),
                    }),
                    block_height: Some(yellowstone_grpc_proto::prelude::BlockHeight {
                        block_height: 9_000 + slot,
                    }),
                    parent_slot: slot.saturating_sub(1),
                    parent_blockhash: Hash::new_unique().to_string(),
                    executed_transaction_count: 12,
                    entries_count: 5,
                },
            )),
        }
    }

    #[derive(Clone, Copy)]
    enum MockYellowstoneStream {
        Transaction,
        TransactionStatus,
        Accounts,
        BlockMeta,
        Slots,
    }

    #[derive(Clone)]
    struct MockYellowstone {
        expected_stream: MockYellowstoneStream,
        expected_account: Option<String>,
        expected_owner: Option<String>,
        update: SubscribeUpdate,
    }

    #[async_trait::async_trait]
    impl Geyser for MockYellowstone {
        type SubscribeStream =
            Pin<Box<dyn Stream<Item = Result<SubscribeUpdate, Status>> + Send + 'static>>;
        type SubscribeDeshredStream =
            Pin<Box<dyn Stream<Item = Result<SubscribeUpdateDeshred, Status>> + Send + 'static>>;

        async fn subscribe(
            &self,
            request: Request<tonic::Streaming<SubscribeRequest>>,
        ) -> Result<Response<Self::SubscribeStream>, Status> {
            let mut inbound = request.into_inner();
            let first = inbound
                .message()
                .await?
                .ok_or_else(|| Status::invalid_argument("missing subscribe request"))?;
            match self.expected_stream {
                MockYellowstoneStream::Transaction => {
                    assert!(first.transactions.contains_key("sof"));
                }
                MockYellowstoneStream::TransactionStatus => {
                    assert!(first.transactions_status.contains_key("sof"));
                }
                MockYellowstoneStream::Accounts => {
                    let filter = first
                        .accounts
                        .get("sof")
                        .expect("expected Yellowstone accounts filter");
                    if let Some(account) = &self.expected_account {
                        assert!(filter.account.iter().any(|value| value == account));
                    }
                    if let Some(owner) = &self.expected_owner {
                        assert!(filter.owner.iter().any(|value| value == owner));
                    }
                }
                MockYellowstoneStream::BlockMeta => {
                    assert!(first.blocks_meta.contains_key("sof"));
                }
                MockYellowstoneStream::Slots => {
                    assert!(first.slots.contains_key("sof"));
                }
            }
            let (tx, rx) = futures_mpsc::unbounded();
            tx.unbounded_send(Ok(self.update.clone()))
                .expect("send Yellowstone test update");
            Ok(Response::new(Box::pin(rx)))
        }

        async fn subscribe_deshred(
            &self,
            _request: Request<tonic::Streaming<SubscribeDeshredRequest>>,
        ) -> Result<Response<Self::SubscribeDeshredStream>, Status> {
            Ok(Response::new(Box::pin(stream::empty())))
        }

        async fn subscribe_replay_info(
            &self,
            _request: Request<SubscribeReplayInfoRequest>,
        ) -> Result<Response<SubscribeReplayInfoResponse>, Status> {
            Ok(Response::new(SubscribeReplayInfoResponse::default()))
        }

        async fn ping(
            &self,
            _request: Request<PingRequest>,
        ) -> Result<Response<PongResponse>, Status> {
            Ok(Response::new(PongResponse::default()))
        }

        async fn get_latest_blockhash(
            &self,
            _request: Request<GetLatestBlockhashRequest>,
        ) -> Result<Response<GetLatestBlockhashResponse>, Status> {
            Ok(Response::new(GetLatestBlockhashResponse::default()))
        }

        async fn get_block_height(
            &self,
            _request: Request<GetBlockHeightRequest>,
        ) -> Result<Response<GetBlockHeightResponse>, Status> {
            Ok(Response::new(GetBlockHeightResponse::default()))
        }

        async fn get_slot(
            &self,
            _request: Request<GetSlotRequest>,
        ) -> Result<Response<GetSlotResponse>, Status> {
            Ok(Response::new(GetSlotResponse::default()))
        }

        async fn is_blockhash_valid(
            &self,
            _request: Request<IsBlockhashValidRequest>,
        ) -> Result<Response<IsBlockhashValidResponse>, Status> {
            Ok(Response::new(IsBlockhashValidResponse::default()))
        }

        async fn get_version(
            &self,
            _request: Request<GetVersionRequest>,
        ) -> Result<Response<GetVersionResponse>, Status> {
            Ok(Response::new(GetVersionResponse::default()))
        }
    }

    async fn spawn_yellowstone_test_server(
        service: MockYellowstone,
    ) -> (
        std::net::SocketAddr,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind Yellowstone test");
        let addr = listener.local_addr().expect("Yellowstone test addr");
        drop(listener);

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            Server::builder()
                .add_service(GeyserServer::new(service))
                .serve_with_shutdown(addr, async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("run Yellowstone test server");
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        (addr, shutdown_tx, handle)
    }

    fn transaction_event_from_update_baseline(
        slot: u64,
        transaction: Option<SubscribeUpdateTransactionInfo>,
        commitment_status: TxCommitmentStatus,
    ) -> Result<TransactionEvent, YellowstoneGrpcError> {
        let transaction =
            transaction.ok_or(YellowstoneGrpcError::Convert("missing transaction payload"))?;
        let signature = Signature::try_from(transaction.signature.as_slice())
            .map(signature_bytes)
            .map(Some)
            .map_err(|_error| YellowstoneGrpcError::Convert("invalid signature"))?;
        let tx = {
            let tx = transaction
                .transaction
                .ok_or(YellowstoneGrpcError::Convert(
                    "missing versioned transaction",
                ))?;
            let signatures = tx
                .signatures
                .into_iter()
                .map(|tx_signature| {
                    Signature::try_from(tx_signature.as_slice()).map_err(|_error| {
                        YellowstoneGrpcError::Convert("failed to parse transaction signature")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let message = {
                let message = tx
                    .message
                    .ok_or(YellowstoneGrpcError::Convert("missing transaction message"))?;
                let header = message
                    .header
                    .ok_or(YellowstoneGrpcError::Convert("missing message header"))?;
                let header = MessageHeader {
                    num_required_signatures: u8::try_from(header.num_required_signatures).map_err(
                        |_error| YellowstoneGrpcError::Convert("invalid num_required_signatures"),
                    )?,
                    num_readonly_signed_accounts: u8::try_from(header.num_readonly_signed_accounts)
                        .map_err(|_error| {
                            YellowstoneGrpcError::Convert("invalid num_readonly_signed_accounts")
                        })?,
                    num_readonly_unsigned_accounts: u8::try_from(
                        header.num_readonly_unsigned_accounts,
                    )
                    .map_err(|_error| {
                        YellowstoneGrpcError::Convert("invalid num_readonly_unsigned_accounts")
                    })?,
                };
                let account_keys = message
                    .account_keys
                    .into_iter()
                    .map(|key| {
                        Pubkey::try_from(key.as_slice())
                            .map_err(|_error| YellowstoneGrpcError::Convert("invalid account key"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let recent_blockhash = <[u8; 32]>::try_from(message.recent_blockhash.as_slice())
                    .map(Hash::new_from_array)
                    .map_err(|_error| YellowstoneGrpcError::Convert("invalid recent blockhash"))?;
                let instructions = message
                    .instructions
                    .into_iter()
                    .map(|instruction| {
                        Ok(CompiledInstruction {
                            program_id_index: u8::try_from(instruction.program_id_index).map_err(
                                |_error| {
                                    YellowstoneGrpcError::Convert(
                                        "invalid compiled instruction program id index",
                                    )
                                },
                            )?,
                            accounts: instruction.accounts,
                            data: instruction.data,
                        })
                    })
                    .collect::<Result<Vec<_>, YellowstoneGrpcError>>()?;
                if message.versioned {
                    let address_table_lookups = message
                        .address_table_lookups
                        .into_iter()
                        .map(|lookup| {
                            Ok(MessageAddressTableLookup {
                                account_key: Pubkey::try_from(lookup.account_key.as_slice())
                                    .map_err(|_error| {
                                        YellowstoneGrpcError::Convert(
                                            "invalid address table account key",
                                        )
                                    })?,
                                writable_indexes: lookup.writable_indexes,
                                readonly_indexes: lookup.readonly_indexes,
                            })
                        })
                        .collect::<Result<Vec<_>, YellowstoneGrpcError>>()?;
                    VersionedMessage::V0(MessageV0 {
                        header,
                        account_keys,
                        recent_blockhash,
                        instructions,
                        address_table_lookups,
                    })
                } else {
                    VersionedMessage::Legacy(Message {
                        header,
                        account_keys,
                        recent_blockhash,
                        instructions,
                    })
                }
            };
            VersionedTransaction {
                signatures,
                message,
            }
        };
        Ok(TransactionEvent {
            slot,
            commitment_status,
            confirmed_slot: None,
            finalized_slot: None,
            signature,
            kind: classify_provider_transaction_kind(&tx),
            tx: Arc::new(tx),
            provider_source: None,
        })
    }

    #[test]
    fn yellowstone_transaction_event_from_update_decodes_transaction() {
        let event = transaction_event_from_update(
            55,
            Some(sample_update()),
            TxCommitmentStatus::Confirmed,
            ProviderCommitmentWatermarks::default(),
        )
        .expect("event");
        assert_eq!(event.slot, 55);
        assert_eq!(event.kind, TxKind::Mixed);
        assert!(event.signature.is_some());
    }

    #[test]
    fn yellowstone_transaction_event_from_update_shortcuts_vote_only() {
        let event = transaction_event_from_update(
            56,
            Some(sample_vote_update()),
            TxCommitmentStatus::Confirmed,
            ProviderCommitmentWatermarks::default(),
        )
        .expect("event");
        assert_eq!(event.slot, 56);
        assert_eq!(event.kind, TxKind::VoteOnly);
        assert!(event.signature.is_some());
    }

    #[tokio::test]
    async fn yellowstone_spawn_rejects_transaction_filters_for_accounts_stream() {
        let (tx, _rx) = create_provider_stream_queue(1);
        let config = YellowstoneGrpcConfig::new("http://127.0.0.1:1")
            .with_stream(YellowstoneGrpcStream::Accounts)
            .with_vote(true);

        let error = spawn_yellowstone_grpc_source(config, tx)
            .await
            .expect_err("accounts stream should reject transaction-only filters");

        match error {
            YellowstoneGrpcError::Config(YellowstoneGrpcConfigError::UnsupportedOption {
                option: YellowstoneGrpcConfigOption::VoteFilter,
                stream: YellowstoneGrpcStreamKind::Accounts,
            }) => {}
            other => panic!("expected vote config rejection, got {other:?}"),
        }
    }

    #[test]
    #[ignore = "profiling fixture for Yellowstone provider transaction conversion A/B"]
    fn yellowstone_transaction_conversion_profile_fixture() {
        let iterations = profile_iterations(200_000);

        let update = sample_update();

        let baseline_started = Instant::now();
        for _ in 0..iterations {
            let event = transaction_event_from_update_baseline(
                55,
                Some(update.clone()),
                TxCommitmentStatus::Processed,
            )
            .expect("baseline event");
            black_box(event);
        }
        let baseline_elapsed = baseline_started.elapsed();

        let optimized_started = Instant::now();
        for _ in 0..iterations {
            let event = transaction_event_from_update(
                55,
                Some(update.clone()),
                TxCommitmentStatus::Processed,
                ProviderCommitmentWatermarks::default(),
            )
            .expect("optimized event");
            black_box(event);
        }
        let optimized_elapsed = optimized_started.elapsed();
        let baseline_avg_ns = avg_ns_per_iteration(baseline_elapsed, iterations);
        let optimized_avg_ns = avg_ns_per_iteration(optimized_elapsed, iterations);

        eprintln!(
            "yellowstone_transaction_conversion_profile_fixture iterations={} baseline_us={} optimized_us={} baseline_avg_ns_per_iteration={} optimized_avg_ns_per_iteration={} baseline_avg_us_per_iteration={:.3} optimized_avg_us_per_iteration={:.3}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
            baseline_avg_ns,
            optimized_avg_ns,
            baseline_avg_ns as f64 / 1_000.0,
            optimized_avg_ns as f64 / 1_000.0,
        );
    }

    #[test]
    #[ignore = "profiling fixture for baseline Yellowstone transaction conversion"]
    fn yellowstone_transaction_conversion_baseline_profile_fixture() {
        let iterations = profile_iterations(200_000);

        let update = sample_update();
        for _ in 0..iterations {
            let event = transaction_event_from_update_baseline(
                55,
                Some(update.clone()),
                TxCommitmentStatus::Processed,
            )
            .expect("baseline event");
            black_box(event);
        }
    }

    #[test]
    #[ignore = "profiling fixture for optimized Yellowstone transaction conversion"]
    fn yellowstone_transaction_conversion_optimized_profile_fixture() {
        let iterations = profile_iterations(200_000);

        let update = sample_update();
        for _ in 0..iterations {
            let event = transaction_event_from_update(
                55,
                Some(update.clone()),
                TxCommitmentStatus::Processed,
                ProviderCommitmentWatermarks::default(),
            )
            .expect("optimized event");
            black_box(event);
        }
    }

    #[test]
    #[ignore = "profiling fixture for Yellowstone vote-only conversion A/B"]
    fn yellowstone_vote_only_conversion_profile_fixture() {
        let iterations = profile_iterations(200_000);

        let update = sample_vote_update();

        let baseline_started = Instant::now();
        for _ in 0..iterations {
            let event = transaction_event_from_update_baseline(
                56,
                Some(update.clone()),
                TxCommitmentStatus::Processed,
            )
            .expect("baseline event");
            black_box(event);
        }
        let baseline_elapsed = baseline_started.elapsed();

        let optimized_started = Instant::now();
        for _ in 0..iterations {
            let event = transaction_event_from_update(
                56,
                Some(update.clone()),
                TxCommitmentStatus::Processed,
                ProviderCommitmentWatermarks::default(),
            )
            .expect("optimized event");
            black_box(event);
        }
        let optimized_elapsed = optimized_started.elapsed();
        let baseline_avg_ns = avg_ns_per_iteration(baseline_elapsed, iterations);
        let optimized_avg_ns = avg_ns_per_iteration(optimized_elapsed, iterations);

        eprintln!(
            "yellowstone_vote_only_conversion_profile_fixture iterations={} baseline_us={} optimized_us={} baseline_avg_ns_per_iteration={} optimized_avg_ns_per_iteration={} baseline_avg_us_per_iteration={:.3} optimized_avg_us_per_iteration={:.3}",
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
