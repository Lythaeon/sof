#![allow(clippy::missing_docs_in_private_items)]

//! Helius LaserStream adapters for SOF processed provider-stream ingress.
//!
//! LaserStream uses the Yellowstone-compatible gRPC subscription model. SOF's
//! built-in adapter exposes transaction, transaction-status, account-update,
//! block-meta, and slot feeds through the same typed provider-stream surface.

use std::{
    collections::HashMap,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_channel::mpsc as futures_mpsc;
use futures_util::{SinkExt, StreamExt};
use helius_laserstream::{
    ChannelOptions, LaserstreamConfig as ClientConfig, LaserstreamError as ClientError, grpc,
};
use laserstream_core_client::{ClientTlsConfig, Interceptor};
use laserstream_core_proto::prelude::Transaction as LaserStreamTransaction;
use laserstream_core_proto::tonic::{
    Status, codec::CompressionEncoding, metadata::MetadataValue, transport::Endpoint,
};
use sof_types::{PubkeyBytes, SignatureBytes};
use solana_hash::Hash;
use solana_message::{
    Message, MessageHeader, VersionedMessage,
    compiled_instruction::CompiledInstruction,
    v0::{Message as MessageV0, MessageAddressTableLookup},
};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use thiserror::Error;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;

use crate::{
    event::{ForkSlotStatus, TxCommitmentStatus, TxKind},
    framework::{
        AccountUpdateEvent, BlockMetaEvent, SlotStatusEvent, TransactionEvent,
        TransactionStatusEvent,
    },
    provider_stream::{
        ProviderCommitmentWatermarks, ProviderReplayMode, ProviderSourceHealthEvent,
        ProviderSourceHealthReason, ProviderSourceHealthStatus, ProviderSourceId,
        ProviderSourceIdentity, ProviderSourceIdentityRegistrationError, ProviderSourceReadiness,
        ProviderSourceReservation, ProviderSourceTaskGuard, ProviderStreamFanIn,
        ProviderStreamMode, ProviderStreamSender, ProviderStreamUpdate,
        classify_provider_transaction_kind,
    },
};

const INTERNAL_WATERMARK_SLOT_FILTER: &str = "__sof_watermark_slots";
const LASERSTREAM_SDK_NAME: &str = "sof";
const LASERSTREAM_SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// LaserStream subscription commitment used for provider-stream transaction updates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LaserStreamCommitment {
    /// Processed commitment.
    #[default]
    Processed,
    /// Confirmed commitment.
    Confirmed,
    /// Finalized commitment.
    Finalized,
}

impl LaserStreamCommitment {
    const fn as_proto(self) -> grpc::CommitmentLevel {
        match self {
            Self::Processed => grpc::CommitmentLevel::Processed,
            Self::Confirmed => grpc::CommitmentLevel::Confirmed,
            Self::Finalized => grpc::CommitmentLevel::Finalized,
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

/// Connection and filter config for LaserStream transaction subscriptions.
#[derive(Clone, Debug)]
pub struct LaserStreamConfig {
    endpoint: String,
    api_key: String,
    source_instance: Option<Arc<str>>,
    readiness: ProviderSourceReadiness,
    stream: LaserStreamStream,
    commitment: LaserStreamCommitment,
    vote: Option<bool>,
    failed: Option<bool>,
    signature: Option<Signature>,
    account_include: Vec<Pubkey>,
    account_exclude: Vec<Pubkey>,
    account_required: Vec<Pubkey>,
    accounts: Vec<Pubkey>,
    owners: Vec<Pubkey>,
    require_txn_signature: bool,
    connect_timeout: Option<Duration>,
    timeout: Option<Duration>,
    stall_timeout: Option<Duration>,
    max_decoding_message_size: Option<usize>,
    max_encoding_message_size: Option<usize>,
    reconnect_delay: Duration,
    max_reconnect_attempts: Option<u32>,
    replay_mode: ProviderReplayMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// One LaserStream stream-specific config option that may be invalid for a selected stream kind.
pub enum LaserStreamConfigOption {
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

impl std::fmt::Display for LaserStreamConfigOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
/// Typed LaserStream config validation failures for built-in stream selectors.
pub enum LaserStreamConfigError {
    /// The selected LaserStream stream kind does not support one configured option.
    #[error("{option} is not supported for laserstream {stream} streams")]
    UnsupportedOption {
        /// Built-in LaserStream stream family receiving the invalid option.
        stream: LaserStreamStreamKind,
        /// Typed config option that is unsupported for that stream family.
        option: LaserStreamConfigOption,
    },
}

impl LaserStreamConfig {
    /// Creates a transaction-stream config for one LaserStream endpoint.
    ///
    /// By default no vote/failed filter is applied, so the stream remains
    /// inclusive unless you narrow it explicitly.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sof::provider_stream::laserstream::LaserStreamConfig;
    ///
    /// let config = LaserStreamConfig::new(
    ///     "https://laserstream-mainnet-fra.helius-rpc.com",
    ///     "api-key",
    /// );
    /// assert_eq!(config.endpoint(), "https://laserstream-mainnet-fra.helius-rpc.com");
    /// ```
    #[must_use]
    pub fn new(endpoint: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key: api_key.into(),
            source_instance: None,
            readiness: ProviderSourceReadiness::Required,
            stream: LaserStreamStream::Transaction,
            commitment: LaserStreamCommitment::Processed,
            vote: None,
            failed: None,
            signature: None,
            account_include: Vec::new(),
            account_exclude: Vec::new(),
            account_required: Vec::new(),
            accounts: Vec::new(),
            owners: Vec::new(),
            require_txn_signature: false,
            connect_timeout: Some(Duration::from_secs(10)),
            timeout: Some(Duration::from_secs(30)),
            stall_timeout: Some(Duration::from_secs(30)),
            max_decoding_message_size: Some(64 * 1024 * 1024),
            max_encoding_message_size: None,
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

    /// Selects the LaserStream stream family this config should subscribe to.
    #[must_use]
    pub const fn with_stream(mut self, stream: LaserStreamStream) -> Self {
        self.stream = stream;
        self
    }

    /// Sets the LaserStream commitment.
    #[must_use]
    pub const fn with_commitment(mut self, commitment: LaserStreamCommitment) -> Self {
        self.commitment = commitment;
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

    /// Requires account updates to carry a transaction signature.
    #[must_use]
    pub const fn require_transaction_signature(mut self) -> Self {
        self.require_txn_signature = true;
        self
    }

    /// Sets the gRPC connect timeout.
    #[must_use]
    pub const fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Sets the gRPC request timeout.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sets the idle watchdog timeout for one stream session.
    #[must_use]
    pub const fn with_stall_timeout(mut self, timeout: Duration) -> Self {
        self.stall_timeout = Some(timeout);
        self
    }

    /// Sets the max decoding message size.
    #[must_use]
    pub const fn with_max_decoding_message_size(mut self, bytes: usize) -> Self {
        self.max_decoding_message_size = Some(bytes);
        self
    }

    /// Sets the max encoding message size.
    #[must_use]
    pub const fn with_max_encoding_message_size(mut self, bytes: usize) -> Self {
        self.max_encoding_message_size = Some(bytes);
        self
    }

    fn validate(&self) -> Result<(), LaserStreamConfigError> {
        match self.stream {
            LaserStreamStream::Transaction | LaserStreamStream::TransactionStatus => {
                self.validate_non_account_stream()?;
            }
            LaserStreamStream::Accounts => {
                self.validate_accounts_stream()?;
            }
            LaserStreamStream::BlockMeta => {
                self.validate_non_account_stream()?;
            }
        }
        Ok(())
    }

    fn validate_non_account_stream(&self) -> Result<(), LaserStreamConfigError> {
        if let Some(option) = [
            (!self.accounts.is_empty()).then_some(LaserStreamConfigOption::Accounts),
            (!self.owners.is_empty()).then_some(LaserStreamConfigOption::Owners),
            self.require_txn_signature
                .then_some(LaserStreamConfigOption::RequireTransactionSignature),
        ]
        .into_iter()
        .flatten()
        .next()
        {
            return Err(LaserStreamConfigError::UnsupportedOption {
                stream: self.stream_kind(),
                option,
            });
        }
        Ok(())
    }

    fn validate_accounts_stream(&self) -> Result<(), LaserStreamConfigError> {
        if let Some(option) = [
            self.vote.map(|_| LaserStreamConfigOption::VoteFilter),
            self.failed.map(|_| LaserStreamConfigOption::FailedFilter),
            self.signature
                .map(|_| LaserStreamConfigOption::SignatureFilter),
            (!self.account_include.is_empty())
                .then_some(LaserStreamConfigOption::AccountIncludeFilter),
            (!self.account_exclude.is_empty())
                .then_some(LaserStreamConfigOption::AccountExcludeFilter),
            (!self.account_required.is_empty())
                .then_some(LaserStreamConfigOption::AccountRequiredFilter),
        ]
        .into_iter()
        .flatten()
        .next()
        {
            return Err(LaserStreamConfigError::UnsupportedOption {
                stream: self.stream_kind(),
                option,
            });
        }
        Ok(())
    }

    /// Sets the max reconnect attempts used by the SDK.
    #[must_use]
    pub const fn with_max_reconnect_attempts(mut self, attempts: u32) -> Self {
        self.max_reconnect_attempts = Some(attempts);
        self
    }

    /// Sets the reconnect backoff used after stream failures.
    #[must_use]
    pub const fn with_reconnect_delay(mut self, delay: Duration) -> Self {
        self.reconnect_delay = delay;
        self
    }

    /// Sets provider replay behavior.
    #[must_use]
    pub const fn with_replay_mode(mut self, mode: ProviderReplayMode) -> Self {
        self.replay_mode = mode;
        self
    }

    #[cfg(test)]
    pub(crate) fn subscribe_request(&self) -> grpc::SubscribeRequest {
        self.subscribe_request_with_state(0)
    }

    pub(crate) fn subscribe_request_with_state(&self, tracked_slot: u64) -> grpc::SubscribeRequest {
        let mut request = grpc::SubscribeRequest {
            slots: HashMap::from([(
                INTERNAL_WATERMARK_SLOT_FILTER.to_owned(),
                grpc::SubscribeRequestFilterSlots {
                    filter_by_commitment: Some(true),
                    ..grpc::SubscribeRequestFilterSlots::default()
                },
            )]),
            commitment: Some(self.commitment.as_proto() as i32),
            from_slot: self.replay_from_slot(tracked_slot),
            ..grpc::SubscribeRequest::default()
        };
        match self.stream {
            LaserStreamStream::Transaction => {
                request.transactions =
                    HashMap::from([("sof".to_owned(), self.transaction_filter())]);
            }
            LaserStreamStream::TransactionStatus => {
                request.transactions_status =
                    HashMap::from([("sof".to_owned(), self.transaction_filter())]);
            }
            LaserStreamStream::Accounts => {
                request.accounts = HashMap::from([("sof".to_owned(), self.account_filter())]);
            }
            LaserStreamStream::BlockMeta => {
                request.blocks_meta = HashMap::from([(
                    "sof".to_owned(),
                    grpc::SubscribeRequestFilterBlocksMeta::default(),
                )]);
            }
        }
        request
    }

    fn client_config(&self) -> ClientConfig {
        let mut options = ChannelOptions::default();
        if let Some(timeout) = self.connect_timeout {
            options.connect_timeout_secs = Some(timeout.as_secs());
        }
        if let Some(timeout) = self.timeout {
            options.timeout_secs = Some(timeout.as_secs());
        }
        if let Some(bytes) = self.max_decoding_message_size {
            options.max_decoding_message_size = Some(bytes);
        }
        if let Some(bytes) = self.max_encoding_message_size {
            options.max_encoding_message_size = Some(bytes);
        }

        let mut config = ClientConfig::new(self.endpoint.clone(), self.api_key.clone())
            .with_channel_options(options)
            .with_replay(!matches!(self.replay_mode, ProviderReplayMode::Live));
        if let Some(attempts) = self.max_reconnect_attempts {
            config = config.with_max_reconnect_attempts(attempts);
        }
        config
    }

    const fn replay_from_slot(&self, tracked_slot: u64) -> Option<u64> {
        match self.replay_mode {
            ProviderReplayMode::Live => None,
            ProviderReplayMode::Resume => {
                if tracked_slot == 0 {
                    None
                } else {
                    Some(tracked_slot)
                }
            }
            ProviderReplayMode::FromSlot(slot) => {
                if tracked_slot == 0 {
                    Some(slot)
                } else {
                    Some(tracked_slot)
                }
            }
        }
    }

    fn transaction_filter(&self) -> grpc::SubscribeRequestFilterTransactions {
        grpc::SubscribeRequestFilterTransactions {
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

    fn account_filter(&self) -> grpc::SubscribeRequestFilterAccounts {
        grpc::SubscribeRequestFilterAccounts {
            account: self.accounts.iter().map(ToString::to_string).collect(),
            owner: self.owners.iter().map(ToString::to_string).collect(),
            filters: Vec::new(),
            nonempty_txn_signature: Some(self.require_txn_signature),
        }
    }

    const fn source_id(&self) -> ProviderSourceId {
        match self.stream {
            LaserStreamStream::Transaction => ProviderSourceId::LaserStream,
            LaserStreamStream::TransactionStatus => ProviderSourceId::LaserStreamTransactionStatus,
            LaserStreamStream::Accounts => ProviderSourceId::LaserStreamAccounts,
            LaserStreamStream::BlockMeta => ProviderSourceId::LaserStreamBlockMeta,
        }
    }

    fn source_instance(&self) -> Option<&str> {
        self.source_instance.as_deref()
    }

    const fn readiness(&self) -> ProviderSourceReadiness {
        self.readiness
    }

    fn source_identity(&self) -> Option<ProviderSourceIdentity> {
        self.source_instance()
            .map(|instance| ProviderSourceIdentity::new(self.source_id(), instance))
    }

    /// Returns the runtime mode matching this built-in LaserStream stream selection.
    #[must_use]
    pub const fn runtime_mode(&self) -> ProviderStreamMode {
        match self.stream {
            LaserStreamStream::Transaction => ProviderStreamMode::LaserStream,
            LaserStreamStream::TransactionStatus => {
                ProviderStreamMode::LaserStreamTransactionStatus
            }
            LaserStreamStream::Accounts => ProviderStreamMode::LaserStreamAccounts,
            LaserStreamStream::BlockMeta => ProviderStreamMode::LaserStreamBlockMeta,
        }
    }

    const fn stream_kind(&self) -> LaserStreamStreamKind {
        match self.stream {
            LaserStreamStream::Transaction => LaserStreamStreamKind::Transaction,
            LaserStreamStream::TransactionStatus => LaserStreamStreamKind::TransactionStatus,
            LaserStreamStream::Accounts => LaserStreamStreamKind::Accounts,
            LaserStreamStream::BlockMeta => LaserStreamStreamKind::BlockMeta,
        }
    }
}

/// Primary LaserStream stream families supported by the built-in adapter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LaserStreamStream {
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

/// Connection and replay config for LaserStream slot subscriptions.
#[derive(Clone, Debug)]
pub struct LaserStreamSlotsConfig {
    endpoint: String,
    api_key: String,
    source_instance: Option<Arc<str>>,
    readiness: ProviderSourceReadiness,
    commitment: LaserStreamCommitment,
    connect_timeout: Option<Duration>,
    timeout: Option<Duration>,
    stall_timeout: Option<Duration>,
    max_decoding_message_size: Option<usize>,
    max_encoding_message_size: Option<usize>,
    reconnect_delay: Duration,
    max_reconnect_attempts: Option<u32>,
    replay_mode: ProviderReplayMode,
}

impl LaserStreamSlotsConfig {
    /// Creates a slot-stream config for one LaserStream endpoint.
    #[must_use]
    pub fn new(endpoint: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key: api_key.into(),
            source_instance: None,
            readiness: ProviderSourceReadiness::Optional,
            commitment: LaserStreamCommitment::Processed,
            connect_timeout: Some(Duration::from_secs(10)),
            timeout: Some(Duration::from_secs(30)),
            stall_timeout: Some(Duration::from_secs(30)),
            max_decoding_message_size: Some(64 * 1024 * 1024),
            max_encoding_message_size: None,
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

    /// Returns the runtime mode matching this built-in LaserStream slot stream.
    #[must_use]
    pub const fn runtime_mode(&self) -> ProviderStreamMode {
        ProviderStreamMode::LaserStreamSlots
    }

    fn source_instance(&self) -> Option<&str> {
        self.source_instance.as_deref()
    }

    const fn readiness(&self) -> ProviderSourceReadiness {
        self.readiness
    }

    fn source_identity(&self) -> Option<ProviderSourceIdentity> {
        self.source_instance().map(|instance| {
            ProviderSourceIdentity::new(ProviderSourceId::LaserStreamSlots, instance)
        })
    }

    /// Sets the LaserStream commitment.
    #[must_use]
    pub const fn with_commitment(mut self, commitment: LaserStreamCommitment) -> Self {
        self.commitment = commitment;
        self
    }

    /// Sets the gRPC connect timeout.
    #[must_use]
    pub const fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Sets the gRPC request timeout.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Sets the idle watchdog timeout for one stream session.
    #[must_use]
    pub const fn with_stall_timeout(mut self, timeout: Duration) -> Self {
        self.stall_timeout = Some(timeout);
        self
    }

    /// Sets the max decoding message size.
    #[must_use]
    pub const fn with_max_decoding_message_size(mut self, bytes: usize) -> Self {
        self.max_decoding_message_size = Some(bytes);
        self
    }

    /// Sets the max encoding message size.
    #[must_use]
    pub const fn with_max_encoding_message_size(mut self, bytes: usize) -> Self {
        self.max_encoding_message_size = Some(bytes);
        self
    }

    /// Sets the max reconnect attempts used by the SDK.
    #[must_use]
    pub const fn with_max_reconnect_attempts(mut self, attempts: u32) -> Self {
        self.max_reconnect_attempts = Some(attempts);
        self
    }

    /// Sets the reconnect backoff used after stream failures.
    #[must_use]
    pub const fn with_reconnect_delay(mut self, delay: Duration) -> Self {
        self.reconnect_delay = delay;
        self
    }

    /// Sets provider replay behavior.
    #[must_use]
    pub const fn with_replay_mode(mut self, mode: ProviderReplayMode) -> Self {
        self.replay_mode = mode;
        self
    }

    pub(crate) fn subscribe_request_with_state(&self, tracked_slot: u64) -> grpc::SubscribeRequest {
        grpc::SubscribeRequest {
            slots: HashMap::from([(
                "sof".to_owned(),
                grpc::SubscribeRequestFilterSlots {
                    filter_by_commitment: Some(true),
                    ..grpc::SubscribeRequestFilterSlots::default()
                },
            )]),
            commitment: Some(self.commitment.as_proto() as i32),
            from_slot: self.replay_from_slot(tracked_slot),
            ..grpc::SubscribeRequest::default()
        }
    }

    fn client_config(&self) -> ClientConfig {
        let mut options = ChannelOptions::default();
        if let Some(timeout) = self.connect_timeout {
            options.connect_timeout_secs = Some(timeout.as_secs());
        }
        if let Some(timeout) = self.timeout {
            options.timeout_secs = Some(timeout.as_secs());
        }
        if let Some(bytes) = self.max_decoding_message_size {
            options.max_decoding_message_size = Some(bytes);
        }
        if let Some(bytes) = self.max_encoding_message_size {
            options.max_encoding_message_size = Some(bytes);
        }

        let mut config = ClientConfig::new(self.endpoint.clone(), self.api_key.clone())
            .with_channel_options(options)
            .with_replay(!matches!(self.replay_mode, ProviderReplayMode::Live));
        if let Some(attempts) = self.max_reconnect_attempts {
            config = config.with_max_reconnect_attempts(attempts);
        }
        config
    }

    const fn replay_from_slot(&self, tracked_slot: u64) -> Option<u64> {
        match self.replay_mode {
            ProviderReplayMode::Live => None,
            ProviderReplayMode::Resume => {
                if tracked_slot == 0 {
                    None
                } else {
                    Some(tracked_slot)
                }
            }
            ProviderReplayMode::FromSlot(slot) => {
                if tracked_slot == 0 {
                    Some(slot)
                } else {
                    Some(tracked_slot)
                }
            }
        }
    }
}

/// LaserStream transaction-stream error surface.
#[derive(Debug, Error)]
pub enum LaserStreamError {
    /// Invalid config for the selected LaserStream stream kind.
    #[error(transparent)]
    Config(#[from] LaserStreamConfigError),
    /// Stream status or transport errors from the SDK.
    #[error(transparent)]
    Client(#[from] ClientError),
    /// Provider update could not be converted into a SOF event.
    #[error("laserstream transaction conversion failed: {0}")]
    Convert(&'static str),
    /// LaserStream protocol/runtime failure.
    #[error(transparent)]
    Protocol(#[from] LaserStreamProtocolError),
    /// Provider-stream queue is closed.
    #[error("provider-stream queue closed")]
    QueueClosed,
    /// One duplicated stable source identity was registered in the same fan-in.
    #[error(transparent)]
    DuplicateSourceIdentity(#[from] ProviderSourceIdentityRegistrationError),
}

/// Typed LaserStream protocol/runtime failures.
#[derive(Debug, Error)]
pub enum LaserStreamProtocolError {
    /// One LaserStream primary stream stopped making progress.
    #[error("laserstream {stream} stream stalled without inbound progress")]
    StreamStalled {
        /// Typed LaserStream stream family that stalled.
        stream: LaserStreamStreamKind,
    },
    /// One LaserStream slot stream stopped making progress.
    #[error("laserstream slot stream stalled without inbound progress")]
    SlotStreamStalled,
    /// The SDK interceptor rejected the API key metadata.
    #[error("invalid api key metadata: {0}")]
    InvalidApiKey(String),
    /// The configured endpoint could not be parsed.
    #[error("failed to parse endpoint: {0}")]
    InvalidEndpoint(String),
    /// TLS client setup failed.
    #[error("tls config error: {0}")]
    TlsConfig(String),
    /// Initial channel connect failed.
    #[error("connection failed: {0}")]
    ConnectionFailed(String),
    /// Sending the initial subscribe request failed.
    #[error("failed to send initial subscription request: {0}")]
    InitialSubscribeSend(String),
    /// Upstream rejected the subscribe call.
    #[error("subscription failed: {0}")]
    SubscriptionFailed(String),
    /// Upstream stream returned a status error.
    #[error("laserstream stream status: {0}")]
    StreamStatus(String),
    /// Reconnect attempts were exhausted.
    ///
    /// `attempts` is the consecutive-failure count that tripped the budget.
    #[error("exhausted laserstream reconnect attempts after {attempts} failures")]
    ReconnectBudgetExhausted {
        /// Consecutive-failure count that exhausted the reconnect budget.
        attempts: u32,
    },
}

/// Stable LaserStream stream kinds used by typed protocol errors.
#[derive(Clone, Copy, Debug)]
pub enum LaserStreamStreamKind {
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

impl std::fmt::Display for LaserStreamStreamKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transaction => f.write_str("transaction"),
            Self::TransactionStatus => f.write_str("transaction-status"),
            Self::Accounts => f.write_str("account"),
            Self::BlockMeta => f.write_str("block-meta"),
            Self::Slots => f.write_str("slot"),
        }
    }
}

type LaserStreamSubscribeSink = std::pin::Pin<
    Box<dyn futures_util::Sink<grpc::SubscribeRequest, Error = futures_mpsc::SendError> + Send>,
>;
type LaserStreamUpdateStream = std::pin::Pin<
    Box<
        dyn futures_util::Stream<
                Item = Result<grpc::SubscribeUpdate, yellowstone_grpc_proto::tonic::Status>,
            > + Send,
    >,
>;

struct LaserStreamPrimaryConnectionState<'state> {
    tracked_slot: &'state mut u64,
    watermarks: &'state mut ProviderCommitmentWatermarks,
    session_established: &'state mut bool,
}

struct LaserStreamSlotConnectionState<'state> {
    tracked_slot: &'state mut u64,
    watermarks: &'state mut ProviderCommitmentWatermarks,
    slot_states: &'state mut HashMap<u64, ForkSlotStatus>,
    session_established: &'state mut bool,
}

/// Spawns one LaserStream primary source into a SOF provider-stream queue.
///
/// # Examples
///
/// ```no_run
/// use sof::provider_stream::{
///     create_provider_stream_queue,
///     laserstream::{spawn_laserstream_source, LaserStreamConfig},
/// };
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let (tx, _rx) = create_provider_stream_queue(1024);
/// let config =
///     LaserStreamConfig::new("https://laserstream-mainnet-fra.helius-rpc.com", "api-key");
/// let handle = spawn_laserstream_source(config, tx).await?;
/// handle.abort();
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// Returns any connection/bootstrap error before the forwarder task starts.
pub async fn spawn_laserstream_source(
    config: LaserStreamConfig,
    sender: ProviderStreamSender,
) -> Result<JoinHandle<Result<(), LaserStreamError>>, LaserStreamError> {
    spawn_laserstream_source_inner(config, sender, None).await
}

#[deprecated(note = "use spawn_laserstream_source")]
/// Deprecated compatibility shim for [`spawn_laserstream_source`].
///
/// # Errors
///
/// Returns the same startup validation or bootstrap error as
/// [`spawn_laserstream_source`].
pub async fn spawn_laserstream_transaction_source(
    config: LaserStreamConfig,
    sender: ProviderStreamSender,
) -> Result<JoinHandle<Result<(), LaserStreamError>>, LaserStreamError> {
    spawn_laserstream_source(config, sender).await
}

async fn spawn_laserstream_source_inner(
    config: LaserStreamConfig,
    sender: ProviderStreamSender,
    reservation: Option<Arc<ProviderSourceReservation>>,
) -> Result<JoinHandle<Result<(), LaserStreamError>>, LaserStreamError> {
    config.validate()?;
    let source = ProviderSourceIdentity::generated(config.source_id(), config.source_instance());
    register_laserstream_primary_initial_health(&source, config.readiness(), &sender);
    let first_session =
        match connect_and_subscribe_once(&config, config.subscribe_request_with_state(0)).await {
            Ok(session) => session,
            Err(error) => {
                publish_laserstream_health_nonblocking(
                    &sender,
                    &ProviderSourceHealthEvent {
                        source,
                        readiness: config.readiness(),
                        status: ProviderSourceHealthStatus::Removed,
                        reason: laserstream_health_reason(&error),
                        message: error.to_string(),
                    },
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
        let mut attempts = 0_u32;
        let mut tracked_slot = 0_u64;
        let mut watermarks = ProviderCommitmentWatermarks::default();
        let mut first_session = Some(first_session);
        loop {
            let mut session_established = false;
            let session = match first_session.take() {
                Some(session) => Ok(session),
                None => {
                    connect_and_subscribe_once(
                        &config,
                        config.subscribe_request_with_state(tracked_slot),
                    )
                    .await
                }
            };
            match session {
                Ok((subscribe_tx, stream)) => match run_laserstream_primary_connection(
                    &config,
                    &source,
                    &sender,
                    LaserStreamPrimaryConnectionState {
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
                            "laserstream {} stream ended unexpectedly",
                            config.stream_kind()
                        );
                        tracing::warn!(
                            endpoint = config.endpoint(),
                            detail,
                            "provider stream laserstream session ended unexpectedly; reconnecting"
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
                    Err(LaserStreamError::QueueClosed) => {
                        return Err(LaserStreamError::QueueClosed);
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            endpoint = config.endpoint(),
                            "provider stream laserstream session ended; reconnecting"
                        );
                        send_primary_provider_health(
                            &source,
                            config.readiness(),
                            &sender,
                            ProviderSourceHealthStatus::Reconnecting,
                            laserstream_health_reason(&error),
                            error.to_string(),
                        )
                        .await?;
                    }
                },
                Err(error) => {
                    tracing::warn!(
                        %error,
                        endpoint = config.endpoint(),
                        "provider stream laserstream connect/subscribe failed; reconnecting"
                    );
                    send_primary_provider_health(
                        &source,
                        config.readiness(),
                        &sender,
                        ProviderSourceHealthStatus::Reconnecting,
                        laserstream_health_reason(&error),
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
                    format!("exhausted laserstream reconnect attempts after {attempts} failures");
                send_primary_provider_health(
                    &source,
                    config.readiness(),
                    &sender,
                    ProviderSourceHealthStatus::Unhealthy,
                    ProviderSourceHealthReason::ReconnectBudgetExhausted,
                    detail.clone(),
                )
                .await?;
                return Err(LaserStreamProtocolError::ReconnectBudgetExhausted { attempts }.into());
            }
            tokio::time::sleep(config.reconnect_delay).await;
        }
    }))
}

/// Spawns one LaserStream slot source into a SOF provider-stream queue.
///
/// # Errors
///
/// Returns an error if the initial client connect or subscribe step fails.
pub async fn spawn_laserstream_slot_source(
    config: LaserStreamSlotsConfig,
    sender: ProviderStreamSender,
) -> Result<JoinHandle<Result<(), LaserStreamError>>, LaserStreamError> {
    spawn_laserstream_slot_source_inner(config, sender, None).await
}

async fn spawn_laserstream_slot_source_inner(
    config: LaserStreamSlotsConfig,
    sender: ProviderStreamSender,
    reservation: Option<Arc<ProviderSourceReservation>>,
) -> Result<JoinHandle<Result<(), LaserStreamError>>, LaserStreamError> {
    let source = ProviderSourceIdentity::generated(
        ProviderSourceId::LaserStreamSlots,
        config.source_instance(),
    );
    register_laserstream_slot_initial_health(&source, config.readiness(), &sender);
    let first_session =
        match connect_and_subscribe_slots_once(&config, config.subscribe_request_with_state(0))
            .await
        {
            Ok(session) => session,
            Err(error) => {
                publish_laserstream_slot_health_nonblocking(
                    &sender,
                    &ProviderSourceHealthEvent {
                        source,
                        readiness: config.readiness(),
                        status: ProviderSourceHealthStatus::Removed,
                        reason: laserstream_health_reason(&error),
                        message: error.to_string(),
                    },
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
        let mut attempts = 0_u32;
        let mut tracked_slot = 0_u64;
        let mut watermarks = ProviderCommitmentWatermarks::default();
        let mut slot_states = HashMap::new();
        let mut first_session = Some(first_session);
        loop {
            let mut session_established = false;
            let session = match first_session.take() {
                Some(session) => Ok(session),
                None => {
                    connect_and_subscribe_slots_once(
                        &config,
                        config.subscribe_request_with_state(tracked_slot),
                    )
                    .await
                }
            };
            match session {
                Ok((subscribe_tx, stream)) => match run_laserstream_slot_connection(
                    &config,
                    &source,
                    &sender,
                    LaserStreamSlotConnectionState {
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
                        let detail = "laserstream slot stream ended unexpectedly".to_owned();
                        tracing::warn!(
                            endpoint = config.endpoint(),
                            detail,
                            "provider stream laserstream slot session ended unexpectedly; reconnecting"
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
                    Err(LaserStreamError::QueueClosed) => {
                        return Err(LaserStreamError::QueueClosed);
                    }
                    Err(error) => {
                        tracing::warn!(
                            %error,
                            endpoint = config.endpoint(),
                            "provider stream laserstream slot session ended; reconnecting"
                        );
                        send_provider_slot_health(
                            &source,
                            config.readiness(),
                            &sender,
                            ProviderSourceHealthStatus::Reconnecting,
                            laserstream_health_reason(&error),
                            error.to_string(),
                        )
                        .await?;
                    }
                },
                Err(error) => {
                    tracing::warn!(
                        %error,
                        endpoint = config.endpoint(),
                        "provider stream laserstream slot connect/subscribe failed; reconnecting"
                    );
                    send_provider_slot_health(
                        &source,
                        config.readiness(),
                        &sender,
                        ProviderSourceHealthStatus::Reconnecting,
                        laserstream_health_reason(&error),
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
                    "exhausted laserstream slot reconnect attempts after {attempts} failures"
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
                return Err(LaserStreamProtocolError::ReconnectBudgetExhausted { attempts }.into());
            }
            tokio::time::sleep(config.reconnect_delay).await;
        }
    }))
}

async fn run_laserstream_primary_connection(
    config: &LaserStreamConfig,
    source: &ProviderSourceIdentity,
    sender: &ProviderStreamSender,
    state: LaserStreamPrimaryConnectionState<'_>,
    _subscribe_tx: LaserStreamSubscribeSink,
    mut stream: LaserStreamUpdateStream,
) -> Result<(), LaserStreamError> {
    *state.session_established = false;
    let commitment = config.commitment.as_tx_commitment();
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
    let mut last_progress = Instant::now();
    loop {
        tokio::select! {
            () = async {
                if let Some(timeout) = config.stall_timeout {
                    let deadline = last_progress.checked_add(timeout).unwrap_or(last_progress);
                    tokio::time::sleep_until(deadline.into()).await;
                } else {
                    futures_util::future::pending::<()>().await;
                }
            } => {
                return Err(LaserStreamProtocolError::StreamStalled {
                    stream: config.stream_kind(),
                }
                .into());
            }
            maybe_update = stream.next() => {
                let Some(update) = maybe_update else {
                    return Ok(());
                };
                let update = update
                    .map_err(|error| LaserStreamProtocolError::StreamStatus(error.to_string()))?;
                last_progress = Instant::now();
                match update.update_oneof {
                    Some(grpc::subscribe_update::UpdateOneof::Slot(slot_update)) => {
                        *state.tracked_slot = (*state.tracked_slot).max(slot_update.slot);
                        if update
                            .filters
                            .iter()
                            .any(|filter| filter == INTERNAL_WATERMARK_SLOT_FILTER)
                        {
                            match grpc::SlotStatus::try_from(slot_update.status).ok() {
                                Some(grpc::SlotStatus::SlotConfirmed) => {
                                    state.watermarks.observe_confirmed_slot(slot_update.slot);
                                }
                                Some(grpc::SlotStatus::SlotFinalized) => {
                                    state.watermarks.observe_finalized_slot(slot_update.slot);
                                }
                                _ => {}
                            }
                        }
                    }
                    Some(grpc::subscribe_update::UpdateOneof::Transaction(tx_update))
                        if config.stream == LaserStreamStream::Transaction =>
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
                                    .with_provider_source(source.clone()),
                            )
                            .await
                            .map_err(|_error| LaserStreamError::QueueClosed)?;
                    }
                    Some(grpc::subscribe_update::UpdateOneof::TransactionStatus(status_update))
                        if config.stream == LaserStreamStream::TransactionStatus =>
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
                                    .with_provider_source(source.clone()),
                            )
                            .await
                            .map_err(|_error| LaserStreamError::QueueClosed)?;
                    }
                    Some(grpc::subscribe_update::UpdateOneof::Account(account_update))
                        if config.stream == LaserStreamStream::Accounts =>
                    {
                        *state.tracked_slot = (*state.tracked_slot).max(account_update.slot);
                        observe_non_transaction_commitment(
                            state.watermarks,
                            account_update.slot,
                            commitment,
                        );
                        let event = account_update_event_from_laserstream(
                            account_update,
                            commitment,
                            *state.watermarks,
                        )?;
                        sender
                            .send(
                                ProviderStreamUpdate::AccountUpdate(event)
                                    .with_provider_source(source.clone()),
                            )
                            .await
                            .map_err(|_error| LaserStreamError::QueueClosed)?;
                    }
                    Some(grpc::subscribe_update::UpdateOneof::BlockMeta(block_meta_update))
                        if config.stream == LaserStreamStream::BlockMeta =>
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
                                    .with_provider_source(source.clone()),
                            )
                            .await
                            .map_err(|_error| LaserStreamError::QueueClosed)?;
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn run_laserstream_slot_connection(
    config: &LaserStreamSlotsConfig,
    source: &ProviderSourceIdentity,
    sender: &ProviderStreamSender,
    state: LaserStreamSlotConnectionState<'_>,
    _subscribe_tx: LaserStreamSubscribeSink,
    mut stream: LaserStreamUpdateStream,
) -> Result<(), LaserStreamError> {
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
    let mut last_progress = Instant::now();
    loop {
        tokio::select! {
            () = async {
                if let Some(timeout) = config.stall_timeout {
                    let deadline = last_progress.checked_add(timeout).unwrap_or(last_progress);
                    tokio::time::sleep_until(deadline.into()).await;
                } else {
                    futures_util::future::pending::<()>().await;
                }
            } => {
                return Err(LaserStreamProtocolError::SlotStreamStalled.into());
            }
            maybe_update = stream.next() => {
                let Some(update) = maybe_update else {
                    return Ok(());
                };
                let update = update
                    .map_err(|error| LaserStreamProtocolError::StreamStatus(error.to_string()))?;
                last_progress = Instant::now();
                if let Some(grpc::subscribe_update::UpdateOneof::Slot(slot_update)) = update.update_oneof {
                    *state.tracked_slot = (*state.tracked_slot).max(slot_update.slot);
                    if let Some(event) = slot_status_event_from_update(
                        slot_update.slot,
                        slot_update.parent,
                        grpc::SlotStatus::try_from(slot_update.status).ok(),
                        state.watermarks,
                        state.slot_states,
                    ) {
                        sender
                            .send(
                                ProviderStreamUpdate::SlotStatus(event)
                                    .with_provider_source(source.clone()),
                            )
                            .await
                            .map_err(|_error| LaserStreamError::QueueClosed)?;
                    }
                }
            }
        }
    }
}

async fn send_primary_provider_health(
    source: &ProviderSourceIdentity,
    readiness: ProviderSourceReadiness,
    sender: &ProviderStreamSender,
    status: ProviderSourceHealthStatus,
    reason: ProviderSourceHealthReason,
    message: String,
) -> Result<(), LaserStreamError> {
    sender
        .send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
            source: source.clone(),
            readiness,
            status,
            reason,
            message,
        }))
        .await
        .map_err(|_error| LaserStreamError::QueueClosed)
}

fn register_laserstream_primary_initial_health(
    source: &ProviderSourceIdentity,
    readiness: ProviderSourceReadiness,
    sender: &ProviderStreamSender,
) {
    publish_laserstream_health_nonblocking(
        sender,
        &ProviderSourceHealthEvent {
            source: source.clone(),
            readiness,
            status: ProviderSourceHealthStatus::Reconnecting,
            reason: ProviderSourceHealthReason::InitialConnectPending,
            message: format!(
                "waiting for first laserstream {} session ack",
                source.kind_str().trim_start_matches("laserstream_")
            ),
        },
    );
}

fn publish_laserstream_health_nonblocking(
    sender: &ProviderStreamSender,
    event: &ProviderSourceHealthEvent,
) {
    match sender.try_send(ProviderStreamUpdate::Health(event.clone())) {
        Ok(()) | Err(TrySendError::Closed(_)) => {}
        Err(TrySendError::Full(update)) => {
            let sender = sender.clone();
            tokio::spawn(async move {
                drop(sender.send(update).await);
            });
        }
    }
}

async fn send_provider_slot_health(
    source: &ProviderSourceIdentity,
    readiness: ProviderSourceReadiness,
    sender: &ProviderStreamSender,
    status: ProviderSourceHealthStatus,
    reason: ProviderSourceHealthReason,
    message: String,
) -> Result<(), LaserStreamError> {
    sender
        .send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
            source: source.clone(),
            readiness,
            status,
            reason,
            message,
        }))
        .await
        .map_err(|_error| LaserStreamError::QueueClosed)
}

fn register_laserstream_slot_initial_health(
    source: &ProviderSourceIdentity,
    readiness: ProviderSourceReadiness,
    sender: &ProviderStreamSender,
) {
    publish_laserstream_slot_health_nonblocking(
        sender,
        &ProviderSourceHealthEvent {
            source: source.clone(),
            readiness,
            status: ProviderSourceHealthStatus::Reconnecting,
            reason: ProviderSourceHealthReason::InitialConnectPending,
            message: "waiting for first laserstream slot session ack".to_owned(),
        },
    );
}

fn publish_laserstream_slot_health_nonblocking(
    sender: &ProviderStreamSender,
    event: &ProviderSourceHealthEvent,
) {
    match sender.try_send(ProviderStreamUpdate::Health(event.clone())) {
        Ok(()) | Err(TrySendError::Closed(_)) => {}
        Err(TrySendError::Full(update)) => {
            let sender = sender.clone();
            tokio::spawn(async move {
                drop(sender.send(update).await);
            });
        }
    }
}

const fn laserstream_health_reason(error: &LaserStreamError) -> ProviderSourceHealthReason {
    match error {
        LaserStreamError::Client(_) => ProviderSourceHealthReason::UpstreamTransportFailure,
        LaserStreamError::Config(_)
        | LaserStreamError::Convert(_)
        | LaserStreamError::Protocol(_)
        | LaserStreamError::DuplicateSourceIdentity(_) => {
            ProviderSourceHealthReason::UpstreamProtocolFailure
        }
        LaserStreamError::QueueClosed => ProviderSourceHealthReason::UpstreamProtocolFailure,
    }
}

#[derive(Clone)]
struct SofLaserStreamInterceptor {
    x_token: Option<laserstream_core_proto::tonic::metadata::AsciiMetadataValue>,
}

impl SofLaserStreamInterceptor {
    fn new(api_key: &str) -> Result<Self, Status> {
        let x_token =
            if api_key.is_empty() {
                None
            } else {
                Some(api_key.parse().map_err(|error| {
                    Status::invalid_argument(format!("Invalid API key: {error}"))
                })?)
            };
        Ok(Self { x_token })
    }
}

const PROVIDER_SUBSCRIPTION_ACKNOWLEDGED: &str = "subscription acknowledged";

impl Interceptor for SofLaserStreamInterceptor {
    fn call(
        &mut self,
        mut request: laserstream_core_proto::tonic::Request<()>,
    ) -> Result<laserstream_core_proto::tonic::Request<()>, Status> {
        if let Some(ref x_token) = self.x_token {
            request.metadata_mut().insert("x-token", x_token.clone());
        }
        request.metadata_mut().insert(
            "x-sdk-name",
            MetadataValue::from_static(LASERSTREAM_SDK_NAME),
        );
        let sdk_version = MetadataValue::try_from(LASERSTREAM_SDK_VERSION)
            .map_err(|error| Status::internal(format!("invalid sdk version metadata: {error}")))?;
        request.metadata_mut().insert("x-sdk-version", sdk_version);
        Ok(request)
    }
}

async fn connect_and_subscribe_once(
    config: &LaserStreamConfig,
    request: grpc::SubscribeRequest,
) -> Result<(LaserStreamSubscribeSink, LaserStreamUpdateStream), LaserStreamError> {
    let options = config.client_config().channel_options;
    let interceptor = SofLaserStreamInterceptor::new(&config.api_key)
        .map_err(|error| LaserStreamProtocolError::InvalidApiKey(error.to_string()))?;

    let mut endpoint = Endpoint::from_shared(config.endpoint.clone())
        .map_err(|error| LaserStreamProtocolError::InvalidEndpoint(error.to_string()))?
        .connect_timeout(Duration::from_secs(
            options.connect_timeout_secs.unwrap_or(10),
        ))
        .timeout(Duration::from_secs(options.timeout_secs.unwrap_or(30)))
        .http2_keep_alive_interval(Duration::from_secs(
            options.http2_keep_alive_interval_secs.unwrap_or(30),
        ))
        .keep_alive_timeout(Duration::from_secs(
            options.keep_alive_timeout_secs.unwrap_or(5),
        ))
        .keep_alive_while_idle(options.keep_alive_while_idle.unwrap_or(true))
        .initial_stream_window_size(options.initial_stream_window_size.or(Some(1024 * 1024 * 4)))
        .initial_connection_window_size(
            options
                .initial_connection_window_size
                .or(Some(1024 * 1024 * 8)),
        )
        .http2_adaptive_window(options.http2_adaptive_window.unwrap_or(true))
        .tcp_nodelay(options.tcp_nodelay.unwrap_or(true))
        .buffer_size(options.buffer_size.or(Some(1024 * 64)));
    if let Some(tcp_keepalive_secs) = options.tcp_keepalive_secs {
        endpoint = endpoint.tcp_keepalive(Some(Duration::from_secs(tcp_keepalive_secs)));
    }
    if endpoint_uses_tls(&config.endpoint) {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_enabled_roots())
            .map_err(|error| LaserStreamProtocolError::TlsConfig(error.to_string()))?;
    }

    let channel = endpoint
        .connect()
        .await
        .map_err(|error| LaserStreamProtocolError::ConnectionFailed(error.to_string()))?;
    let mut geyser_client =
        grpc::geyser_client::GeyserClient::with_interceptor(channel, interceptor);
    geyser_client = geyser_client
        .max_decoding_message_size(options.max_decoding_message_size.unwrap_or(1_000_000_000))
        .max_encoding_message_size(options.max_encoding_message_size.unwrap_or(32_000_000));
    if let Some(send_comp) = options.send_compression {
        let encoding = match send_comp {
            helius_laserstream::CompressionEncoding::Gzip => CompressionEncoding::Gzip,
            helius_laserstream::CompressionEncoding::Zstd => CompressionEncoding::Zstd,
        };
        geyser_client = geyser_client.send_compressed(encoding);
    }
    if let Some(ref accept_comps) = options.accept_compression {
        for comp in accept_comps {
            let encoding = match comp {
                helius_laserstream::CompressionEncoding::Gzip => CompressionEncoding::Gzip,
                helius_laserstream::CompressionEncoding::Zstd => CompressionEncoding::Zstd,
            };
            geyser_client = geyser_client.accept_compressed(encoding);
        }
    }

    let (mut subscribe_tx, subscribe_rx) = futures_mpsc::unbounded();
    subscribe_tx
        .send(request)
        .await
        .map_err(|error| LaserStreamProtocolError::InitialSubscribeSend(error.to_string()))?;
    let response = geyser_client
        .subscribe(subscribe_rx)
        .await
        .map_err(|error| LaserStreamProtocolError::SubscriptionFailed(error.to_string()))?;

    Ok((Box::pin(subscribe_tx), Box::pin(response.into_inner())))
}

async fn connect_and_subscribe_slots_once(
    config: &LaserStreamSlotsConfig,
    request: grpc::SubscribeRequest,
) -> Result<(LaserStreamSubscribeSink, LaserStreamUpdateStream), LaserStreamError> {
    let options = config.client_config().channel_options;
    let interceptor = SofLaserStreamInterceptor::new(&config.api_key)
        .map_err(|error| LaserStreamProtocolError::InvalidApiKey(error.to_string()))?;

    let mut endpoint = Endpoint::from_shared(config.endpoint.clone())
        .map_err(|error| LaserStreamProtocolError::InvalidEndpoint(error.to_string()))?
        .connect_timeout(Duration::from_secs(
            options.connect_timeout_secs.unwrap_or(10),
        ))
        .timeout(Duration::from_secs(options.timeout_secs.unwrap_or(30)))
        .http2_keep_alive_interval(Duration::from_secs(
            options.http2_keep_alive_interval_secs.unwrap_or(30),
        ))
        .keep_alive_timeout(Duration::from_secs(
            options.keep_alive_timeout_secs.unwrap_or(5),
        ))
        .keep_alive_while_idle(options.keep_alive_while_idle.unwrap_or(true))
        .initial_stream_window_size(options.initial_stream_window_size.or(Some(1024 * 1024 * 4)))
        .initial_connection_window_size(
            options
                .initial_connection_window_size
                .or(Some(1024 * 1024 * 8)),
        )
        .http2_adaptive_window(options.http2_adaptive_window.unwrap_or(true))
        .tcp_nodelay(options.tcp_nodelay.unwrap_or(true))
        .buffer_size(options.buffer_size.or(Some(1024 * 64)));
    if let Some(tcp_keepalive_secs) = options.tcp_keepalive_secs {
        endpoint = endpoint.tcp_keepalive(Some(Duration::from_secs(tcp_keepalive_secs)));
    }
    if endpoint_uses_tls(&config.endpoint) {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_enabled_roots())
            .map_err(|error| LaserStreamProtocolError::TlsConfig(error.to_string()))?;
    }

    let channel = endpoint
        .connect()
        .await
        .map_err(|error| LaserStreamProtocolError::ConnectionFailed(error.to_string()))?;
    let mut geyser_client =
        grpc::geyser_client::GeyserClient::with_interceptor(channel, interceptor);
    geyser_client = geyser_client
        .max_decoding_message_size(options.max_decoding_message_size.unwrap_or(1_000_000_000))
        .max_encoding_message_size(options.max_encoding_message_size.unwrap_or(32_000_000));
    if let Some(send_comp) = options.send_compression {
        let encoding = match send_comp {
            helius_laserstream::CompressionEncoding::Gzip => CompressionEncoding::Gzip,
            helius_laserstream::CompressionEncoding::Zstd => CompressionEncoding::Zstd,
        };
        geyser_client = geyser_client.send_compressed(encoding);
    }
    if let Some(ref accept_comps) = options.accept_compression {
        for comp in accept_comps {
            let encoding = match comp {
                helius_laserstream::CompressionEncoding::Gzip => CompressionEncoding::Gzip,
                helius_laserstream::CompressionEncoding::Zstd => CompressionEncoding::Zstd,
            };
            geyser_client = geyser_client.accept_compressed(encoding);
        }
    }

    let (mut subscribe_tx, subscribe_rx) = futures_mpsc::unbounded();
    subscribe_tx
        .send(request)
        .await
        .map_err(|error| LaserStreamProtocolError::InitialSubscribeSend(error.to_string()))?;
    let response = geyser_client
        .subscribe(subscribe_rx)
        .await
        .map_err(|error| LaserStreamProtocolError::SubscriptionFailed(error.to_string()))?;

    Ok((Box::pin(subscribe_tx), Box::pin(response.into_inner())))
}

fn transaction_event_from_update(
    slot: u64,
    transaction: Option<grpc::SubscribeUpdateTransactionInfo>,
    commitment_status: TxCommitmentStatus,
    watermarks: ProviderCommitmentWatermarks,
) -> Result<TransactionEvent, LaserStreamError> {
    let transaction =
        transaction.ok_or(LaserStreamError::Convert("missing transaction payload"))?;
    let is_vote = transaction.is_vote;
    let signature = Some(signature_bytes_from_slice(
        transaction.signature.as_slice(),
        "invalid signature",
    )?);
    let tx = convert_transaction(
        transaction
            .transaction
            .ok_or(LaserStreamError::Convert("missing versioned transaction"))?,
    )?;
    Ok(TransactionEvent {
        slot,
        commitment_status,
        confirmed_slot: watermarks.confirmed_slot,
        finalized_slot: watermarks.finalized_slot,
        signature,
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
    update: grpc::SubscribeUpdateTransactionStatus,
) -> Result<TransactionStatusEvent, LaserStreamError> {
    let signature = signature_bytes_from_slice(
        update.signature.as_slice(),
        "invalid transaction-status signature",
    )?;
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

fn account_update_event_from_laserstream(
    update: grpc::SubscribeUpdateAccount,
    commitment_status: TxCommitmentStatus,
    watermarks: ProviderCommitmentWatermarks,
) -> Result<AccountUpdateEvent, LaserStreamError> {
    let account = update
        .account
        .ok_or(LaserStreamError::Convert("missing account payload"))?;
    let pubkey = pubkey_bytes_from_slice(account.pubkey.as_slice(), "invalid account pubkey")?;
    let owner = pubkey_bytes_from_slice(account.owner.as_slice(), "invalid account owner")?;
    let txn_signature = match account.txn_signature {
        Some(signature) => Some(signature_bytes_from_slice(
            signature.as_slice(),
            "invalid account txn signature",
        )?),
        None => None,
    };
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
    update: &grpc::SubscribeUpdateBlockMeta,
) -> Result<BlockMetaEvent, LaserStreamError> {
    let blockhash = Hash::from_str(&update.blockhash)
        .map_err(|_error| LaserStreamError::Convert("invalid block-meta blockhash"))?;
    let parent_blockhash = Hash::from_str(&update.parent_blockhash)
        .map_err(|_error| LaserStreamError::Convert("invalid parent blockhash"))?;
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

fn signature_bytes_from_slice(
    bytes: &[u8],
    message: &'static str,
) -> Result<SignatureBytes, LaserStreamError> {
    let raw: [u8; 64] = bytes
        .try_into()
        .map_err(|_error: std::array::TryFromSliceError| LaserStreamError::Convert(message))?;
    Ok(SignatureBytes::from(raw))
}

fn pubkey_bytes_from_slice(
    bytes: &[u8],
    message: &'static str,
) -> Result<PubkeyBytes, LaserStreamError> {
    let raw: [u8; 32] = bytes
        .try_into()
        .map_err(|_error: std::array::TryFromSliceError| LaserStreamError::Convert(message))?;
    Ok(PubkeyBytes::from(raw))
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

fn endpoint_uses_tls(endpoint: &str) -> bool {
    endpoint.starts_with("https://")
}

fn slot_status_event_from_update(
    slot: u64,
    parent_slot: Option<u64>,
    status: Option<grpc::SlotStatus>,
    watermarks: &mut ProviderCommitmentWatermarks,
    slot_states: &mut HashMap<u64, ForkSlotStatus>,
) -> Option<SlotStatusEvent> {
    let mapped = match status? {
        grpc::SlotStatus::SlotConfirmed => {
            watermarks.observe_confirmed_slot(slot);
            ForkSlotStatus::Confirmed
        }
        grpc::SlotStatus::SlotFinalized => {
            watermarks.observe_finalized_slot(slot);
            ForkSlotStatus::Finalized
        }
        grpc::SlotStatus::SlotDead => ForkSlotStatus::Orphaned,
        grpc::SlotStatus::SlotProcessed
        | grpc::SlotStatus::SlotFirstShredReceived
        | grpc::SlotStatus::SlotCompleted
        | grpc::SlotStatus::SlotCreatedBank => ForkSlotStatus::Processed,
    };
    let previous_status = slot_states.insert(slot, mapped);
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
    /// Spawns one LaserStream primary source into this fan-in.
    ///
    /// # Errors
    ///
    /// Returns an error if the selected config is invalid or the source cannot
    /// be started.
    pub async fn spawn_laserstream_source(
        &self,
        config: LaserStreamConfig,
    ) -> Result<JoinHandle<Result<(), LaserStreamError>>, LaserStreamError> {
        let reservation = match config.source_identity() {
            Some(source) => Some(Arc::new(self.reserve_source_identity(source)?)),
            None => None,
        };
        spawn_laserstream_source_inner(config, self.sender(), reservation).await
    }

    #[deprecated(note = "use spawn_laserstream_source")]
    /// Deprecated compatibility shim for [`ProviderStreamFanIn::spawn_laserstream_source`].
    ///
    /// # Errors
    ///
    /// Returns the same source-registration or startup error as
    /// [`ProviderStreamFanIn::spawn_laserstream_source`].
    pub async fn spawn_laserstream_transaction_source(
        &self,
        config: LaserStreamConfig,
    ) -> Result<JoinHandle<Result<(), LaserStreamError>>, LaserStreamError> {
        self.spawn_laserstream_source(config).await
    }

    /// Spawns one LaserStream slot source into this fan-in.
    ///
    /// # Errors
    ///
    /// Returns an error if the source cannot be started.
    pub async fn spawn_laserstream_slot_source(
        &self,
        config: LaserStreamSlotsConfig,
    ) -> Result<JoinHandle<Result<(), LaserStreamError>>, LaserStreamError> {
        let reservation = match config.source_identity() {
            Some(source) => Some(Arc::new(self.reserve_source_identity(source)?)),
            None => None,
        };
        spawn_laserstream_slot_source_inner(config, self.sender(), reservation).await
    }
}

#[inline]
fn convert_transaction(
    tx: LaserStreamTransaction,
) -> Result<VersionedTransaction, LaserStreamError> {
    let mut signatures = Vec::with_capacity(tx.signatures.len());
    for signature in tx.signatures {
        signatures.push(Signature::try_from(signature.as_slice()).map_err(|_error| {
            LaserStreamError::Convert("failed to parse transaction signature")
        })?);
    }
    let message = tx
        .message
        .ok_or(LaserStreamError::Convert("missing transaction message"))?;
    let header = message
        .header
        .ok_or(LaserStreamError::Convert("missing message header"))?;
    let header = MessageHeader {
        num_required_signatures: u8::try_from(header.num_required_signatures)
            .map_err(|_error| LaserStreamError::Convert("invalid num_required_signatures"))?,
        num_readonly_signed_accounts: u8::try_from(header.num_readonly_signed_accounts)
            .map_err(|_error| LaserStreamError::Convert("invalid num_readonly_signed_accounts"))?,
        num_readonly_unsigned_accounts: u8::try_from(header.num_readonly_unsigned_accounts)
            .map_err(|_error| {
                LaserStreamError::Convert("invalid num_readonly_unsigned_accounts")
            })?,
    };

    let recent_blockhash = <[u8; 32]>::try_from(message.recent_blockhash.as_slice())
        .map(Hash::new_from_array)
        .map_err(|_error| LaserStreamError::Convert("invalid recent blockhash"))?;
    let mut account_keys = Vec::with_capacity(message.account_keys.len());
    for key in message.account_keys {
        account_keys.push(
            Pubkey::try_from(key.as_slice())
                .map_err(|_error| LaserStreamError::Convert("invalid account key"))?,
        );
    }

    let mut instructions = Vec::with_capacity(message.instructions.len());
    for instruction in message.instructions {
        instructions.push(CompiledInstruction {
            program_id_index: u8::try_from(instruction.program_id_index).map_err(|_error| {
                LaserStreamError::Convert("invalid compiled instruction program id index")
            })?,
            accounts: instruction.accounts,
            data: instruction.data,
        });
    }

    let message = if message.versioned {
        let mut address_table_lookups = Vec::with_capacity(message.address_table_lookups.len());
        for lookup in message.address_table_lookups {
            address_table_lookups.push(MessageAddressTableLookup {
                account_key: Pubkey::try_from(lookup.account_key.as_slice()).map_err(|_error| {
                    LaserStreamError::Convert("invalid address table account key")
                })?,
                writable_indexes: lookup.writable_indexes,
                readonly_indexes: lookup.readonly_indexes,
            });
        }

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
    };

    Ok(VersionedTransaction {
        signatures,
        message,
    })
}

#[cfg(all(test, feature = "provider-grpc"))]
#[allow(
    clippy::let_underscore_must_use,
    clippy::shadow_unrelated,
    clippy::wildcard_enum_match_arm
)]
mod tests {
    use super::*;
    use crate::provider_stream::create_provider_stream_queue;
    use crate::provider_stream::yellowstone::{YellowstoneGrpcCommitment, YellowstoneGrpcConfig};
    use futures_channel::mpsc as futures_mpsc;
    use futures_util::stream::{self, Stream};
    use laserstream_core_proto::geyser::geyser_server::{Geyser, GeyserServer};
    use laserstream_core_proto::prelude::{
        BlockHeight, CompiledInstruction as ProtoCompiledInstruction, GetBlockHeightRequest,
        GetBlockHeightResponse, GetLatestBlockhashRequest, GetLatestBlockhashResponse,
        GetSlotRequest, GetSlotResponse, GetVersionRequest, GetVersionResponse,
        IsBlockhashValidRequest, IsBlockhashValidResponse, Message as ProtoMessage,
        MessageAddressTableLookup as ProtoMessageAddressTableLookup,
        MessageHeader as ProtoMessageHeader, PingRequest, PongResponse,
        SubscribePreprocessedRequest, SubscribePreprocessedUpdate, SubscribeReplayInfoRequest,
        SubscribeReplayInfoResponse, SubscribeRequest, SubscribeUpdate, SubscribeUpdateAccount,
        SubscribeUpdateAccountInfo, SubscribeUpdateTransactionInfo,
        SubscribeUpdateTransactionStatus, UnixTimestamp,
    };
    use laserstream_core_proto::tonic::{self, Request, Response, Status, transport::Server};
    use solana_instruction::Instruction;
    use solana_keypair::Keypair;
    use solana_message::{Message, VersionedMessage};
    use solana_sdk_ids::{compute_budget, system_program, vote};
    use solana_signer::Signer;
    use std::{pin::Pin, time::Instant};
    use tokio::sync::oneshot;
    use tokio::time::{Duration, timeout};

    fn profile_iterations(default: usize) -> usize {
        std::env::var("SOF_PROFILE_ITERATIONS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(default)
    }

    #[test]
    fn transaction_filter_shape_matches_yellowstone_config() {
        let signature = Signature::from([7_u8; 64]);
        let include = [Pubkey::new_unique(), Pubkey::new_unique()];
        let exclude = [Pubkey::new_unique()];
        let required = [Pubkey::new_unique()];

        let laser = LaserStreamConfig::new("https://laserstream.example", "token")
            .with_commitment(LaserStreamCommitment::Confirmed)
            .with_vote(true)
            .with_failed(true)
            .with_signature(signature)
            .with_account_include(include)
            .with_account_exclude(exclude)
            .with_account_required(required)
            .subscribe_request();
        let yellowstone = YellowstoneGrpcConfig::new("http://127.0.0.1:10000")
            .with_commitment(YellowstoneGrpcCommitment::Confirmed)
            .with_vote(true)
            .with_failed(true)
            .with_signature(signature)
            .with_account_include(include)
            .with_account_exclude(exclude)
            .with_account_required(required)
            .subscribe_request();

        let laser_filter = laser.transactions.get("sof").expect("laser filter");
        let yellowstone_filter = yellowstone.transactions.get("sof").expect("ys filter");

        assert_eq!(laser.commitment, yellowstone.commitment);
        assert_eq!(laser_filter.vote, yellowstone_filter.vote);
        assert_eq!(laser_filter.failed, yellowstone_filter.failed);
        assert_eq!(laser_filter.signature, yellowstone_filter.signature);
        assert_eq!(
            laser_filter.account_include,
            yellowstone_filter.account_include
        );
        assert_eq!(
            laser_filter.account_exclude,
            yellowstone_filter.account_exclude
        );
        assert_eq!(
            laser_filter.account_required,
            yellowstone_filter.account_required
        );
    }

    #[test]
    fn laserstream_config_defaults_do_not_filter_vote_or_failed() {
        let request =
            LaserStreamConfig::new("https://laserstream.example", "token").subscribe_request();
        let filter = request.transactions.get("sof").expect("sof filter");
        assert_eq!(filter.vote, None);
        assert_eq!(filter.failed, None);
    }

    #[test]
    fn laserstream_subscribe_request_tracks_slots_for_watermarks() {
        let request =
            LaserStreamConfig::new("https://laserstream.example", "token").subscribe_request();
        assert!(request.slots.contains_key(INTERNAL_WATERMARK_SLOT_FILTER));
        assert_eq!(request.from_slot, None);
    }

    #[test]
    fn laserstream_slot_subscribe_request_tracks_slots_and_replay_cursor() {
        let request = LaserStreamSlotsConfig::new("https://laserstream.example", "token")
            .with_replay_mode(ProviderReplayMode::FromSlot(321))
            .subscribe_request_with_state(777);
        assert!(request.slots.contains_key("sof"));
        assert_eq!(request.from_slot, Some(777));
    }

    #[test]
    fn laserstream_replay_mode_can_start_from_explicit_slot() {
        let request = LaserStreamConfig::new("https://laserstream.example", "token")
            .with_replay_mode(ProviderReplayMode::FromSlot(321))
            .subscribe_request();
        assert_eq!(request.from_slot, Some(321));
    }

    #[test]
    fn laserstream_live_mode_starts_at_stream_head() {
        let request = LaserStreamConfig::new("https://laserstream.example", "token")
            .with_replay_mode(ProviderReplayMode::Live)
            .subscribe_request();
        assert_eq!(request.from_slot, None);
    }

    #[test]
    fn laserstream_resume_replay_uses_tracked_slot() {
        let request = LaserStreamConfig::new("https://laserstream.example", "token")
            .with_replay_mode(ProviderReplayMode::Resume)
            .subscribe_request_with_state(777);
        assert_eq!(request.from_slot, Some(777));
    }

    #[test]
    fn laserstream_from_slot_reconnect_uses_tracked_slot() {
        let request = LaserStreamConfig::new("https://laserstream.example", "token")
            .with_replay_mode(ProviderReplayMode::FromSlot(321))
            .subscribe_request_with_state(777);
        assert_eq!(request.from_slot, Some(777));
    }

    #[test]
    fn laserstream_subscribe_request_can_target_transaction_status() {
        let request = LaserStreamConfig::new("https://laserstream.example", "token")
            .with_stream(LaserStreamStream::TransactionStatus)
            .subscribe_request_with_state(0);
        assert!(request.transactions.is_empty());
        assert!(request.transactions_status.contains_key("sof"));
        assert!(request.slots.contains_key(INTERNAL_WATERMARK_SLOT_FILTER));
    }

    #[test]
    fn laserstream_subscribe_request_can_target_accounts() {
        let key = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let request = LaserStreamConfig::new("https://laserstream.example", "token")
            .with_stream(LaserStreamStream::Accounts)
            .with_accounts([key])
            .with_owners([owner])
            .require_transaction_signature()
            .subscribe_request_with_state(0);
        let filter = request.accounts.get("sof").expect("accounts filter");
        assert_eq!(filter.account, vec![key.to_string()]);
        assert_eq!(filter.owner, vec![owner.to_string()]);
        assert_eq!(filter.nonempty_txn_signature, Some(true));
        assert!(request.slots.contains_key(INTERNAL_WATERMARK_SLOT_FILTER));
    }

    #[test]
    fn laserstream_subscribe_request_can_target_block_meta() {
        let request = LaserStreamConfig::new("https://laserstream.example", "token")
            .with_stream(LaserStreamStream::BlockMeta)
            .subscribe_request_with_state(0);
        assert!(request.transactions.is_empty());
        assert!(request.accounts.is_empty());
        assert!(request.transactions_status.is_empty());
        assert!(request.blocks_meta.contains_key("sof"));
        assert!(request.slots.contains_key(INTERNAL_WATERMARK_SLOT_FILTER));
    }

    #[tokio::test]
    async fn laserstream_local_source_delivers_transaction_update() {
        let update = grpc::SubscribeUpdate {
            filters: vec!["sof".to_owned()],
            created_at: None,
            update_oneof: Some(grpc::subscribe_update::UpdateOneof::Transaction(
                grpc::SubscribeUpdateTransaction {
                    transaction: Some(sample_update()),
                    slot: 91,
                },
            )),
        };
        let (addr, shutdown_tx, server) = spawn_laserstream_test_server(MockLaserStream {
            expected_stream: MockLaserStreamStream::Transaction,
            expected_account: None,
            expected_owner: None,
            update,
        })
        .await;

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = LaserStreamConfig::new(format!("http://{addr}"), "token")
            .with_stream(LaserStreamStream::Transaction)
            .with_source_instance("laserstream-primary")
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10))
            .with_connect_timeout(Duration::from_secs(2));
        let handle = spawn_laserstream_source(config, tx)
            .await
            .expect("spawn LaserStream transaction source");

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
            "laserstream-primary"
        );

        handle.abort();
        handle.await.ok();
        let _ = shutdown_tx.send(());
        server.await.expect("LaserStream server task");
    }

    #[tokio::test]
    async fn laserstream_source_emits_initial_health_registration() {
        let update = grpc::SubscribeUpdate {
            filters: vec!["sof".to_owned()],
            created_at: None,
            update_oneof: Some(grpc::subscribe_update::UpdateOneof::Ping(
                grpc::SubscribeUpdatePing {},
            )),
        };
        let (addr, shutdown_tx, server) = spawn_laserstream_test_server(MockLaserStream {
            expected_stream: MockLaserStreamStream::Transaction,
            expected_account: None,
            expected_owner: None,
            update,
        })
        .await;

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = LaserStreamConfig::new(format!("http://{addr}"), "token")
            .with_stream(LaserStreamStream::Transaction)
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10))
            .with_connect_timeout(Duration::from_secs(2));
        let handle = spawn_laserstream_source(config, tx)
            .await
            .expect("spawn LaserStream transaction source");

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
        server.await.expect("LaserStream server task");
    }

    #[tokio::test]
    async fn laserstream_local_source_delivers_transaction_status_update() {
        let update = sample_status_update(91);
        let (addr, shutdown_tx, server) = spawn_laserstream_test_server(MockLaserStream {
            expected_stream: MockLaserStreamStream::TransactionStatus,
            expected_account: None,
            expected_owner: None,
            update,
        })
        .await;

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = LaserStreamConfig::new(format!("http://{addr}"), "token")
            .with_stream(LaserStreamStream::TransactionStatus)
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10))
            .with_connect_timeout(Duration::from_secs(2));
        let handle = spawn_laserstream_source(config, tx)
            .await
            .expect("spawn LaserStream status source");

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
        server.await.expect("LaserStream server task");
    }

    #[tokio::test]
    async fn laserstream_local_source_delivers_account_update() {
        let pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let update = sample_account_update(92, pubkey, owner);
        let (addr, shutdown_tx, server) = spawn_laserstream_test_server(MockLaserStream {
            expected_stream: MockLaserStreamStream::Accounts,
            expected_account: Some(pubkey.to_string()),
            expected_owner: Some(owner.to_string()),
            update,
        })
        .await;

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = LaserStreamConfig::new(format!("http://{addr}"), "token")
            .with_stream(LaserStreamStream::Accounts)
            .with_accounts([pubkey])
            .with_owners([owner])
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10))
            .with_connect_timeout(Duration::from_secs(2));
        let handle = spawn_laserstream_source(config, tx)
            .await
            .expect("spawn LaserStream account source");

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
        server.await.expect("LaserStream server task");
    }

    #[tokio::test]
    async fn laserstream_local_slot_source_delivers_slot_status_update() {
        let update = grpc::SubscribeUpdate {
            filters: vec!["sof".to_owned()],
            created_at: None,
            update_oneof: Some(grpc::subscribe_update::UpdateOneof::Slot(
                grpc::SubscribeUpdateSlot {
                    slot: 93,
                    parent: Some(92),
                    status: grpc::SlotStatus::SlotConfirmed as i32,
                    dead_error: Some(String::new()),
                },
            )),
        };
        let (addr, shutdown_tx, server) = spawn_laserstream_test_server(MockLaserStream {
            expected_stream: MockLaserStreamStream::Slots,
            expected_account: None,
            expected_owner: None,
            update,
        })
        .await;

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = LaserStreamSlotsConfig::new(format!("http://{addr}"), "token")
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10))
            .with_connect_timeout(Duration::from_secs(2));
        let handle = spawn_laserstream_slot_source(config, tx)
            .await
            .expect("spawn LaserStream slot source");

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
        server.await.expect("LaserStream server task");
    }

    #[tokio::test]
    async fn laserstream_local_source_delivers_block_meta_update() {
        let update = sample_block_meta_update(94);
        let (addr, shutdown_tx, server) = spawn_laserstream_test_server(MockLaserStream {
            expected_stream: MockLaserStreamStream::BlockMeta,
            expected_account: None,
            expected_owner: None,
            update,
        })
        .await;

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = LaserStreamConfig::new(format!("http://{addr}"), "token")
            .with_stream(LaserStreamStream::BlockMeta)
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10))
            .with_connect_timeout(Duration::from_secs(2));
        let handle = spawn_laserstream_source(config, tx)
            .await
            .expect("spawn LaserStream block-meta source");

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
        server.await.expect("LaserStream server task");
    }

    fn sample_transaction() -> VersionedTransaction {
        let signer = Keypair::new();
        let instructions = [
            Instruction::new_with_bytes(vote::id(), &[], vec![]),
            Instruction::new_with_bytes(system_program::id(), &[], vec![]),
            Instruction::new_with_bytes(compute_budget::id(), &[], vec![]),
        ];
        let message = Message::new(&instructions, Some(&signer.pubkey()));
        VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[&signer]).expect("tx")
    }

    fn sample_vote_transaction() -> VersionedTransaction {
        let signer = Keypair::new();
        let instructions = [
            Instruction::new_with_bytes(vote::id(), &[], vec![]),
            Instruction::new_with_bytes(compute_budget::id(), &[], vec![]),
        ];
        let message = Message::new(&instructions, Some(&signer.pubkey()));
        VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[&signer]).expect("tx")
    }

    fn proto_transaction_from_versioned(tx: &VersionedTransaction) -> LaserStreamTransaction {
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
        LaserStreamTransaction {
            signatures: tx
                .signatures
                .iter()
                .map(|sig| sig.as_ref().to_vec())
                .collect(),
            message: Some(message),
        }
    }

    fn sample_update() -> grpc::SubscribeUpdateTransactionInfo {
        let tx = sample_transaction();
        grpc::SubscribeUpdateTransactionInfo {
            signature: tx.signatures.first().expect("signature").as_ref().to_vec(),
            is_vote: false,
            transaction: Some(proto_transaction_from_versioned(&tx)),
            meta: None,
            index: 0,
        }
    }

    fn sample_vote_update() -> grpc::SubscribeUpdateTransactionInfo {
        let tx = sample_vote_transaction();
        grpc::SubscribeUpdateTransactionInfo {
            signature: tx.signatures.first().expect("signature").as_ref().to_vec(),
            is_vote: true,
            transaction: Some(proto_transaction_from_versioned(&tx)),
            meta: None,
            index: 0,
        }
    }

    fn sample_status_update(slot: u64) -> grpc::SubscribeUpdate {
        let tx = sample_transaction();
        grpc::SubscribeUpdate {
            filters: vec!["sof".to_owned()],
            created_at: None,
            update_oneof: Some(grpc::subscribe_update::UpdateOneof::TransactionStatus(
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

    fn sample_account_update(slot: u64, pubkey: Pubkey, owner: Pubkey) -> grpc::SubscribeUpdate {
        grpc::SubscribeUpdate {
            filters: vec!["sof".to_owned()],
            created_at: None,
            update_oneof: Some(grpc::subscribe_update::UpdateOneof::Account(
                SubscribeUpdateAccount {
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
                },
            )),
        }
    }

    fn sample_block_meta_update(slot: u64) -> grpc::SubscribeUpdate {
        grpc::SubscribeUpdate {
            filters: vec!["sof".to_owned()],
            created_at: None,
            update_oneof: Some(grpc::subscribe_update::UpdateOneof::BlockMeta(
                grpc::SubscribeUpdateBlockMeta {
                    slot,
                    blockhash: Hash::new_unique().to_string(),
                    rewards: None,
                    block_time: Some(UnixTimestamp {
                        timestamp: 1_700_000_000 + i64::try_from(slot).unwrap_or(0),
                    }),
                    block_height: Some(BlockHeight {
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
    enum MockLaserStreamStream {
        Transaction,
        TransactionStatus,
        Accounts,
        BlockMeta,
        Slots,
    }

    #[derive(Clone)]
    struct MockLaserStream {
        expected_stream: MockLaserStreamStream,
        expected_account: Option<String>,
        expected_owner: Option<String>,
        update: grpc::SubscribeUpdate,
    }

    #[async_trait::async_trait]
    impl Geyser for MockLaserStream {
        type SubscribeStream =
            Pin<Box<dyn Stream<Item = Result<SubscribeUpdate, Status>> + Send + 'static>>;
        type SubscribePreprocessedStream = Pin<
            Box<dyn Stream<Item = Result<SubscribePreprocessedUpdate, Status>> + Send + 'static>,
        >;

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
                MockLaserStreamStream::Transaction => {
                    assert!(first.transactions.contains_key("sof"));
                }
                MockLaserStreamStream::TransactionStatus => {
                    assert!(first.transactions_status.contains_key("sof"));
                }
                MockLaserStreamStream::Accounts => {
                    let filter = first
                        .accounts
                        .get("sof")
                        .expect("expected LaserStream accounts filter");
                    if let Some(account) = &self.expected_account {
                        assert!(filter.account.iter().any(|value| value == account));
                    }
                    if let Some(owner) = &self.expected_owner {
                        assert!(filter.owner.iter().any(|value| value == owner));
                    }
                }
                MockLaserStreamStream::BlockMeta => {
                    assert!(first.blocks_meta.contains_key("sof"));
                }
                MockLaserStreamStream::Slots => {
                    assert!(first.slots.contains_key("sof"));
                }
            }
            let (tx, rx) = futures_mpsc::unbounded();
            tx.unbounded_send(Ok(self.update.clone()))
                .expect("send LaserStream test update");
            Ok(Response::new(Box::pin(rx)))
        }

        async fn subscribe_preprocessed(
            &self,
            _request: Request<tonic::Streaming<SubscribePreprocessedRequest>>,
        ) -> Result<Response<Self::SubscribePreprocessedStream>, Status> {
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

    async fn spawn_laserstream_test_server(
        service: MockLaserStream,
    ) -> (
        std::net::SocketAddr,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind LaserStream test");
        let addr = listener.local_addr().expect("LaserStream test addr");
        drop(listener);

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            Server::builder()
                .add_service(GeyserServer::new(service))
                .serve_with_shutdown(addr, async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("run LaserStream test server");
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        (addr, shutdown_tx, handle)
    }

    fn transaction_event_from_update_baseline(
        slot: u64,
        transaction: Option<SubscribeUpdateTransactionInfo>,
        commitment_status: TxCommitmentStatus,
    ) -> Result<TransactionEvent, LaserStreamError> {
        let transaction =
            transaction.ok_or(LaserStreamError::Convert("missing transaction payload"))?;
        let signature = Signature::try_from(transaction.signature.as_slice())
            .map(crate::framework::signature_bytes)
            .map(Some)
            .map_err(|_error| LaserStreamError::Convert("invalid signature"))?;
        let tx = convert_transaction(
            transaction
                .transaction
                .ok_or(LaserStreamError::Convert("missing versioned transaction"))?,
        )?;
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

    fn convert_transaction_sdk_baseline(
        tx: LaserStreamTransaction,
    ) -> Result<VersionedTransaction, LaserStreamError> {
        let mut signatures = Vec::with_capacity(tx.signatures.len());
        for signature in tx.signatures {
            signatures.push(Signature::try_from(signature.as_slice()).map_err(|_error| {
                LaserStreamError::Convert("failed to parse transaction signature")
            })?);
        }

        let message = tx
            .message
            .ok_or(LaserStreamError::Convert("missing transaction message"))?;
        let header = message
            .header
            .ok_or(LaserStreamError::Convert("missing message header"))?;
        let header = MessageHeader {
            num_required_signatures: header
                .num_required_signatures
                .try_into()
                .map_err(|_error| LaserStreamError::Convert("invalid num_required_signatures"))?,
            num_readonly_signed_accounts: header.num_readonly_signed_accounts.try_into().map_err(
                |_error| LaserStreamError::Convert("invalid num_readonly_signed_accounts"),
            )?,
            num_readonly_unsigned_accounts: header
                .num_readonly_unsigned_accounts
                .try_into()
                .map_err(|_error| {
                    LaserStreamError::Convert("invalid num_readonly_unsigned_accounts")
                })?,
        };

        if message.recent_blockhash.len() != 32 {
            return Err(LaserStreamError::Convert("invalid recent blockhash"));
        }

        let recent_blockhash = Hash::new_from_array(
            <[u8; 32]>::try_from(message.recent_blockhash.as_slice())
                .map_err(|_error| LaserStreamError::Convert("invalid recent blockhash"))?,
        );

        let account_keys = {
            let mut keys = Vec::with_capacity(message.account_keys.len());
            for key in message.account_keys {
                keys.push(
                    Pubkey::try_from(key.as_slice())
                        .map_err(|_error| LaserStreamError::Convert("invalid account key"))?,
                );
            }
            keys
        };

        let instructions = {
            let mut compiled = Vec::with_capacity(message.instructions.len());
            for instruction in message.instructions {
                compiled.push(CompiledInstruction {
                    program_id_index: instruction.program_id_index.try_into().map_err(
                        |_error| {
                            LaserStreamError::Convert(
                                "invalid compiled instruction program id index",
                            )
                        },
                    )?,
                    accounts: instruction.accounts,
                    data: instruction.data,
                });
            }
            compiled
        };

        let message = if message.versioned {
            let mut address_table_lookups = Vec::with_capacity(message.address_table_lookups.len());
            for lookup in message.address_table_lookups {
                address_table_lookups.push(MessageAddressTableLookup {
                    account_key: Pubkey::try_from(lookup.account_key.as_slice()).map_err(
                        |_error| LaserStreamError::Convert("invalid address table account key"),
                    )?,
                    writable_indexes: lookup.writable_indexes,
                    readonly_indexes: lookup.readonly_indexes,
                });
            }
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
        };

        Ok(VersionedTransaction {
            signatures,
            message,
        })
    }

    #[tokio::test]
    async fn laserstream_spawn_rejects_account_filters_for_block_meta_stream() {
        let (tx, _rx) = crate::provider_stream::create_provider_stream_queue(1);
        let config = LaserStreamConfig::new("http://127.0.0.1:1", "test-api-key")
            .with_stream(LaserStreamStream::BlockMeta)
            .with_accounts([Pubkey::new_unique()]);

        let error = spawn_laserstream_source(config, tx)
            .await
            .expect_err("block-meta stream should reject account filters");

        match error {
            LaserStreamError::Config(LaserStreamConfigError::UnsupportedOption {
                option: LaserStreamConfigOption::Accounts,
                stream: LaserStreamStreamKind::BlockMeta,
            }) => {}
            other => panic!("expected accounts config rejection, got {other:?}"),
        }
    }

    #[test]
    fn laserstream_local_conversion_matches_sdk_baseline() {
        let tx = proto_transaction_from_versioned(&sample_transaction());
        let local = convert_transaction(tx.clone()).expect("local tx");
        let baseline = convert_transaction_sdk_baseline(tx).expect("baseline tx");
        assert_eq!(local, baseline);
    }

    #[test]
    #[ignore = "profiling fixture for local-vs-sdk-like LaserStream tx conversion"]
    fn laserstream_local_conversion_profile_fixture() {
        let iterations = profile_iterations(200_000);
        let tx = proto_transaction_from_versioned(&sample_transaction());

        let baseline_started = Instant::now();
        for _ in 0..iterations {
            let tx = convert_transaction_sdk_baseline(tx.clone()).expect("baseline tx");
            std::hint::black_box(tx);
        }
        let baseline_elapsed = baseline_started.elapsed();

        let optimized_started = Instant::now();
        for _ in 0..iterations {
            let tx = convert_transaction(tx.clone()).expect("optimized tx");
            std::hint::black_box(tx);
        }
        let optimized_elapsed = optimized_started.elapsed();

        eprintln!(
            "laserstream_local_conversion_profile_fixture iterations={} baseline_us={} optimized_us={}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
        );
    }

    #[test]
    fn laserstream_transaction_event_from_update_decodes_transaction() {
        let event = transaction_event_from_update(
            77,
            Some(sample_update()),
            TxCommitmentStatus::Confirmed,
            ProviderCommitmentWatermarks::default(),
        )
        .expect("event");
        assert_eq!(event.slot, 77);
        assert_eq!(event.kind, crate::event::TxKind::Mixed);
        assert!(event.signature.is_some());
    }

    #[test]
    fn laserstream_transaction_event_from_update_shortcuts_vote_only() {
        let event = transaction_event_from_update(
            78,
            Some(sample_vote_update()),
            TxCommitmentStatus::Confirmed,
            ProviderCommitmentWatermarks::default(),
        )
        .expect("event");
        assert_eq!(event.slot, 78);
        assert_eq!(event.kind, crate::event::TxKind::VoteOnly);
        assert!(event.signature.is_some());
    }

    #[test]
    #[ignore = "profiling fixture for LaserStream provider transaction conversion A/B"]
    fn laserstream_transaction_conversion_profile_fixture() {
        let iterations = profile_iterations(200_000);

        let update = sample_update();

        let baseline_started = Instant::now();
        for _ in 0..iterations {
            let event = transaction_event_from_update_baseline(
                77,
                Some(update.clone()),
                TxCommitmentStatus::Processed,
            )
            .expect("baseline event");
            std::hint::black_box(event);
        }
        let baseline_elapsed = baseline_started.elapsed();

        let optimized_started = Instant::now();
        for _ in 0..iterations {
            let event = transaction_event_from_update(
                77,
                Some(update.clone()),
                TxCommitmentStatus::Processed,
                ProviderCommitmentWatermarks::default(),
            )
            .expect("optimized event");
            std::hint::black_box(event);
        }
        let optimized_elapsed = optimized_started.elapsed();

        eprintln!(
            "laserstream_transaction_conversion_profile_fixture iterations={} baseline_us={} optimized_us={}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
        );
    }

    #[test]
    #[ignore = "profiling fixture for baseline LaserStream transaction conversion"]
    fn laserstream_transaction_conversion_baseline_profile_fixture() {
        let iterations = profile_iterations(200_000);

        let update = sample_update();
        for _ in 0..iterations {
            let event = transaction_event_from_update_baseline(
                77,
                Some(update.clone()),
                TxCommitmentStatus::Processed,
            )
            .expect("baseline event");
            std::hint::black_box(event);
        }
    }

    #[test]
    #[ignore = "profiling fixture for optimized LaserStream transaction conversion"]
    fn laserstream_transaction_conversion_optimized_profile_fixture() {
        let iterations = profile_iterations(200_000);

        let update = sample_update();
        for _ in 0..iterations {
            let event = transaction_event_from_update(
                77,
                Some(update.clone()),
                TxCommitmentStatus::Processed,
                ProviderCommitmentWatermarks::default(),
            )
            .expect("optimized event");
            std::hint::black_box(event);
        }
    }

    #[test]
    #[ignore = "profiling fixture for LaserStream vote-only conversion A/B"]
    fn laserstream_vote_only_conversion_profile_fixture() {
        let iterations = profile_iterations(200_000);

        let update = sample_vote_update();

        let baseline_started = Instant::now();
        for _ in 0..iterations {
            let event = transaction_event_from_update_baseline(
                78,
                Some(update.clone()),
                TxCommitmentStatus::Processed,
            )
            .expect("baseline event");
            std::hint::black_box(event);
        }
        let baseline_elapsed = baseline_started.elapsed();

        let optimized_started = Instant::now();
        for _ in 0..iterations {
            let event = transaction_event_from_update(
                78,
                Some(update.clone()),
                TxCommitmentStatus::Processed,
                ProviderCommitmentWatermarks::default(),
            )
            .expect("optimized event");
            std::hint::black_box(event);
        }
        let optimized_elapsed = optimized_started.elapsed();

        eprintln!(
            "laserstream_vote_only_conversion_profile_fixture iterations={} baseline_us={} optimized_us={}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
        );
    }
}
