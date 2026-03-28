#![allow(clippy::missing_docs_in_private_items)]

//! Websocket processed-provider adapters for SOF provider-stream ingress.
//!
//! This module covers websocket transaction, logs, account, and program feeds.
//! Transaction subscriptions keep the same transaction semantics as Yellowstone
//! and LaserStream by requesting full base64 transaction payloads and converting
//! them into [`crate::framework::TransactionEvent`] values before dispatch.

use std::{borrow::Cow, str::FromStr, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use simd_json::{Buffers as SimdJsonBuffers, serde::from_slice as simd_from_slice};
use sof_types::{PubkeyBytes, SignatureBytes};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async, tungstenite::protocol::Message as WsMessage,
};

use crate::{
    event::TxCommitmentStatus,
    framework::{AccountUpdateEvent, TransactionEvent, pubkey_bytes, signature_bytes_opt},
    provider_stream::{
        ProviderCommitmentWatermarks, ProviderSourceHealthEvent, ProviderSourceHealthReason,
        ProviderSourceHealthStatus, ProviderSourceId, ProviderSourceIdentity,
        ProviderSourceIdentityRegistrationError, ProviderSourceReadiness,
        ProviderSourceReservation, ProviderSourceTaskGuard, ProviderStreamFanIn,
        ProviderStreamMode, ProviderStreamSender, ProviderStreamUpdate, SerializedTransactionEvent,
        classify_provider_transaction_kind, emit_provider_source_removed_with_reservation,
    },
};

/// Commitment level used for websocket `transactionSubscribe` notifications.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WebsocketTransactionCommitment {
    /// `processed`
    #[default]
    Processed,
    /// `confirmed`
    Confirmed,
    /// `finalized`
    Finalized,
}

impl WebsocketTransactionCommitment {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Processed => "processed",
            Self::Confirmed => "confirmed",
            Self::Finalized => "finalized",
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

/// Connection and filter config for websocket `transactionSubscribe`.
#[derive(Clone, Debug)]
pub struct WebsocketTransactionConfig {
    endpoint: String,
    http_endpoint: Option<String>,
    source_instance: Option<Arc<str>>,
    readiness: ProviderSourceReadiness,
    stream: WebsocketPrimaryStream,
    program_filters: Vec<WebsocketProgramFilter>,
    commitment: WebsocketTransactionCommitment,
    vote: Option<bool>,
    failed: Option<bool>,
    signature: Option<Signature>,
    account_include: Vec<Pubkey>,
    account_exclude: Vec<Pubkey>,
    account_required: Vec<Pubkey>,
    ping_interval: Option<Duration>,
    stall_timeout: Option<Duration>,
    reconnect_delay: Duration,
    max_reconnect_attempts: Option<u32>,
    replay_on_reconnect: bool,
    replay_on_reconnect_explicit: bool,
    replay_max_slots: u64,
    replay_max_slots_explicit: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// One stream-specific websocket config option that may be invalid for a selected stream kind.
pub enum WebsocketConfigOption {
    /// HTTP companion endpoint used for reconnect backfill.
    HttpEndpoint,
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
    /// Replay-on-reconnect toggle.
    ReplayOnReconnect,
    /// Replay window in slots.
    ReplayMaxSlots,
    /// `programSubscribe` filter list.
    ProgramFilters,
}

impl std::fmt::Display for WebsocketConfigOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HttpEndpoint => f.write_str("http replay endpoint"),
            Self::VoteFilter => f.write_str("vote filter"),
            Self::FailedFilter => f.write_str("failed filter"),
            Self::SignatureFilter => f.write_str("signature filter"),
            Self::AccountIncludeFilter => f.write_str("account-include filter"),
            Self::AccountExcludeFilter => f.write_str("account-exclude filter"),
            Self::AccountRequiredFilter => f.write_str("account-required filter"),
            Self::ReplayOnReconnect => f.write_str("replay-on-reconnect"),
            Self::ReplayMaxSlots => f.write_str("replay-max-slots"),
            Self::ProgramFilters => f.write_str("program filters"),
        }
    }
}

#[derive(Debug, Error)]
/// Typed websocket config validation failures for built-in stream selectors.
pub enum WebsocketConfigError {
    /// The selected websocket stream kind does not support one configured option.
    #[error("{option} is not supported for websocket {stream} streams")]
    UnsupportedOption {
        /// Built-in websocket stream family receiving the invalid option.
        stream: WebsocketStreamKind,
        /// Typed config option that is unsupported for that stream family.
        option: WebsocketConfigOption,
    },
}

impl WebsocketTransactionConfig {
    /// Creates a websocket transaction-stream config for one endpoint.
    ///
    /// By default no vote/failed filter is applied, so the stream remains
    /// inclusive unless you narrow it explicitly.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sof::provider_stream::websocket::WebsocketTransactionConfig;
    ///
    /// let config = WebsocketTransactionConfig::new("wss://example.invalid");
    /// assert_eq!(config.endpoint(), "wss://example.invalid");
    /// ```
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            http_endpoint: None,
            source_instance: None,
            readiness: ProviderSourceReadiness::Required,
            stream: WebsocketPrimaryStream::Transaction,
            program_filters: Vec::new(),
            commitment: WebsocketTransactionCommitment::Processed,
            vote: None,
            failed: None,
            signature: None,
            account_include: Vec::new(),
            account_exclude: Vec::new(),
            account_required: Vec::new(),
            ping_interval: Some(Duration::from_secs(60)),
            stall_timeout: Some(Duration::from_secs(30)),
            reconnect_delay: Duration::from_secs(1),
            max_reconnect_attempts: None,
            replay_on_reconnect: true,
            replay_on_reconnect_explicit: false,
            replay_max_slots: 128,
            replay_max_slots_explicit: false,
        }
    }

    /// Returns the configured websocket endpoint.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Returns the configured HTTP RPC endpoint used for reconnect backfill.
    #[must_use]
    pub fn http_endpoint(&self) -> Option<&str> {
        self.http_endpoint.as_deref()
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

    /// Selects the websocket subscription family this config should use.
    #[must_use]
    pub const fn with_stream(mut self, stream: WebsocketPrimaryStream) -> Self {
        self.stream = stream;
        self
    }

    /// Sets the HTTP RPC endpoint used for reconnect backfill.
    #[must_use]
    pub fn with_http_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.http_endpoint = Some(endpoint.into());
        self
    }

    /// Sets the commitment level.
    #[must_use]
    pub const fn with_commitment(mut self, commitment: WebsocketTransactionCommitment) -> Self {
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

    /// Sets the websocket keepalive ping interval.
    #[must_use]
    pub const fn with_ping_interval(mut self, interval: Duration) -> Self {
        self.ping_interval = Some(interval);
        self
    }

    /// Sets the idle watchdog timeout for one websocket session.
    #[must_use]
    pub const fn with_stall_timeout(mut self, timeout: Duration) -> Self {
        self.stall_timeout = Some(timeout);
        self
    }

    /// Sets the reconnect backoff used after websocket failures.
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

    /// Enables or disables best-effort HTTP backfill after reconnect.
    #[must_use]
    pub const fn with_replay_on_reconnect(mut self, replay: bool) -> Self {
        self.replay_on_reconnect = replay;
        self.replay_on_reconnect_explicit = true;
        self
    }

    /// Sets the maximum reconnect backfill window in slots.
    #[must_use]
    pub const fn with_replay_max_slots(mut self, slots: u64) -> Self {
        self.replay_max_slots = slots;
        self.replay_max_slots_explicit = true;
        self
    }

    /// Adds program subscription filters used with `programSubscribe`.
    #[must_use]
    pub fn with_program_filters<I>(mut self, filters: I) -> Self
    where
        I: IntoIterator<Item = WebsocketProgramFilter>,
    {
        self.program_filters.extend(filters);
        self
    }

    fn validate(&self) -> Result<(), WebsocketConfigError> {
        match self.stream {
            WebsocketPrimaryStream::Transaction => {
                if !self.program_filters.is_empty() {
                    return Err(WebsocketConfigError::UnsupportedOption {
                        stream: self.stream_kind(),
                        option: WebsocketConfigOption::ProgramFilters,
                    });
                }
            }
            WebsocketPrimaryStream::Account(_) => {
                self.validate_non_transaction_stream()?;
                if !self.program_filters.is_empty() {
                    return Err(WebsocketConfigError::UnsupportedOption {
                        stream: self.stream_kind(),
                        option: WebsocketConfigOption::ProgramFilters,
                    });
                }
            }
            WebsocketPrimaryStream::Program(_) => {
                self.validate_non_transaction_stream()?;
            }
        }
        Ok(())
    }

    fn validate_non_transaction_stream(&self) -> Result<(), WebsocketConfigError> {
        if let Some(option) = [
            self.http_endpoint
                .as_ref()
                .map(|_| WebsocketConfigOption::HttpEndpoint),
            self.vote.map(|_| WebsocketConfigOption::VoteFilter),
            self.failed.map(|_| WebsocketConfigOption::FailedFilter),
            self.signature
                .map(|_| WebsocketConfigOption::SignatureFilter),
            (!self.account_include.is_empty())
                .then_some(WebsocketConfigOption::AccountIncludeFilter),
            (!self.account_exclude.is_empty())
                .then_some(WebsocketConfigOption::AccountExcludeFilter),
            (!self.account_required.is_empty())
                .then_some(WebsocketConfigOption::AccountRequiredFilter),
            (self.replay_on_reconnect_explicit && self.replay_on_reconnect)
                .then_some(WebsocketConfigOption::ReplayOnReconnect),
            self.replay_max_slots_explicit
                .then_some(WebsocketConfigOption::ReplayMaxSlots),
        ]
        .into_iter()
        .flatten()
        .next()
        {
            return Err(WebsocketConfigError::UnsupportedOption {
                stream: self.stream_kind(),
                option,
            });
        }
        Ok(())
    }

    pub(crate) fn subscribe_request(&self) -> Value {
        match self.stream {
            WebsocketPrimaryStream::Transaction => {
                let mut filter = serde_json::Map::new();
                if let Some(vote) = self.vote {
                    filter.insert("vote".to_owned(), Value::Bool(vote));
                }
                if let Some(failed) = self.failed {
                    filter.insert("failed".to_owned(), Value::Bool(failed));
                }
                if let Some(signature) = self.signature {
                    filter.insert("signature".to_owned(), Value::String(signature.to_string()));
                }
                if !self.account_include.is_empty() {
                    filter.insert(
                        "accountInclude".to_owned(),
                        Value::Array(
                            self.account_include
                                .iter()
                                .map(|key| Value::String(key.to_string()))
                                .collect(),
                        ),
                    );
                }
                if !self.account_exclude.is_empty() {
                    filter.insert(
                        "accountExclude".to_owned(),
                        Value::Array(
                            self.account_exclude
                                .iter()
                                .map(|key| Value::String(key.to_string()))
                                .collect(),
                        ),
                    );
                }
                if !self.account_required.is_empty() {
                    filter.insert(
                        "accountRequired".to_owned(),
                        Value::Array(
                            self.account_required
                                .iter()
                                .map(|key| Value::String(key.to_string()))
                                .collect(),
                        ),
                    );
                }
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "transactionSubscribe",
                    "params": [
                        Value::Object(filter),
                        {
                            "commitment": self.commitment.as_str(),
                            "encoding": "base64",
                            "transactionDetails": "full",
                            "maxSupportedTransactionVersion": 0
                        }
                    ]
                })
            }
            WebsocketPrimaryStream::Account(pubkey) => json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "accountSubscribe",
                "params": [
                    pubkey.to_string(),
                    {
                        "commitment": self.commitment.as_str(),
                        "encoding": "base64"
                    }
                ]
            }),
            WebsocketPrimaryStream::Program(program_id) => {
                let filters = self
                    .program_filters
                    .iter()
                    .map(WebsocketProgramFilter::as_json)
                    .collect::<Vec<_>>();
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "programSubscribe",
                    "params": [
                        program_id.to_string(),
                        {
                            "commitment": self.commitment.as_str(),
                            "encoding": "base64",
                            "filters": filters
                        }
                    ]
                })
            }
        }
    }

    /// Returns the runtime mode matching this built-in websocket stream selection.
    #[must_use]
    pub const fn runtime_mode(&self) -> ProviderStreamMode {
        match self.stream {
            WebsocketPrimaryStream::Transaction => ProviderStreamMode::WebsocketTransaction,
            WebsocketPrimaryStream::Account(_) => ProviderStreamMode::WebsocketAccount,
            WebsocketPrimaryStream::Program(_) => ProviderStreamMode::WebsocketProgram,
        }
    }

    const fn source_id(&self) -> ProviderSourceId {
        match self.stream {
            WebsocketPrimaryStream::Transaction => ProviderSourceId::WebsocketTransaction,
            WebsocketPrimaryStream::Account(_) => ProviderSourceId::WebsocketAccount,
            WebsocketPrimaryStream::Program(_) => ProviderSourceId::WebsocketProgram,
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

    const fn stream_kind(&self) -> WebsocketStreamKind {
        match self.stream {
            WebsocketPrimaryStream::Transaction => WebsocketStreamKind::Transaction,
            WebsocketPrimaryStream::Account(_) => WebsocketStreamKind::Account,
            WebsocketPrimaryStream::Program(_) => WebsocketStreamKind::Program,
        }
    }
}

/// Primary websocket subscription families supported by the built-in adapter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WebsocketPrimaryStream {
    /// `transactionSubscribe`
    #[default]
    Transaction,
    /// `accountSubscribe`
    Account(Pubkey),
    /// `programSubscribe`
    Program(Pubkey),
}

/// One websocket `programSubscribe` filter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebsocketProgramFilter {
    /// Filter accounts by exact data size.
    DataSize(u64),
    /// Filter accounts by a base58 memcmp at one byte offset.
    MemcmpBase58 {
        /// Offset inside the account data.
        offset: u64,
        /// Base58-encoded comparison bytes.
        bytes: String,
    },
}

impl WebsocketProgramFilter {
    fn as_json(&self) -> Value {
        match self {
            Self::DataSize(size) => json!({ "dataSize": size }),
            Self::MemcmpBase58 { offset, bytes } => json!({
                "memcmp": {
                    "offset": offset,
                    "bytes": bytes
                }
            }),
        }
    }
}

/// Filter shape for websocket `logsSubscribe`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WebsocketLogsFilter {
    /// Subscribe to all transaction logs except simple vote transactions.
    #[default]
    All,
    /// Subscribe to all transaction logs including vote transactions.
    AllWithVotes,
    /// Subscribe to logs for one mentioned account/program pubkey.
    Mentions(Pubkey),
}

/// Connection and filter config for websocket `logsSubscribe`.
#[derive(Clone, Debug)]
pub struct WebsocketLogsConfig {
    endpoint: String,
    source_instance: Option<Arc<str>>,
    readiness: ProviderSourceReadiness,
    commitment: WebsocketTransactionCommitment,
    filter: WebsocketLogsFilter,
    ping_interval: Option<Duration>,
    stall_timeout: Option<Duration>,
    reconnect_delay: Duration,
    max_reconnect_attempts: Option<u32>,
}

impl WebsocketLogsConfig {
    /// Creates a websocket logs-stream config for one endpoint.
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            source_instance: None,
            readiness: ProviderSourceReadiness::Optional,
            commitment: WebsocketTransactionCommitment::Processed,
            filter: WebsocketLogsFilter::All,
            ping_interval: Some(Duration::from_secs(60)),
            stall_timeout: Some(Duration::from_secs(30)),
            reconnect_delay: Duration::from_secs(1),
            max_reconnect_attempts: None,
        }
    }

    /// Returns the configured websocket endpoint.
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

    /// Sets the commitment level.
    #[must_use]
    pub const fn with_commitment(mut self, commitment: WebsocketTransactionCommitment) -> Self {
        self.commitment = commitment;
        self
    }

    /// Sets the logs filter.
    #[must_use]
    pub const fn with_filter(mut self, filter: WebsocketLogsFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Sets the websocket keepalive ping interval.
    #[must_use]
    pub const fn with_ping_interval(mut self, interval: Duration) -> Self {
        self.ping_interval = Some(interval);
        self
    }

    /// Sets the idle watchdog timeout for one websocket session.
    #[must_use]
    pub const fn with_stall_timeout(mut self, timeout: Duration) -> Self {
        self.stall_timeout = Some(timeout);
        self
    }

    /// Sets the reconnect backoff used after websocket failures.
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

    fn subscribe_request(&self) -> Value {
        let filter = match self.filter {
            WebsocketLogsFilter::All => Value::String("all".to_owned()),
            WebsocketLogsFilter::AllWithVotes => Value::String("allWithVotes".to_owned()),
            WebsocketLogsFilter::Mentions(pubkey) => json!({ "mentions": [pubkey.to_string()] }),
        };
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "logsSubscribe",
            "params": [
                filter,
                {
                    "commitment": self.commitment.as_str()
                }
            ]
        })
    }

    /// Returns the runtime mode matching this built-in websocket logs stream.
    #[must_use]
    pub const fn runtime_mode(&self) -> ProviderStreamMode {
        ProviderStreamMode::WebsocketLogs
    }

    fn source_instance(&self) -> Option<&str> {
        self.source_instance.as_deref()
    }

    const fn readiness(&self) -> ProviderSourceReadiness {
        self.readiness
    }

    fn source_identity(&self) -> Option<ProviderSourceIdentity> {
        self.source_instance()
            .map(|instance| ProviderSourceIdentity::new(ProviderSourceId::WebsocketLogs, instance))
    }
}

/// Websocket `transactionSubscribe` error surface.
#[derive(Debug, Error)]
pub enum WebsocketTransactionError {
    /// Invalid websocket config for the selected stream kind.
    #[error(transparent)]
    Config(#[from] WebsocketConfigError),
    /// Websocket transport failure.
    #[error(transparent)]
    Transport(#[from] tokio_tungstenite::tungstenite::Error),
    /// Upstream payload shape/protocol failure.
    #[error(transparent)]
    Protocol(#[from] WebsocketProtocolError),
    /// Provider payload could not be converted into a SOF transaction event.
    #[error("websocket transaction conversion failed: {0}")]
    Convert(&'static str),
    /// Provider-stream queue is closed.
    #[error("provider-stream queue closed")]
    QueueClosed,
    /// One duplicated stable source identity was registered in the same fan-in.
    #[error(transparent)]
    DuplicateSourceIdentity(#[from] ProviderSourceIdentityRegistrationError),
}

/// Websocket `logsSubscribe` error surface.
#[derive(Debug, Error)]
pub enum WebsocketLogsError {
    /// Websocket transport failure.
    #[error(transparent)]
    Transport(#[from] tokio_tungstenite::tungstenite::Error),
    /// Upstream payload shape/protocol failure.
    #[error(transparent)]
    Protocol(#[from] WebsocketProtocolError),
    /// Provider-stream queue is closed.
    #[error("provider-stream queue closed")]
    QueueClosed,
    /// One duplicated stable source identity was registered in the same fan-in.
    #[error(transparent)]
    DuplicateSourceIdentity(#[from] ProviderSourceIdentityRegistrationError),
}

/// Typed websocket protocol/runtime failures used by both transaction and log feeds.
#[derive(Debug, Error)]
pub enum WebsocketProtocolError {
    /// Subscription ack never arrived.
    #[error("timed out waiting for websocket subscription ack")]
    SubscriptionAckTimeout,
    /// Socket closed before subscription ack arrived.
    #[error("websocket closed before subscription ack")]
    ClosedBeforeSubscriptionAck,
    /// Socket closed before subscription ack, with close-frame detail.
    #[error("websocket closed before subscription ack: {0}")]
    ClosedBeforeSubscriptionAckWithFrame(String),
    /// One websocket stream stopped making progress.
    #[error("{stream} stream stalled without inbound progress")]
    StreamStalled {
        /// Typed websocket stream kind that stalled.
        stream: WebsocketStreamKind,
    },
    /// The websocket closed after subscription succeeded.
    #[error("websocket closed: {0}")]
    Closed(String),
    /// Provider sent invalid JSON.
    #[error("invalid websocket json: {0}")]
    InvalidJson(String),
    /// Upstream rejected the subscribe request.
    #[error("websocket subscription error: {0}")]
    SubscriptionError(String),
    /// Upstream sent a provider-level error payload.
    #[error("websocket provider error: {0}")]
    ProviderError(String),
    /// Parsed log payload had an invalid signature string.
    #[error("invalid websocket log signature")]
    InvalidLogSignature,
    /// HTTP RPC companion call failed.
    #[error("http rpc {method} failed: {detail}")]
    HttpRpcFailed {
        /// RPC method name, for example `getSlot`.
        method: &'static str,
        /// Upstream failure detail.
        detail: String,
    },
    /// HTTP RPC companion call decoded to an invalid payload.
    #[error("http rpc {method} decode failed: {detail}")]
    HttpRpcDecodeFailed {
        /// RPC method name, for example `getBlock`.
        method: &'static str,
        /// Decode failure detail.
        detail: String,
    },
    /// HTTP RPC companion call returned no result.
    #[error("http rpc {method} returned no result")]
    HttpRpcMissingResult {
        /// RPC method name that returned no result.
        method: &'static str,
    },
    /// Best-effort replay requires an HTTP RPC endpoint.
    #[error("websocket reconnect replay requires a matching http rpc endpoint")]
    MissingReplayHttpEndpoint,
    /// Reconnect attempts were exhausted.
    ///
    /// `attempts` is the consecutive-failure count that tripped the budget.
    #[error("exhausted websocket reconnect attempts after {attempts} failures")]
    ReconnectBudgetExhausted {
        /// Consecutive-failure count that exhausted the reconnect budget.
        attempts: u32,
    },
}

/// Stable built-in websocket stream kinds used by typed websocket errors.
#[derive(Clone, Copy, Debug)]
pub enum WebsocketStreamKind {
    /// `transactionSubscribe`
    Transaction,
    /// `logsSubscribe`
    Logs,
    /// `accountSubscribe`
    Account,
    /// `programSubscribe`
    Program,
}

impl std::fmt::Display for WebsocketStreamKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transaction => f.write_str("websocket transaction"),
            Self::Logs => f.write_str("websocket logs"),
            Self::Account => f.write_str("websocket account"),
            Self::Program => f.write_str("websocket program"),
        }
    }
}

const PROVIDER_SUBSCRIPTION_ACKNOWLEDGED: &str = "subscription acknowledged";

type WebsocketProviderStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Spawns one websocket primary source into a SOF provider-stream queue.
///
/// # Examples
///
/// ```no_run
/// use sof::provider_stream::{
///     create_provider_stream_queue,
///     websocket::{spawn_websocket_source, WebsocketTransactionConfig},
/// };
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let (tx, _rx) = create_provider_stream_queue(1024);
/// let config = WebsocketTransactionConfig::new("wss://mainnet.helius-rpc.com/?api-key=example");
/// let handle = spawn_websocket_source(&config, tx).await?;
/// handle.abort();
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// Returns any connection/bootstrap error before the forwarder task starts.
pub async fn spawn_websocket_source(
    config: &WebsocketTransactionConfig,
    sender: ProviderStreamSender,
) -> Result<JoinHandle<Result<(), WebsocketTransactionError>>, WebsocketTransactionError> {
    spawn_websocket_source_inner(config, sender, None).await
}

#[deprecated(note = "use spawn_websocket_source")]
/// Deprecated compatibility shim for [`spawn_websocket_source`].
///
/// # Errors
///
/// Returns the same startup validation or bootstrap error as
/// [`spawn_websocket_source`].
pub async fn spawn_websocket_transaction_source(
    config: &WebsocketTransactionConfig,
    sender: ProviderStreamSender,
) -> Result<JoinHandle<Result<(), WebsocketTransactionError>>, WebsocketTransactionError> {
    spawn_websocket_source(config, sender).await
}

async fn spawn_websocket_source_inner(
    config: &WebsocketTransactionConfig,
    sender: ProviderStreamSender,
    reservation: Option<Arc<ProviderSourceReservation>>,
) -> Result<JoinHandle<Result<(), WebsocketTransactionError>>, WebsocketTransactionError> {
    config.validate()?;
    let config = config.clone();
    let source = ProviderSourceIdentity::generated(config.source_id(), config.source_instance());
    let initial_health = queue_primary_provider_health(
        &source,
        config.readiness(),
        &sender,
        ProviderSourceHealthStatus::Reconnecting,
        ProviderSourceHealthReason::InitialConnectPending,
        format!(
            "waiting for first websocket {} session ack",
            source.kind_str().trim_start_matches("websocket_")
        ),
    )?;
    let first_session = match establish_websocket_primary_session(&config).await {
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
                .map_err(|_error| WebsocketTransactionError::QueueClosed)?;
        }
        let mut attempts = 0_u32;
        let mut last_seen_slot = None;
        let mut watermarks = ProviderCommitmentWatermarks::default();
        let mut first_session = Some(first_session);
        loop {
            let mut session_established = false;
            let session = match first_session.take() {
                Some(session) => Ok(session),
                None => establish_websocket_primary_session(&config).await,
            };
            match session {
                Ok(session) => match run_websocket_primary_connection(
                    &config,
                    &source,
                    &sender,
                    &mut last_seen_slot,
                    &mut watermarks,
                    &mut session_established,
                    session,
                )
                .await
                {
                    Ok(()) => {
                        let detail = format!("{} stream ended unexpectedly", config.stream_kind());
                        tracing::warn!(
                            detail,
                            endpoint = config.endpoint(),
                            "provider stream websocket session ended; reconnecting"
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
                    Err(WebsocketTransactionError::QueueClosed) => {
                        return Err(WebsocketTransactionError::QueueClosed);
                    }
                    Err(error) => {
                        tracing::warn!(%error, endpoint = config.endpoint(), "provider stream websocket session ended; reconnecting");
                        send_primary_provider_health(
                            &source,
                            config.readiness(),
                            &sender,
                            ProviderSourceHealthStatus::Reconnecting,
                            websocket_health_reason(&error),
                            error.to_string(),
                        )
                        .await?;
                    }
                },
                Err(error) => {
                    tracing::warn!(%error, endpoint = config.endpoint(), "provider stream websocket connect/subscribe failed; reconnecting");
                    send_primary_provider_health(
                        &source,
                        config.readiness(),
                        &sender,
                        ProviderSourceHealthStatus::Reconnecting,
                        websocket_health_reason(&error),
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
                    format!("exhausted websocket reconnect attempts after {attempts} failures");
                send_primary_provider_health(
                    &source,
                    config.readiness(),
                    &sender,
                    ProviderSourceHealthStatus::Unhealthy,
                    ProviderSourceHealthReason::ReconnectBudgetExhausted,
                    detail.clone(),
                )
                .await?;
                return Err(WebsocketProtocolError::ReconnectBudgetExhausted { attempts }.into());
            }
            tokio::time::sleep(config.reconnect_delay).await;
        }
    }))
}

/// Spawns one websocket `logsSubscribe` source into a SOF provider-stream queue.
///
/// # Errors
///
/// Returns an error if the initial source connect or subscribe step fails.
pub async fn spawn_websocket_logs_source(
    config: &WebsocketLogsConfig,
    sender: ProviderStreamSender,
) -> Result<JoinHandle<Result<(), WebsocketLogsError>>, WebsocketLogsError> {
    spawn_websocket_logs_source_inner(config, sender, None).await
}

async fn spawn_websocket_logs_source_inner(
    config: &WebsocketLogsConfig,
    sender: ProviderStreamSender,
    reservation: Option<Arc<ProviderSourceReservation>>,
) -> Result<JoinHandle<Result<(), WebsocketLogsError>>, WebsocketLogsError> {
    let config = config.clone();
    let source = ProviderSourceIdentity::generated(
        ProviderSourceId::WebsocketLogs,
        config.source_instance(),
    );
    let initial_health = queue_logs_provider_health(
        &source,
        config.readiness(),
        &sender,
        ProviderSourceHealthStatus::Reconnecting,
        ProviderSourceHealthReason::InitialConnectPending,
        "waiting for first websocket logs session ack".to_owned(),
    )?;
    let first_session = match establish_websocket_logs_session(&config).await {
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
                .map_err(|_error| WebsocketLogsError::QueueClosed)?;
        }
        let mut attempts = 0_u32;
        let mut first_session = Some(first_session);
        loop {
            let mut session_established = false;
            let session = match first_session.take() {
                Some(session) => Ok(session),
                None => establish_websocket_logs_session(&config).await,
            };
            match session {
                Ok(session) => {
                    match run_websocket_logs_connection(
                        &config,
                        &source,
                        &sender,
                        &mut session_established,
                        session,
                    )
                    .await
                    {
                        Ok(()) => {
                            let detail = "websocket logs stream ended unexpectedly".to_owned();
                            tracing::warn!(
                                detail,
                                endpoint = config.endpoint(),
                                "provider stream websocket logs session ended; reconnecting"
                            );
                            send_provider_logs_health(
                                &source,
                                config.readiness(),
                                &sender,
                                ProviderSourceHealthStatus::Reconnecting,
                                ProviderSourceHealthReason::UpstreamStreamClosedUnexpectedly,
                                detail,
                            )
                            .await?;
                        }
                        Err(WebsocketLogsError::QueueClosed) => {
                            return Err(WebsocketLogsError::QueueClosed);
                        }
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                endpoint = config.endpoint(),
                                "provider stream websocket logs session ended; reconnecting"
                            );
                            send_provider_logs_health(
                                &source,
                                config.readiness(),
                                &sender,
                                ProviderSourceHealthStatus::Reconnecting,
                                websocket_logs_health_reason(&error),
                                error.to_string(),
                            )
                            .await?;
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        endpoint = config.endpoint(),
                        "provider stream websocket logs connect/subscribe failed; reconnecting"
                    );
                    send_provider_logs_health(
                        &source,
                        config.readiness(),
                        &sender,
                        ProviderSourceHealthStatus::Reconnecting,
                        websocket_logs_health_reason(&error),
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
                    "exhausted websocket logs reconnect attempts after {attempts} failures"
                );
                send_provider_logs_health(
                    &source,
                    config.readiness(),
                    &sender,
                    ProviderSourceHealthStatus::Unhealthy,
                    ProviderSourceHealthReason::ReconnectBudgetExhausted,
                    detail.clone(),
                )
                .await?;
                return Err(WebsocketProtocolError::ReconnectBudgetExhausted { attempts }.into());
            }
            tokio::time::sleep(config.reconnect_delay).await;
        }
    }))
}

async fn run_websocket_primary_connection(
    config: &WebsocketTransactionConfig,
    source: &ProviderSourceIdentity,
    sender: &ProviderStreamSender,
    last_seen_slot: &mut Option<u64>,
    watermarks: &mut ProviderCommitmentWatermarks,
    session_established: &mut bool,
    stream: WebsocketProviderStream,
) -> Result<(), WebsocketTransactionError> {
    *session_established = false;
    let (mut write, mut read) = stream.split();
    *session_established = true;
    send_primary_provider_health(
        source,
        config.readiness(),
        sender,
        ProviderSourceHealthStatus::Healthy,
        ProviderSourceHealthReason::SubscriptionAckReceived,
        PROVIDER_SUBSCRIPTION_ACKNOWLEDGED.to_owned(),
    )
    .await?;
    if matches!(config.stream, WebsocketPrimaryStream::Transaction)
        && config.replay_on_reconnect
        && last_seen_slot.is_some()
    {
        replay_websocket_gap(config, source, sender, last_seen_slot, watermarks).await?;
    }
    let mut ping = config.ping_interval.map(tokio::time::interval);
    let mut scratch = WebsocketParseScratch::default();
    let mut last_progress = tokio::time::Instant::now();

    loop {
        tokio::select! {
            () = async {
                if let Some(interval) = ping.as_mut() {
                    interval.tick().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                write.send(WsMessage::Ping(Vec::new().into())).await?;
            }
            () = async {
                if let Some(timeout) = config.stall_timeout {
                    let deadline = last_progress.checked_add(timeout).unwrap_or(last_progress);
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                return Err(WebsocketProtocolError::StreamStalled {
                    stream: config.stream_kind(),
                }
                .into());
            }
            maybe_frame = read.next() => {
                let Some(frame) = maybe_frame else {
                    return Ok(());
                };
                let frame = frame?;
                last_progress = tokio::time::Instant::now();
                match frame {
                    WsMessage::Text(text) => {
                        let mut state = WebsocketPrimaryNotificationState {
                            last_seen_slot,
                            watermarks,
                            json_buffers: &mut scratch.json_buffers,
                            tx_bytes: &mut scratch.tx_bytes,
                        };
                        handle_primary_notification(
                            config,
                            source,
                            sender,
                            frame_bytes_mut(&mut scratch.frame_bytes, text.as_str().as_bytes()),
                            &mut state,
                        )
                        .await?;
                    }
                    WsMessage::Binary(bytes) => {
                        let mut state = WebsocketPrimaryNotificationState {
                            last_seen_slot,
                            watermarks,
                            json_buffers: &mut scratch.json_buffers,
                            tx_bytes: &mut scratch.tx_bytes,
                        };
                        handle_primary_notification(
                            config,
                            source,
                            sender,
                            frame_bytes_mut(&mut scratch.frame_bytes, bytes.as_ref()),
                            &mut state,
                        )
                        .await?;
                    }
                    WsMessage::Ping(payload) => {
                        write.send(WsMessage::Pong(payload)).await?;
                    }
                    WsMessage::Pong(_) => {}
                    WsMessage::Close(frame) => {
                        return Err(WebsocketProtocolError::Closed(format!("{frame:?}")).into());
                    }
                    _ => {}
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
) -> Result<(), WebsocketTransactionError> {
    sender
        .send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
            source: source.clone(),
            readiness,
            status,
            reason,
            message,
        }))
        .await
        .map_err(|_error| WebsocketTransactionError::QueueClosed)
}

fn queue_primary_provider_health(
    source: &ProviderSourceIdentity,
    readiness: ProviderSourceReadiness,
    sender: &ProviderStreamSender,
    status: ProviderSourceHealthStatus,
    reason: ProviderSourceHealthReason,
    message: String,
) -> Result<Option<ProviderStreamUpdate>, WebsocketTransactionError> {
    queue_provider_health_update(
        source,
        readiness,
        sender,
        status,
        reason,
        message,
        WebsocketTransactionError::QueueClosed,
    )
}

async fn send_provider_logs_health(
    source: &ProviderSourceIdentity,
    readiness: ProviderSourceReadiness,
    sender: &ProviderStreamSender,
    status: ProviderSourceHealthStatus,
    reason: ProviderSourceHealthReason,
    message: String,
) -> Result<(), WebsocketLogsError> {
    sender
        .send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
            source: source.clone(),
            readiness,
            status,
            reason,
            message,
        }))
        .await
        .map_err(|_error| WebsocketLogsError::QueueClosed)
}

fn queue_logs_provider_health(
    source: &ProviderSourceIdentity,
    readiness: ProviderSourceReadiness,
    sender: &ProviderStreamSender,
    status: ProviderSourceHealthStatus,
    reason: ProviderSourceHealthReason,
    message: String,
) -> Result<Option<ProviderStreamUpdate>, WebsocketLogsError> {
    queue_provider_health_update(
        source,
        readiness,
        sender,
        status,
        reason,
        message,
        WebsocketLogsError::QueueClosed,
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

const fn websocket_health_reason(error: &WebsocketTransactionError) -> ProviderSourceHealthReason {
    match error {
        WebsocketTransactionError::Config(_) => ProviderSourceHealthReason::UpstreamProtocolFailure,
        WebsocketTransactionError::Transport(_) => {
            ProviderSourceHealthReason::UpstreamTransportFailure
        }
        WebsocketTransactionError::Protocol(_) | WebsocketTransactionError::Convert(_) => {
            ProviderSourceHealthReason::UpstreamProtocolFailure
        }
        WebsocketTransactionError::QueueClosed => {
            ProviderSourceHealthReason::UpstreamProtocolFailure
        }
        WebsocketTransactionError::DuplicateSourceIdentity(_) => {
            ProviderSourceHealthReason::UpstreamProtocolFailure
        }
    }
}

const fn websocket_logs_health_reason(error: &WebsocketLogsError) -> ProviderSourceHealthReason {
    match error {
        WebsocketLogsError::Transport(_) => ProviderSourceHealthReason::UpstreamTransportFailure,
        WebsocketLogsError::Protocol(_)
        | WebsocketLogsError::QueueClosed
        | WebsocketLogsError::DuplicateSourceIdentity(_) => {
            ProviderSourceHealthReason::UpstreamProtocolFailure
        }
    }
}

async fn wait_for_subscription_ack<S>(read: &mut S) -> Result<(), WebsocketTransactionError>
where
    S: futures_util::Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
    let ack_timeout = Duration::from_secs(10);
    let mut frame_bytes = Vec::new();
    tokio::time::timeout(ack_timeout, async {
        loop {
            let Some(frame) = read.next().await else {
                return Err(WebsocketTransactionError::Protocol(
                    WebsocketProtocolError::ClosedBeforeSubscriptionAck,
                ));
            };
            let frame = frame?;
            match frame {
                WsMessage::Text(text) => {
                    if handle_subscription_text(frame_bytes_mut(
                        &mut frame_bytes,
                        text.as_str().as_bytes(),
                    ))? {
                        return Ok(());
                    }
                }
                WsMessage::Binary(bytes) => {
                    if handle_subscription_text(frame_bytes_mut(&mut frame_bytes, bytes.as_ref()))?
                    {
                        return Ok(());
                    }
                }
                WsMessage::Ping(_) | WsMessage::Pong(_) => {}
                WsMessage::Close(frame) => {
                    return Err(
                        WebsocketProtocolError::ClosedBeforeSubscriptionAckWithFrame(format!(
                            "{frame:?}"
                        ))
                        .into(),
                    );
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_elapsed| WebsocketProtocolError::SubscriptionAckTimeout)?
}

fn handle_subscription_text(bytes: &mut [u8]) -> Result<bool, WebsocketTransactionError> {
    let value: WebsocketSubscriptionAck = simd_from_slice(bytes)
        .map_err(|error| WebsocketProtocolError::InvalidJson(error.to_string()))?;
    if let Some(error) = value.error {
        return Err(WebsocketProtocolError::SubscriptionError(error.to_string()).into());
    }
    Ok(value.id == Some(1) && value.result.is_some())
}

fn handle_logs_subscription_text(bytes: &mut [u8]) -> Result<bool, WebsocketLogsError> {
    let value: WebsocketSubscriptionAck = simd_from_slice(bytes)
        .map_err(|error| WebsocketProtocolError::InvalidJson(error.to_string()))?;
    if let Some(error) = value.error {
        return Err(WebsocketProtocolError::SubscriptionError(error.to_string()).into());
    }
    Ok(value.id == Some(1) && value.result.is_some())
}

fn parse_transaction_notification(
    bytes: &mut [u8],
    json_buffers: &mut SimdJsonBuffers,
    tx_bytes: &mut Vec<u8>,
    commitment_status: WebsocketTransactionCommitment,
    watermarks: &mut ProviderCommitmentWatermarks,
) -> Result<Option<SerializedTransactionEvent>, WebsocketTransactionError> {
    let value: WebsocketTransactionEnvelopeMessage =
        simd_json::serde::from_slice_with_buffers(bytes, json_buffers)
            .map_err(|error| WebsocketProtocolError::InvalidJson(error.to_string()))?;
    if let Some(error) = value.error {
        return Err(WebsocketProtocolError::ProviderError(error.to_string()).into());
    }
    let Some(notification) = value.params.map(|params| params.result) else {
        return Ok(None);
    };
    if notification.transaction.transaction.1 != "base64" {
        return Err(WebsocketTransactionError::Convert(
            "unsupported websocket transaction encoding",
        ));
    }
    tx_bytes.clear();
    STANDARD
        .decode_vec(notification.transaction.transaction.0.as_bytes(), tx_bytes)
        .map_err(|_error| {
            WebsocketTransactionError::Convert("invalid base64 transaction payload")
        })?;
    let signature = serialized_transaction_first_signature(tx_bytes).or_else(|| {
        notification
            .signature
            .and_then(|signature| Signature::from_str(&signature).ok())
            .map(SignatureBytes::from)
    });
    watermarks
        .observe_transaction_commitment(notification.slot, commitment_status.as_tx_commitment());
    let tx_payload = std::mem::take(tx_bytes).into_boxed_slice();
    Ok(Some(SerializedTransactionEvent {
        slot: notification.slot,
        commitment_status: commitment_status.as_tx_commitment(),
        confirmed_slot: watermarks.confirmed_slot,
        finalized_slot: watermarks.finalized_slot,
        signature,
        provider_source: None,
        bytes: tx_payload,
    }))
}

fn parse_account_notification(
    bytes: &mut [u8],
    json_buffers: &mut SimdJsonBuffers,
    tx_bytes: &mut Vec<u8>,
    config: &WebsocketTransactionConfig,
    watermarks: &mut ProviderCommitmentWatermarks,
) -> Result<Option<AccountUpdateEvent>, WebsocketTransactionError> {
    let value: WebsocketAccountEnvelopeMessage =
        simd_json::serde::from_slice_with_buffers(bytes, json_buffers)
            .map_err(|error| WebsocketProtocolError::InvalidJson(error.to_string()))?;
    if let Some(error) = value.error {
        return Err(WebsocketProtocolError::ProviderError(error.to_string()).into());
    }
    let Some(notification) = value.params.map(|params| params.result) else {
        return Ok(None);
    };
    let (pubkey, account) = match (config.stream, notification.value) {
        (
            WebsocketPrimaryStream::Account(account_pubkey),
            WebsocketAccountNotificationValue::Account(account),
        ) => (account_pubkey, account),
        (
            WebsocketPrimaryStream::Program(_),
            WebsocketAccountNotificationValue::Program(account),
        ) => (parse_pubkey(&account.pubkey)?, account.account),
        (WebsocketPrimaryStream::Transaction, _) => {
            return Err(WebsocketTransactionError::Convert(
                "unexpected account payload for transaction stream",
            ));
        }
        _ => {
            return Err(WebsocketTransactionError::Convert(
                "unexpected websocket account/program payload shape",
            ));
        }
    };
    decode_account_update_event(
        notification.context.slot,
        pubkey,
        &account,
        match config.stream {
            WebsocketPrimaryStream::Program(program_id) => Some(pubkey_bytes(program_id)),
            WebsocketPrimaryStream::Account(filter_pubkey) => Some(pubkey_bytes(filter_pubkey)),
            WebsocketPrimaryStream::Transaction => None,
        },
        config.commitment,
        watermarks,
        tx_bytes,
    )
    .map(Some)
}

fn decode_account_update_event(
    slot: u64,
    pubkey: Pubkey,
    account: &WebsocketUiAccount,
    matched_filter: Option<sof_types::PubkeyBytes>,
    commitment: WebsocketTransactionCommitment,
    watermarks: &mut ProviderCommitmentWatermarks,
    tx_bytes: &mut Vec<u8>,
) -> Result<AccountUpdateEvent, WebsocketTransactionError> {
    let owner = parse_pubkey(&account.owner)?;
    tx_bytes.clear();
    if account.data.1 != "base64" {
        return Err(WebsocketTransactionError::Convert(
            "unsupported websocket account encoding",
        ));
    }
    STANDARD
        .decode_vec(account.data.0.as_bytes(), tx_bytes)
        .map_err(|_error| WebsocketTransactionError::Convert("invalid base64 account payload"))?;
    observe_non_transaction_commitment(watermarks, slot, commitment.as_tx_commitment());
    let data = std::mem::take(tx_bytes).into_boxed_slice();
    Ok(AccountUpdateEvent {
        slot,
        commitment_status: commitment.as_tx_commitment(),
        confirmed_slot: watermarks.confirmed_slot,
        finalized_slot: watermarks.finalized_slot,
        pubkey: pubkey_bytes(pubkey),
        owner: pubkey_bytes(owner),
        lamports: account.lamports,
        executable: account.executable,
        rent_epoch: account.rent_epoch,
        data: Arc::from(data),
        write_version: None,
        txn_signature: None,
        is_startup: false,
        matched_filter,
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

fn serialized_transaction_first_signature(payload: &[u8]) -> Option<SignatureBytes> {
    let mut offset = 0_usize;
    let signature_count = decode_short_u16_len(payload, &mut offset)?;
    if signature_count == 0 {
        return None;
    }
    let end = offset.checked_add(64)?;
    let bytes: [u8; 64] = payload.get(offset..end)?.try_into().ok()?;
    Some(SignatureBytes::from(bytes))
}

fn decode_short_u16_len(payload: &[u8], offset: &mut usize) -> Option<usize> {
    let mut value = 0_usize;
    let mut shift = 0_u32;
    for byte_index in 0..3 {
        let byte = usize::from(*payload.get(*offset)?);
        *offset = (*offset).saturating_add(1);
        value |= (byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift = shift.saturating_add(7);
        if byte_index == 2 {
            return None;
        }
    }
    None
}

fn parse_pubkey(input: &str) -> Result<Pubkey, WebsocketTransactionError> {
    Pubkey::from_str(input)
        .map_err(|_error| WebsocketTransactionError::Convert("invalid websocket pubkey"))
}

struct WebsocketPrimaryNotificationState<'state> {
    last_seen_slot: &'state mut Option<u64>,
    watermarks: &'state mut ProviderCommitmentWatermarks,
    json_buffers: &'state mut SimdJsonBuffers,
    tx_bytes: &'state mut Vec<u8>,
}

async fn handle_primary_notification(
    config: &WebsocketTransactionConfig,
    source: &ProviderSourceIdentity,
    sender: &ProviderStreamSender,
    bytes: &mut [u8],
    state: &mut WebsocketPrimaryNotificationState<'_>,
) -> Result<(), WebsocketTransactionError> {
    match config.stream {
        WebsocketPrimaryStream::Transaction => {
            if let Some(update) = parse_transaction_notification(
                bytes,
                state.json_buffers,
                state.tx_bytes,
                config.commitment,
                state.watermarks,
            )? {
                *state.last_seen_slot = Some(
                    (*state.last_seen_slot)
                        .unwrap_or(update.slot)
                        .max(update.slot),
                );
                sender
                    .send(
                        ProviderStreamUpdate::SerializedTransaction(update)
                            .with_provider_source(source.clone()),
                    )
                    .await
                    .map_err(|_error| WebsocketTransactionError::QueueClosed)?;
            }
        }
        WebsocketPrimaryStream::Account(_) | WebsocketPrimaryStream::Program(_) => {
            if let Some(update) = parse_account_notification(
                bytes,
                state.json_buffers,
                state.tx_bytes,
                config,
                state.watermarks,
            )? {
                *state.last_seen_slot = Some(
                    (*state.last_seen_slot)
                        .unwrap_or(update.slot)
                        .max(update.slot),
                );
                sender
                    .send(
                        ProviderStreamUpdate::AccountUpdate(update)
                            .with_provider_source(source.clone()),
                    )
                    .await
                    .map_err(|_error| WebsocketTransactionError::QueueClosed)?;
            }
        }
    }
    Ok(())
}

async fn run_websocket_logs_connection(
    config: &WebsocketLogsConfig,
    source: &ProviderSourceIdentity,
    sender: &ProviderStreamSender,
    session_established: &mut bool,
    stream: WebsocketProviderStream,
) -> Result<(), WebsocketLogsError> {
    *session_established = false;
    let (mut write, mut read) = stream.split();
    *session_established = true;
    send_provider_logs_health(
        source,
        config.readiness(),
        sender,
        ProviderSourceHealthStatus::Healthy,
        ProviderSourceHealthReason::SubscriptionAckReceived,
        PROVIDER_SUBSCRIPTION_ACKNOWLEDGED.to_owned(),
    )
    .await?;
    let mut ping = config.ping_interval.map(tokio::time::interval);
    let mut frame_bytes = Vec::new();
    let mut last_progress = tokio::time::Instant::now();

    loop {
        tokio::select! {
            () = async {
                if let Some(interval) = ping.as_mut() {
                    interval.tick().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                write.send(WsMessage::Ping(Vec::new().into())).await?;
            }
            () = async {
                if let Some(timeout) = config.stall_timeout {
                    let deadline = last_progress.checked_add(timeout).unwrap_or(last_progress);
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                return Err(WebsocketProtocolError::StreamStalled {
                    stream: WebsocketStreamKind::Logs,
                }
                .into());
            }
            maybe_frame = read.next() => {
                let Some(frame) = maybe_frame else {
                    return Ok(());
                };
                let frame = frame?;
                last_progress = tokio::time::Instant::now();
                match frame {
                    WsMessage::Text(text) => {
                        if let Some(update) = parse_logs_notification(
                            frame_bytes_mut(&mut frame_bytes, text.as_str().as_bytes()),
                            config,
                        )? {
                            sender
                                .send(
                                    ProviderStreamUpdate::TransactionLog(update)
                                        .with_provider_source(source.clone()),
                                )
                                .await
                                .map_err(|_error| WebsocketLogsError::QueueClosed)?;
                        }
                    }
                    WsMessage::Binary(bytes) => {
                        if let Some(update) = parse_logs_notification(
                            frame_bytes_mut(&mut frame_bytes, bytes.as_ref()),
                            config,
                        )? {
                            sender
                                .send(
                                    ProviderStreamUpdate::TransactionLog(update)
                                        .with_provider_source(source.clone()),
                                )
                                .await
                                .map_err(|_error| WebsocketLogsError::QueueClosed)?;
                        }
                    }
                    WsMessage::Ping(payload) => {
                        write.send(WsMessage::Pong(payload)).await?;
                    }
                    WsMessage::Pong(_) => {}
                    WsMessage::Close(frame) => {
                        return Err(WebsocketProtocolError::Closed(format!("{frame:?}")).into());
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn establish_websocket_logs_session(
    config: &WebsocketLogsConfig,
) -> Result<WebsocketProviderStream, WebsocketLogsError> {
    let (mut stream, _response) = connect_async(config.endpoint()).await?;
    stream
        .send(WsMessage::Text(
            config.subscribe_request().to_string().into(),
        ))
        .await?;
    wait_for_logs_subscription_ack(&mut stream).await?;
    Ok(stream)
}

async fn wait_for_logs_subscription_ack<S>(read: &mut S) -> Result<(), WebsocketLogsError>
where
    S: futures_util::Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
    let ack_timeout = Duration::from_secs(10);
    let mut frame_bytes = Vec::new();
    tokio::time::timeout(ack_timeout, async {
        loop {
            let Some(frame) = read.next().await else {
                return Err(WebsocketLogsError::Protocol(
                    WebsocketProtocolError::ClosedBeforeSubscriptionAck,
                ));
            };
            let frame = frame?;
            match frame {
                WsMessage::Text(text) => {
                    if handle_logs_subscription_text(frame_bytes_mut(
                        &mut frame_bytes,
                        text.as_str().as_bytes(),
                    ))? {
                        return Ok(());
                    }
                }
                WsMessage::Binary(bytes) => {
                    if handle_logs_subscription_text(frame_bytes_mut(
                        &mut frame_bytes,
                        bytes.as_ref(),
                    ))? {
                        return Ok(());
                    }
                }
                WsMessage::Ping(_) | WsMessage::Pong(_) => {}
                WsMessage::Close(frame) => {
                    return Err(
                        WebsocketProtocolError::ClosedBeforeSubscriptionAckWithFrame(format!(
                            "{frame:?}"
                        ))
                        .into(),
                    );
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_elapsed| WebsocketProtocolError::SubscriptionAckTimeout)?
}

fn parse_logs_notification(
    bytes: &mut [u8],
    config: &WebsocketLogsConfig,
) -> Result<Option<crate::framework::TransactionLogEvent>, WebsocketLogsError> {
    let value: WebsocketLogsEnvelopeMessage = simd_from_slice(bytes)
        .map_err(|error| WebsocketProtocolError::InvalidJson(error.to_string()))?;
    if let Some(error) = value.error {
        return Err(WebsocketProtocolError::ProviderError(error.to_string()).into());
    }
    let Some(notification) = value.params.map(|params| params.result) else {
        return Ok(None);
    };
    let signature = Signature::from_str(notification.value.signature.as_ref())
        .map_err(|_error| WebsocketProtocolError::InvalidLogSignature)?;
    let matched_filter = match config.filter {
        WebsocketLogsFilter::Mentions(pubkey) => Some(PubkeyBytes::from(pubkey)),
        WebsocketLogsFilter::All | WebsocketLogsFilter::AllWithVotes => None,
    };
    Ok(Some(crate::framework::TransactionLogEvent {
        slot: notification.context.slot,
        commitment_status: config.commitment.as_tx_commitment(),
        signature: signature.into(),
        err: notification.value.err,
        logs: Arc::from(
            notification
                .value
                .logs
                .into_iter()
                .map(Cow::into_owned)
                .collect::<Vec<_>>(),
        ),
        matched_filter,
        provider_source: None,
    }))
}

#[cfg(test)]
fn materialize_transaction_baseline(
    bytes: &mut [u8],
    commitment_status: WebsocketTransactionCommitment,
) -> Result<Option<TransactionEvent>, WebsocketTransactionError> {
    let value: WebsocketTransactionEnvelopeMessage = simd_from_slice(bytes)
        .map_err(|error| WebsocketProtocolError::InvalidJson(error.to_string()))?;
    if let Some(error) = value.error {
        return Err(WebsocketProtocolError::ProviderError(error.to_string()).into());
    }
    let Some(notification) = value.params.map(|params| params.result) else {
        return Ok(None);
    };
    if notification.transaction.transaction.1 != "base64" {
        return Err(WebsocketTransactionError::Convert(
            "unsupported websocket transaction encoding",
        ));
    }
    let tx_bytes = STANDARD
        .decode(notification.transaction.transaction.0.as_bytes())
        .map_err(|_error| {
            WebsocketTransactionError::Convert("invalid base64 transaction payload")
        })?;
    let tx = bincode::deserialize::<VersionedTransaction>(&tx_bytes).map_err(|_error| {
        WebsocketTransactionError::Convert("failed to deserialize transaction")
    })?;
    let signature = tx.signatures.first().copied().or_else(|| {
        notification
            .signature
            .and_then(|signature| Signature::from_str(&signature).ok())
    });
    Ok(Some(TransactionEvent {
        slot: notification.slot,
        commitment_status: commitment_status.as_tx_commitment(),
        confirmed_slot: None,
        finalized_slot: None,
        signature: signature_bytes_opt(signature),
        provider_source: None,
        kind: classify_provider_transaction_kind(&tx),
        tx: Arc::new(tx),
    }))
}

#[derive(Default)]
struct WebsocketParseScratch {
    frame_bytes: Vec<u8>,
    json_buffers: SimdJsonBuffers,
    tx_bytes: Vec<u8>,
}

fn frame_bytes_mut<'buffer>(buffer: &'buffer mut Vec<u8>, bytes: &[u8]) -> &'buffer mut [u8] {
    buffer.clear();
    buffer.extend_from_slice(bytes);
    buffer.as_mut_slice()
}

async fn replay_websocket_gap(
    config: &WebsocketTransactionConfig,
    source: &ProviderSourceIdentity,
    sender: &ProviderStreamSender,
    last_seen_slot: &mut Option<u64>,
    watermarks: &mut ProviderCommitmentWatermarks,
) -> Result<(), WebsocketTransactionError> {
    let Some(previous_slot) = *last_seen_slot else {
        return Ok(());
    };
    let Some(http_endpoint) = websocket_http_endpoint(config) else {
        return Err(WebsocketProtocolError::MissingReplayHttpEndpoint.into());
    };

    let client = reqwest::Client::new();
    let head = rpc_get_slot(&client, &http_endpoint, config.commitment).await?;
    if head < previous_slot {
        return Ok(());
    }

    let start_slot = websocket_replay_start_slot(previous_slot, head, config.replay_max_slots);
    for slot in start_slot..=head {
        let Some(block) = rpc_get_block(&client, &http_endpoint, slot, config.commitment).await?
        else {
            continue;
        };
        for transaction in block.transactions {
            let tx_bytes = STANDARD
                .decode(transaction.transaction.0.as_bytes())
                .map_err(|_error| {
                    WebsocketTransactionError::Convert("invalid base64 transaction payload")
                })?;
            let tx = bincode::deserialize::<VersionedTransaction>(&tx_bytes).map_err(|_error| {
                WebsocketTransactionError::Convert("failed to deserialize transaction")
            })?;
            let kind = classify_provider_transaction_kind(&tx);
            let failed = transaction
                .meta
                .as_ref()
                .and_then(|meta| meta.err.as_ref())
                .is_some();
            if !websocket_transaction_matches_filter(
                config,
                &tx,
                transaction
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.loaded_addresses.as_ref()),
                kind,
                failed,
            ) {
                continue;
            }
            watermarks.observe_transaction_commitment(slot, config.commitment.as_tx_commitment());
            sender
                .send(
                    ProviderStreamUpdate::Transaction(TransactionEvent {
                        slot,
                        commitment_status: config.commitment.as_tx_commitment(),
                        confirmed_slot: watermarks.confirmed_slot,
                        finalized_slot: watermarks.finalized_slot,
                        signature: signature_bytes_opt(tx.signatures.first().copied()),
                        provider_source: None,
                        kind,
                        tx: Arc::new(tx),
                    })
                    .with_provider_source(source.clone()),
                )
                .await
                .map_err(|_error| WebsocketTransactionError::QueueClosed)?;
            *last_seen_slot = Some((*last_seen_slot).unwrap_or(slot).max(slot));
        }
    }
    Ok(())
}

async fn establish_websocket_primary_session(
    config: &WebsocketTransactionConfig,
) -> Result<WebsocketProviderStream, WebsocketTransactionError> {
    if matches!(config.stream, WebsocketPrimaryStream::Transaction)
        && config.replay_on_reconnect
        && websocket_http_endpoint(config).is_none()
    {
        return Err(WebsocketProtocolError::MissingReplayHttpEndpoint.into());
    }
    let (mut stream, _response) = connect_async(config.endpoint()).await?;
    stream
        .send(WsMessage::Text(
            config.subscribe_request().to_string().into(),
        ))
        .await?;
    wait_for_subscription_ack(&mut stream).await?;
    Ok(stream)
}

fn websocket_replay_start_slot(previous_slot: u64, head: u64, replay_max_slots: u64) -> u64 {
    previous_slot.max(
        head.saturating_add(1)
            .saturating_sub(replay_max_slots.max(1)),
    )
}

fn websocket_http_endpoint(config: &WebsocketTransactionConfig) -> Option<String> {
    if let Some(endpoint) = config.http_endpoint() {
        return Some(endpoint.to_owned());
    }
    if let Some(rest) = config.endpoint().strip_prefix("wss://") {
        return Some(format!("https://{rest}"));
    }
    if let Some(rest) = config.endpoint().strip_prefix("ws://") {
        return Some(format!("http://{rest}"));
    }
    None
}

async fn rpc_get_slot(
    client: &reqwest::Client,
    endpoint: &str,
    commitment: WebsocketTransactionCommitment,
) -> Result<u64, WebsocketTransactionError> {
    let response: RpcJsonResponse<u64> = client
        .post(endpoint)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getSlot",
            "params": [{ "commitment": commitment.as_str() }],
        }))
        .send()
        .await
        .map_err(|error| WebsocketProtocolError::HttpRpcFailed {
            method: "getSlot",
            detail: error.to_string(),
        })?
        .json()
        .await
        .map_err(|error| WebsocketProtocolError::HttpRpcDecodeFailed {
            method: "getSlot",
            detail: error.to_string(),
        })?;
    if let Some(error) = response.error {
        return Err(WebsocketProtocolError::HttpRpcFailed {
            method: "getSlot",
            detail: format!("returned error: {error}"),
        }
        .into());
    }
    response
        .result
        .ok_or_else(|| WebsocketProtocolError::HttpRpcMissingResult { method: "getSlot" }.into())
}

async fn rpc_get_block(
    client: &reqwest::Client,
    endpoint: &str,
    slot: u64,
    commitment: WebsocketTransactionCommitment,
) -> Result<Option<RpcBlockResponse>, WebsocketTransactionError> {
    let response: RpcJsonResponse<RpcBlockResponse> = client
        .post(endpoint)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getBlock",
            "params": [
                slot,
                {
                    "commitment": commitment.as_str(),
                    "encoding": "base64",
                    "transactionDetails": "full",
                    "maxSupportedTransactionVersion": 0,
                    "rewards": false
                }
            ],
        }))
        .send()
        .await
        .map_err(|error| WebsocketProtocolError::HttpRpcFailed {
            method: "getBlock",
            detail: error.to_string(),
        })?
        .json()
        .await
        .map_err(|error| WebsocketProtocolError::HttpRpcDecodeFailed {
            method: "getBlock",
            detail: error.to_string(),
        })?;
    if let Some(error) = response.error {
        return Err(WebsocketProtocolError::HttpRpcFailed {
            method: "getBlock",
            detail: format!("returned error: {error}"),
        }
        .into());
    }
    Ok(response.result)
}

fn websocket_transaction_matches_filter(
    config: &WebsocketTransactionConfig,
    tx: &VersionedTransaction,
    loaded_addresses: Option<&RpcLoadedAddresses>,
    kind: crate::event::TxKind,
    failed: bool,
) -> bool {
    if let Some(signature) = config.signature
        && tx.signatures.first().copied() != Some(signature)
    {
        return false;
    }
    if let Some(expect_vote) = config.vote {
        let is_vote = kind == crate::event::TxKind::VoteOnly;
        if is_vote != expect_vote {
            return false;
        }
    }
    if let Some(expect_failed) = config.failed
        && failed != expect_failed
    {
        return false;
    }
    let key_present = |key: &Pubkey| {
        tx.message.static_account_keys().contains(key)
            || loaded_addresses.is_some_and(|loaded| loaded.contains(key))
    };
    if !config.account_include.is_empty() && !config.account_include.iter().any(key_present) {
        return false;
    }
    if !config.account_exclude.is_empty() && config.account_exclude.iter().any(key_present) {
        return false;
    }
    if !config.account_required.is_empty() && !config.account_required.iter().all(key_present) {
        return false;
    }
    true
}

#[derive(Debug, Deserialize)]
struct WebsocketSubscriptionAck {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct WebsocketTransactionEnvelopeMessage<'input> {
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    #[serde(borrow)]
    params: Option<WebsocketTransactionParams<'input>>,
}

#[derive(Debug, Deserialize)]
struct WebsocketLogsEnvelopeMessage<'input> {
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    #[serde(borrow)]
    params: Option<WebsocketLogsParams<'input>>,
}

#[derive(Debug, Deserialize)]
struct WebsocketLogsParams<'input> {
    #[serde(borrow)]
    result: WebsocketLogsNotification<'input>,
}

#[derive(Debug, Deserialize)]
struct WebsocketLogsNotification<'input> {
    context: WebsocketLogsContext,
    #[serde(borrow)]
    value: WebsocketLogsValue<'input>,
}

#[derive(Debug, Deserialize)]
struct WebsocketLogsContext {
    slot: u64,
}

#[derive(Debug, Deserialize)]
struct WebsocketLogsValue<'input> {
    #[serde(borrow)]
    signature: Cow<'input, str>,
    #[serde(default)]
    #[serde(rename = "err")]
    err: Option<Value>,
    #[serde(default)]
    #[serde(borrow)]
    logs: Vec<Cow<'input, str>>,
}

#[derive(Debug, Deserialize)]
struct WebsocketAccountEnvelopeMessage {
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    params: Option<WebsocketAccountParams>,
}

#[derive(Debug, Deserialize)]
struct WebsocketAccountParams {
    result: WebsocketAccountNotification,
}

#[derive(Debug, Deserialize)]
struct WebsocketAccountNotification {
    context: WebsocketLogsContext,
    value: WebsocketAccountNotificationValue,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WebsocketAccountNotificationValue {
    Account(WebsocketUiAccount),
    Program(WebsocketProgramAccount),
}

#[derive(Debug, Deserialize)]
struct WebsocketProgramAccount {
    pubkey: String,
    account: WebsocketUiAccount,
}

#[derive(Debug, Deserialize)]
struct WebsocketUiAccount {
    lamports: u64,
    owner: String,
    executable: bool,
    #[serde(rename = "rentEpoch")]
    rent_epoch: u64,
    data: (String, String),
}

impl ProviderStreamFanIn {
    /// Spawns one websocket primary source into this fan-in.
    ///
    /// # Errors
    ///
    /// Returns an error if the selected config is invalid or the source cannot
    /// be started.
    pub async fn spawn_websocket_source(
        &self,
        config: &WebsocketTransactionConfig,
    ) -> Result<JoinHandle<Result<(), WebsocketTransactionError>>, WebsocketTransactionError> {
        let reservation = match config.source_identity() {
            Some(source) => Some(Arc::new(self.reserve_source_identity(source)?)),
            None => None,
        };
        spawn_websocket_source_inner(config, self.sender(), reservation).await
    }

    #[deprecated(note = "use spawn_websocket_source")]
    /// Deprecated compatibility shim for [`ProviderStreamFanIn::spawn_websocket_source`].
    ///
    /// # Errors
    ///
    /// Returns the same source-registration or startup error as
    /// [`ProviderStreamFanIn::spawn_websocket_source`].
    pub async fn spawn_websocket_transaction_source(
        &self,
        config: &WebsocketTransactionConfig,
    ) -> Result<JoinHandle<Result<(), WebsocketTransactionError>>, WebsocketTransactionError> {
        self.spawn_websocket_source(config).await
    }

    /// Spawns one websocket `logsSubscribe` source into this fan-in.
    ///
    /// # Errors
    ///
    /// Returns an error if the source cannot be started.
    pub async fn spawn_websocket_logs_source(
        &self,
        config: &WebsocketLogsConfig,
    ) -> Result<JoinHandle<Result<(), WebsocketLogsError>>, WebsocketLogsError> {
        let reservation = match config.source_identity() {
            Some(source) => Some(Arc::new(self.reserve_source_identity(source)?)),
            None => None,
        };
        spawn_websocket_logs_source_inner(config, self.sender(), reservation).await
    }
}

#[derive(Debug, Deserialize)]
struct WebsocketTransactionParams<'input> {
    #[serde(borrow)]
    result: WebsocketTransactionNotification<'input>,
}

#[derive(Debug, Deserialize)]
struct WebsocketTransactionNotification<'input> {
    slot: u64,
    #[serde(default)]
    #[serde(borrow)]
    signature: Option<Cow<'input, str>>,
    #[serde(borrow)]
    transaction: WebsocketTransactionEnvelope<'input>,
}

#[derive(Debug, Deserialize)]
struct WebsocketTransactionEnvelope<'input> {
    #[serde(borrow)]
    transaction: WebsocketEncodedTransaction<'input>,
}

#[derive(Debug, Deserialize)]
struct WebsocketEncodedTransaction<'input>(
    #[serde(borrow)] Cow<'input, str>,
    #[serde(borrow)] Cow<'input, str>,
);

#[derive(Debug, Deserialize)]
struct RpcJsonResponse<T> {
    result: Option<T>,
    error: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct RpcBlockResponse {
    #[serde(default)]
    transactions: Vec<RpcBlockTransaction>,
}

#[derive(Debug, Deserialize)]
struct RpcBlockTransaction {
    transaction: (String, String),
    #[serde(default)]
    meta: Option<RpcTransactionMeta>,
}

#[derive(Debug, Deserialize)]
struct RpcTransactionMeta {
    #[serde(default)]
    err: Option<Value>,
    #[serde(default, rename = "loadedAddresses")]
    loaded_addresses: Option<RpcLoadedAddresses>,
}

#[derive(Debug, Deserialize)]
struct RpcLoadedAddresses {
    #[serde(default)]
    writable: Vec<String>,
    #[serde(default)]
    readonly: Vec<String>,
}

impl RpcLoadedAddresses {
    fn contains(&self, key: &Pubkey) -> bool {
        let target = key.to_string();
        self.writable.iter().any(|candidate| candidate == &target)
            || self.readonly.iter().any(|candidate| candidate == &target)
    }
}

#[cfg(test)]
#[allow(
    clippy::let_underscore_must_use,
    clippy::shadow_unrelated,
    clippy::wildcard_enum_match_arm
)]
mod tests {
    use super::*;
    use crate::event::TxKind;
    use crate::provider_stream::{create_provider_stream_fan_in, create_provider_stream_queue};
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use serde_json::json;
    use solana_keypair::Keypair;
    use solana_message::{Message, VersionedMessage};
    use solana_signer::Signer;
    use std::time::Instant;
    use tokio::net::TcpListener;
    use tokio::time::{Duration, timeout};
    use tokio_tungstenite::{accept_async, tungstenite::protocol::Message as WsMessage};

    #[cfg(feature = "provider-grpc")]
    use crate::provider_stream::yellowstone::{YellowstoneGrpcCommitment, YellowstoneGrpcConfig};

    fn profile_iterations(default: usize) -> usize {
        std::env::var("SOF_PROFILE_ITERATIONS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(default)
    }

    fn sample_notification_payload() -> Vec<u8> {
        let signer = Keypair::new();
        let message = Message::new(&[], Some(&signer.pubkey()));
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[&signer])
            .expect("tx");
        let signature = tx.signatures[0];
        let tx_bytes = bincode::serialize(&tx).expect("serialize tx");
        json!({
            "jsonrpc":"2.0",
            "method":"transactionNotification",
            "params":{
                "result":{
                    "slot":55,
                    "signature":signature.to_string(),
                    "transaction":{
                        "transaction":[BASE64_STANDARD.encode(tx_bytes),"base64"]
                    }
                }
            }
        })
        .to_string()
        .into_bytes()
    }

    #[cfg(feature = "provider-grpc")]
    #[test]
    fn websocket_filter_shape_matches_yellowstone_config() {
        let signature = Signature::from([7_u8; 64]);
        let include = [Pubkey::new_unique(), Pubkey::new_unique()];
        let exclude = [Pubkey::new_unique()];
        let required = [Pubkey::new_unique()];

        let websocket = WebsocketTransactionConfig::new("wss://example.invalid")
            .with_commitment(WebsocketTransactionCommitment::Confirmed)
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

        let websocket_filter = websocket["params"][0]
            .as_object()
            .expect("websocket filter");
        let yellowstone_filter = yellowstone.transactions.get("sof").expect("ys filter");

        assert_eq!(
            websocket["params"][1]["commitment"].as_str(),
            Some("confirmed")
        );
        assert_eq!(
            websocket_filter.get("vote").and_then(Value::as_bool),
            yellowstone_filter.vote
        );
        assert_eq!(
            websocket_filter.get("failed").and_then(Value::as_bool),
            yellowstone_filter.failed
        );
        assert_eq!(
            websocket_filter.get("signature").and_then(Value::as_str),
            yellowstone_filter.signature.as_deref()
        );
        assert_eq!(
            websocket_filter.get("accountInclude"),
            Some(
                &serde_json::to_value(yellowstone_filter.account_include.clone())
                    .expect("include json")
            )
        );
        assert_eq!(
            websocket_filter.get("accountExclude"),
            Some(
                &serde_json::to_value(yellowstone_filter.account_exclude.clone())
                    .expect("exclude json")
            )
        );
        assert_eq!(
            websocket_filter.get("accountRequired"),
            Some(
                &serde_json::to_value(yellowstone_filter.account_required.clone())
                    .expect("required json")
            )
        );
    }

    #[test]
    fn websocket_subscription_ack_accepts_successful_response() {
        let mut ack = br#"{"jsonrpc":"2.0","id":1,"result":42}"#.to_vec();
        assert!(handle_subscription_text(&mut ack).expect("ack should parse"));

        let mut ping = br#"{"jsonrpc":"2.0","method":"ping"}"#.to_vec();
        assert!(!handle_subscription_text(&mut ping).expect("non-ack payload"));
    }

    #[test]
    fn websocket_subscription_ack_rejects_provider_error() {
        let mut error =
            br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"boom"}}"#.to_vec();
        let error = handle_subscription_text(&mut error).expect_err("provider error should fail");
        assert!(error.to_string().contains("subscription error"));
    }

    #[test]
    fn websocket_config_defaults_do_not_filter_vote_or_failed() {
        let request = WebsocketTransactionConfig::new("wss://example.invalid").subscribe_request();
        let filter = request["params"][0].as_object().expect("filter object");
        assert!(!filter.contains_key("vote"));
        assert!(!filter.contains_key("failed"));
    }

    #[test]
    fn websocket_logs_subscribe_request_uses_configured_filter_and_commitment() {
        let pubkey = Pubkey::new_unique();
        let request = WebsocketLogsConfig::new("wss://example.invalid")
            .with_commitment(WebsocketTransactionCommitment::Confirmed)
            .with_filter(WebsocketLogsFilter::Mentions(pubkey))
            .subscribe_request();

        assert_eq!(request["method"].as_str(), Some("logsSubscribe"));
        assert_eq!(
            request["params"][1]["commitment"].as_str(),
            Some("confirmed")
        );
        assert_eq!(
            request["params"][0]["mentions"][0].as_str(),
            Some(pubkey.to_string().as_str())
        );
    }

    #[test]
    fn websocket_account_subscribe_request_uses_configured_pubkey() {
        let pubkey = Pubkey::new_unique();
        let request = WebsocketTransactionConfig::new("wss://example.invalid")
            .with_stream(WebsocketPrimaryStream::Account(pubkey))
            .with_commitment(WebsocketTransactionCommitment::Finalized)
            .subscribe_request();

        assert_eq!(request["method"].as_str(), Some("accountSubscribe"));
        assert_eq!(
            request["params"][0].as_str(),
            Some(pubkey.to_string().as_str())
        );
        assert_eq!(
            request["params"][1]["commitment"].as_str(),
            Some("finalized")
        );
        assert_eq!(request["params"][1]["encoding"].as_str(), Some("base64"));
    }

    #[test]
    fn websocket_program_subscribe_request_uses_filters() {
        let pubkey = Pubkey::new_unique();
        let request = WebsocketTransactionConfig::new("wss://example.invalid")
            .with_stream(WebsocketPrimaryStream::Program(pubkey))
            .with_commitment(WebsocketTransactionCommitment::Confirmed)
            .with_program_filters([
                WebsocketProgramFilter::DataSize(165),
                WebsocketProgramFilter::MemcmpBase58 {
                    offset: 32,
                    bytes: "abc123".to_owned(),
                },
            ])
            .subscribe_request();

        assert_eq!(request["method"].as_str(), Some("programSubscribe"));
        assert_eq!(
            request["params"][0].as_str(),
            Some(pubkey.to_string().as_str())
        );
        assert_eq!(
            request["params"][1]["commitment"].as_str(),
            Some("confirmed")
        );
        assert_eq!(
            request["params"][1]["filters"][0]["dataSize"].as_u64(),
            Some(165)
        );
        assert_eq!(
            request["params"][1]["filters"][1]["memcmp"]["offset"].as_u64(),
            Some(32)
        );
        assert_eq!(
            request["params"][1]["filters"][1]["memcmp"]["bytes"].as_str(),
            Some("abc123")
        );
    }

    #[test]
    fn websocket_transaction_notification_decodes_full_transaction() {
        let payload = sample_notification_payload();
        let mut payload = payload;
        let event = materialize_transaction_baseline(
            &mut payload,
            WebsocketTransactionCommitment::Confirmed,
        )
        .expect("notification should parse")
        .expect("transaction event");

        assert_eq!(event.slot, 55);
        assert!(event.signature.is_some());
        assert_eq!(event.commitment_status, TxCommitmentStatus::Confirmed);
        assert_eq!(event.kind, TxKind::NonVote);
    }

    #[test]
    fn websocket_transaction_notification_tracks_commitment_watermarks() {
        let payload = sample_notification_payload();
        let mut frame_bytes = payload;
        let mut json_buffers = SimdJsonBuffers::default();
        let mut tx_bytes = Vec::new();
        let mut watermarks = ProviderCommitmentWatermarks::default();
        let event = parse_transaction_notification(
            &mut frame_bytes,
            &mut json_buffers,
            &mut tx_bytes,
            WebsocketTransactionCommitment::Confirmed,
            &mut watermarks,
        )
        .expect("notification should parse")
        .expect("serialized event");
        assert_eq!(event.confirmed_slot, Some(55));
        assert_eq!(event.finalized_slot, None);
    }

    #[test]
    fn websocket_http_endpoint_derives_from_websocket_scheme() {
        let config = WebsocketTransactionConfig::new("wss://example.invalid/?api-key=1");
        assert_eq!(
            websocket_http_endpoint(&config).as_deref(),
            Some("https://example.invalid/?api-key=1")
        );
    }

    #[test]
    fn websocket_replay_start_slot_includes_last_seen_slot() {
        assert_eq!(websocket_replay_start_slot(55, 55, 128), 55);
        assert_eq!(websocket_replay_start_slot(55, 60, 128), 55);
    }

    #[test]
    fn websocket_backfill_filter_uses_loaded_addresses() {
        let signer = Keypair::new();
        let message = Message::new(&[], Some(&signer.pubkey()));
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[&signer])
            .expect("tx");
        let loaded_key = Pubkey::new_unique();
        let config = WebsocketTransactionConfig::new("wss://example.invalid")
            .with_account_include([loaded_key]);
        let loaded_addresses = RpcLoadedAddresses {
            writable: vec![loaded_key.to_string()],
            readonly: Vec::new(),
        };

        assert!(websocket_transaction_matches_filter(
            &config,
            &tx,
            Some(&loaded_addresses),
            TxKind::NonVote,
            false,
        ));
    }

    #[test]
    fn websocket_logs_notification_decodes_transaction_log_event() {
        let signature = Signature::from([9_u8; 64]);
        let payload = json!({
            "jsonrpc":"2.0",
            "method":"logsNotification",
            "params":{
                "result":{
                    "context":{"slot":88},
                    "value":{
                        "signature":signature.to_string(),
                        "err":null,
                        "logs":["Program log: test"]
                    }
                }
            }
        })
        .to_string()
        .into_bytes();
        let mut payload = payload;
        let config = WebsocketLogsConfig::new("wss://example.invalid")
            .with_commitment(WebsocketTransactionCommitment::Finalized)
            .with_filter(WebsocketLogsFilter::All);

        let event = parse_logs_notification(&mut payload, &config)
            .expect("logs notification should parse")
            .expect("transaction log event");

        assert_eq!(event.slot, 88);
        assert_eq!(event.commitment_status, TxCommitmentStatus::Finalized);
        assert_eq!(event.signature, signature.into());
        assert_eq!(event.logs.as_ref(), &["Program log: test".to_owned()]);
        assert_eq!(event.matched_filter, None);
    }

    #[test]
    fn websocket_account_notification_decodes_account_update_event() {
        let pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let payload = json!({
            "jsonrpc":"2.0",
            "method":"accountNotification",
            "params":{
                "result":{
                    "context":{"slot":77},
                    "value":{
                        "lamports":42,
                        "data":[STANDARD.encode([1_u8, 2, 3, 4]), "base64"],
                        "owner":owner.to_string(),
                        "executable":false,
                        "rentEpoch":9
                    }
                }
            }
        })
        .to_string()
        .into_bytes();
        let mut payload = payload;
        let mut json_buffers = SimdJsonBuffers::default();
        let mut scratch = Vec::new();
        let mut watermarks = ProviderCommitmentWatermarks::default();
        let config = WebsocketTransactionConfig::new("wss://example.invalid")
            .with_stream(WebsocketPrimaryStream::Account(pubkey))
            .with_commitment(WebsocketTransactionCommitment::Confirmed);

        let event = parse_account_notification(
            &mut payload,
            &mut json_buffers,
            &mut scratch,
            &config,
            &mut watermarks,
        )
        .expect("account notification should parse")
        .expect("account update event");

        assert_eq!(event.slot, 77);
        assert_eq!(event.commitment_status, TxCommitmentStatus::Confirmed);
        assert_eq!(event.confirmed_slot, Some(77));
        assert_eq!(event.finalized_slot, None);
        assert_eq!(event.pubkey, pubkey.into());
        assert_eq!(event.owner, owner.into());
        assert_eq!(event.lamports, 42);
        assert_eq!(event.rent_epoch, 9);
        assert_eq!(event.data.as_ref(), &[1, 2, 3, 4]);
        assert_eq!(event.matched_filter, Some(pubkey.into()));
    }

    #[test]
    fn websocket_program_notification_decodes_account_update_event() {
        let program_id = Pubkey::new_unique();
        let account_pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let payload = json!({
            "jsonrpc":"2.0",
            "method":"programNotification",
            "params":{
                "result":{
                    "context":{"slot":88},
                    "value":{
                        "pubkey":account_pubkey.to_string(),
                        "account":{
                            "lamports":7,
                            "data":[STANDARD.encode([5_u8, 6, 7]), "base64"],
                            "owner":owner.to_string(),
                            "executable":true,
                            "rentEpoch":11
                        }
                    }
                }
            }
        })
        .to_string()
        .into_bytes();
        let mut payload = payload;
        let mut json_buffers = SimdJsonBuffers::default();
        let mut scratch = Vec::new();
        let mut watermarks = ProviderCommitmentWatermarks::default();
        let config = WebsocketTransactionConfig::new("wss://example.invalid")
            .with_stream(WebsocketPrimaryStream::Program(program_id))
            .with_commitment(WebsocketTransactionCommitment::Finalized);

        let event = parse_account_notification(
            &mut payload,
            &mut json_buffers,
            &mut scratch,
            &config,
            &mut watermarks,
        )
        .expect("program notification should parse")
        .expect("account update event");

        assert_eq!(event.slot, 88);
        assert_eq!(event.commitment_status, TxCommitmentStatus::Finalized);
        assert_eq!(event.confirmed_slot, Some(88));
        assert_eq!(event.finalized_slot, Some(88));
        assert_eq!(event.pubkey, account_pubkey.into());
        assert_eq!(event.owner, owner.into());
        assert!(event.executable);
        assert_eq!(event.data.as_ref(), &[5, 6, 7]);
        assert_eq!(event.matched_filter, Some(program_id.into()));
    }

    #[tokio::test]
    async fn websocket_spawn_rejects_transaction_only_options_for_program_stream() {
        let (tx, _rx) = create_provider_stream_queue(1);
        let config = WebsocketTransactionConfig::new("ws://127.0.0.1:1")
            .with_stream(WebsocketPrimaryStream::Program(Pubkey::new_unique()))
            .with_vote(true);

        let error = spawn_websocket_source(&config, tx)
            .await
            .expect_err("program stream should reject transaction-only filters");

        match error {
            WebsocketTransactionError::Config(WebsocketConfigError::UnsupportedOption {
                option: WebsocketConfigOption::VoteFilter,
                stream: WebsocketStreamKind::Program,
            }) => {}
            other => panic!("expected vote config rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn websocket_logs_source_delivers_log_update() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        let signature = Signature::from([9_u8; 64]);
        let payload = json!({
            "jsonrpc":"2.0",
            "method":"logsNotification",
            "params":{
                "result":{
                    "context":{"slot":88},
                    "value":{
                        "signature":signature.to_string(),
                        "err":null,
                        "logs":["Program log: test"]
                    }
                }
            }
        })
        .to_string();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = accept_async(stream).await.expect("websocket handshake");
            let subscribe = ws
                .next()
                .await
                .expect("subscribe frame")
                .expect("subscribe message");
            match subscribe {
                WsMessage::Text(text) => assert!(text.contains("logsSubscribe")),
                other => panic!("expected subscribe text frame, got {other:?}"),
            }
            ws.send(WsMessage::Text(
                String::from(r#"{"jsonrpc":"2.0","id":1,"result":42}"#).into(),
            ))
            .await
            .expect("ack");
            ws.send(WsMessage::Text(payload.into()))
                .await
                .expect("notification");
            ws.close(None).await.expect("close");
        });

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = WebsocketLogsConfig::new(format!("ws://{addr}"))
            .with_ping_interval(Duration::from_millis(250))
            .with_reconnect_delay(Duration::from_millis(10))
            .with_max_reconnect_attempts(1);
        let handle = spawn_websocket_logs_source(&config, tx)
            .await
            .expect("spawn websocket logs source");

        let event = loop {
            let update = timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("provider update timeout")
                .expect("provider update");
            match update {
                ProviderStreamUpdate::TransactionLog(event) => break event,
                ProviderStreamUpdate::Health(_) => continue,
                other => panic!("expected log update, got {other:?}"),
            }
        };
        assert_eq!(event.slot, 88);
        assert_eq!(event.signature, signature.into());

        handle.abort();
        handle.await.ok();
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn websocket_transaction_source_emits_initial_health_registration() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = accept_async(stream).await.expect("websocket handshake");
            let subscribe = ws
                .next()
                .await
                .expect("subscribe frame")
                .expect("subscribe message");
            match subscribe {
                WsMessage::Text(text) => assert!(text.contains("transactionSubscribe")),
                other => panic!("expected subscribe text frame, got {other:?}"),
            }
            ws.send(WsMessage::Text(
                String::from(r#"{"jsonrpc":"2.0","id":1,"result":42}"#).into(),
            ))
            .await
            .expect("ack");
            tokio::time::sleep(Duration::from_millis(50)).await;
            ws.close(None).await.expect("close");
        });

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = WebsocketTransactionConfig::new(format!("ws://{addr}"))
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10));
        let handle = spawn_websocket_source(&config, tx)
            .await
            .expect("spawn websocket transaction source");

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
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn websocket_transaction_source_emits_removed_health_on_abort() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = accept_async(stream).await.expect("websocket handshake");
            let subscribe = ws
                .next()
                .await
                .expect("subscribe frame")
                .expect("subscribe message");
            match subscribe {
                WsMessage::Text(text) => assert!(text.contains("transactionSubscribe")),
                other => panic!("expected subscribe text frame, got {other:?}"),
            }
            ws.send(WsMessage::Text(
                String::from(r#"{"jsonrpc":"2.0","id":1,"result":42}"#).into(),
            ))
            .await
            .expect("ack");
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = ws.close(None).await;
        });

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = WebsocketTransactionConfig::new(format!("ws://{addr}"))
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10));
        let handle = spawn_websocket_source(&config, tx)
            .await
            .expect("spawn websocket transaction source");

        let first = timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("provider update timeout")
            .expect("provider update");
        let ProviderStreamUpdate::Health(first) = first else {
            panic!("expected initial provider health update");
        };
        assert_eq!(first.status, ProviderSourceHealthStatus::Reconnecting);

        handle.abort();
        handle.await.ok();

        let second = timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("removed provider update timeout")
            .expect("removed provider update");
        let ProviderStreamUpdate::Health(second) = second else {
            panic!("expected removed provider health update");
        };
        assert_eq!(second.status, ProviderSourceHealthStatus::Removed);
        assert_eq!(second.reason, ProviderSourceHealthReason::SourceRemoved);

        server.await.expect("server task");
    }

    #[tokio::test]
    async fn websocket_account_source_delivers_account_update() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        let pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let payload = json!({
            "jsonrpc":"2.0",
            "method":"accountNotification",
            "params":{
                "result":{
                    "context":{"slot":77},
                    "value":{
                        "lamports":42,
                        "data":[STANDARD.encode([1_u8, 2, 3, 4]), "base64"],
                        "owner":owner.to_string(),
                        "executable":false,
                        "rentEpoch":9
                    }
                }
            }
        })
        .to_string();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = accept_async(stream).await.expect("websocket handshake");
            let subscribe = ws
                .next()
                .await
                .expect("subscribe frame")
                .expect("subscribe message");
            match subscribe {
                WsMessage::Text(text) => assert!(text.contains("accountSubscribe")),
                other => panic!("expected subscribe text frame, got {other:?}"),
            }
            ws.send(WsMessage::Text(
                String::from(r#"{"jsonrpc":"2.0","id":1,"result":42}"#).into(),
            ))
            .await
            .expect("ack");
            ws.send(WsMessage::Text(payload.into()))
                .await
                .expect("notification");
            ws.close(None).await.expect("close");
        });

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = WebsocketTransactionConfig::new(format!("ws://{addr}"))
            .with_stream(WebsocketPrimaryStream::Account(pubkey))
            .with_source_instance("websocket-account-primary")
            .with_ping_interval(Duration::from_millis(250))
            .with_reconnect_delay(Duration::from_millis(10))
            .with_max_reconnect_attempts(1);
        let handle = spawn_websocket_source(&config, tx)
            .await
            .expect("spawn websocket account source");

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
        assert_eq!(event.slot, 77);
        assert_eq!(event.pubkey, pubkey.into());
        assert_eq!(event.owner, owner.into());
        assert_eq!(event.data.as_ref(), &[1, 2, 3, 4]);
        assert_eq!(
            event
                .provider_source
                .as_deref()
                .expect("provider source")
                .instance_str(),
            "websocket-account-primary"
        );

        handle.abort();
        handle.await.ok();
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn websocket_program_source_delivers_account_update() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        let program_id = Pubkey::new_unique();
        let account_pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let payload = json!({
            "jsonrpc":"2.0",
            "method":"programNotification",
            "params":{
                "result":{
                    "context":{"slot":88},
                    "value":{
                        "pubkey":account_pubkey.to_string(),
                        "account":{
                            "lamports":7,
                            "data":[STANDARD.encode([5_u8, 6, 7]), "base64"],
                            "owner":owner.to_string(),
                            "executable":true,
                            "rentEpoch":11
                        }
                    }
                }
            }
        })
        .to_string();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = accept_async(stream).await.expect("websocket handshake");
            let subscribe = ws
                .next()
                .await
                .expect("subscribe frame")
                .expect("subscribe message");
            match subscribe {
                WsMessage::Text(text) => assert!(text.contains("programSubscribe")),
                other => panic!("expected subscribe text frame, got {other:?}"),
            }
            ws.send(WsMessage::Text(
                String::from(r#"{"jsonrpc":"2.0","id":1,"result":42}"#).into(),
            ))
            .await
            .expect("ack");
            ws.send(WsMessage::Text(payload.into()))
                .await
                .expect("notification");
            ws.close(None).await.expect("close");
        });

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = WebsocketTransactionConfig::new(format!("ws://{addr}"))
            .with_stream(WebsocketPrimaryStream::Program(program_id))
            .with_ping_interval(Duration::from_millis(250))
            .with_reconnect_delay(Duration::from_millis(10))
            .with_max_reconnect_attempts(1);
        let handle = spawn_websocket_source(&config, tx)
            .await
            .expect("spawn websocket program source");

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
        assert_eq!(event.slot, 88);
        assert_eq!(event.pubkey, account_pubkey.into());
        assert_eq!(event.owner, owner.into());
        assert_eq!(event.matched_filter, Some(program_id.into()));
        assert_eq!(event.data.as_ref(), &[5, 6, 7]);

        handle.abort();
        handle.await.ok();
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn websocket_source_uses_first_acknowledged_session_as_live_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        let payload = sample_notification_payload();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = accept_async(stream).await.expect("websocket handshake");
            let subscribe = ws
                .next()
                .await
                .expect("subscribe frame")
                .expect("subscribe message");
            match subscribe {
                WsMessage::Text(text) => {
                    assert!(text.contains("transactionSubscribe"));
                }
                other @ WsMessage::Binary(_)
                | other @ WsMessage::Ping(_)
                | other @ WsMessage::Pong(_)
                | other @ WsMessage::Close(_)
                | other @ WsMessage::Frame(_) => {
                    panic!("expected subscribe text frame, got {other:?}");
                }
            }
            ws.send(WsMessage::Text(
                String::from(r#"{"jsonrpc":"2.0","id":1,"result":42}"#).into(),
            ))
            .await
            .expect("ack");
            ws.send(WsMessage::Text(
                String::from_utf8(payload)
                    .expect("notification utf8")
                    .into(),
            ))
            .await
            .expect("notification");
            ws.close(None).await.expect("close");
        });

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = WebsocketTransactionConfig::new(format!("ws://{addr}"))
            .with_max_reconnect_attempts(1)
            .with_reconnect_delay(Duration::from_millis(10));
        let handle = spawn_websocket_source(&config, tx)
            .await
            .expect("spawn websocket source");

        let event = loop {
            let update = timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("provider update timeout")
                .expect("provider update");
            match update {
                ProviderStreamUpdate::SerializedTransaction(event) => break event,
                ProviderStreamUpdate::Health(_) => continue,
                other @ ProviderStreamUpdate::Transaction(_)
                | other @ ProviderStreamUpdate::TransactionLog(_)
                | other @ ProviderStreamUpdate::TransactionStatus(_)
                | other @ ProviderStreamUpdate::TransactionViewBatch(_)
                | other @ ProviderStreamUpdate::AccountUpdate(_)
                | other @ ProviderStreamUpdate::BlockMeta(_)
                | other @ ProviderStreamUpdate::RecentBlockhash(_)
                | other @ ProviderStreamUpdate::SlotStatus(_)
                | other @ ProviderStreamUpdate::ClusterTopology(_)
                | other @ ProviderStreamUpdate::LeaderSchedule(_)
                | other @ ProviderStreamUpdate::Reorg(_) => {
                    panic!("expected transaction update, got {other:?}");
                }
            }
        };
        assert_eq!(event.slot, 55);
        assert!(event.signature.is_some());

        handle.abort();
        handle.await.ok();
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn websocket_source_reconnects_and_delivers_after_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        let payload = sample_notification_payload();

        let server = tokio::spawn(async move {
            for attempt in 0..2 {
                let (stream, _) = listener.accept().await.expect("accept");
                let mut ws = accept_async(stream).await.expect("websocket handshake");
                let subscribe = ws
                    .next()
                    .await
                    .expect("subscribe frame")
                    .expect("subscribe message");
                match subscribe {
                    WsMessage::Text(text) => {
                        assert!(text.contains("transactionSubscribe"));
                    }
                    other @ WsMessage::Binary(_)
                    | other @ WsMessage::Ping(_)
                    | other @ WsMessage::Pong(_)
                    | other @ WsMessage::Close(_)
                    | other @ WsMessage::Frame(_) => {
                        panic!("expected subscribe text frame, got {other:?}");
                    }
                }
                ws.send(WsMessage::Text(
                    String::from(r#"{"jsonrpc":"2.0","id":1,"result":42}"#).into(),
                ))
                .await
                .expect("ack");
                if attempt == 0 {
                    ws.close(None).await.expect("close first session");
                    continue;
                }
                ws.send(WsMessage::Text(
                    String::from_utf8(payload.clone())
                        .expect("notification utf8")
                        .into(),
                ))
                .await
                .expect("notification");
                ws.close(None).await.expect("close second session");
                break;
            }
        });

        let (tx, mut rx) = create_provider_stream_queue(8);
        let config = WebsocketTransactionConfig::new(format!("ws://{addr}"))
            .with_max_reconnect_attempts(1)
            .with_ping_interval(Duration::from_millis(250))
            .with_reconnect_delay(Duration::from_millis(10));
        let handle = spawn_websocket_source(&config, tx)
            .await
            .expect("spawn websocket source");

        let mut saw_health = false;
        let event = loop {
            let update = timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("provider update timeout")
                .expect("provider update");
            match update {
                ProviderStreamUpdate::Health(event) => {
                    saw_health = true;
                    assert_eq!(
                        event.source.kind,
                        crate::provider_stream::ProviderSourceId::WebsocketTransaction
                    );
                    continue;
                }
                ProviderStreamUpdate::SerializedTransaction(event) => break event,
                other @ ProviderStreamUpdate::Transaction(_)
                | other @ ProviderStreamUpdate::TransactionLog(_)
                | other @ ProviderStreamUpdate::TransactionStatus(_)
                | other @ ProviderStreamUpdate::TransactionViewBatch(_)
                | other @ ProviderStreamUpdate::AccountUpdate(_)
                | other @ ProviderStreamUpdate::BlockMeta(_)
                | other @ ProviderStreamUpdate::RecentBlockhash(_)
                | other @ ProviderStreamUpdate::SlotStatus(_)
                | other @ ProviderStreamUpdate::ClusterTopology(_)
                | other @ ProviderStreamUpdate::LeaderSchedule(_)
                | other @ ProviderStreamUpdate::Reorg(_) => {
                    panic!("expected transaction update, got {other:?}");
                }
            }
        };
        assert!(saw_health);
        assert_eq!(event.slot, 55);
        assert!(event.signature.is_some());
        assert!(!event.bytes.is_empty());

        handle.abort();
        handle.await.ok();
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn websocket_fan_in_delivers_updates_from_multiple_sources() {
        let tx_listener = TcpListener::bind("127.0.0.1:0").await.expect("tx listener");
        let tx_addr = tx_listener.local_addr().expect("tx local addr");
        let tx_payload = sample_notification_payload();

        let logs_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("logs listener");
        let logs_addr = logs_listener.local_addr().expect("logs local addr");
        let log_signature = Signature::from([7_u8; 64]);
        let logs_payload = json!({
            "jsonrpc":"2.0",
            "method":"logsNotification",
            "params":{
                "result":{
                    "context":{"slot":101},
                    "value":{
                        "signature":log_signature.to_string(),
                        "err":null,
                        "logs":["Program log: fan-in"]
                    }
                }
            }
        })
        .to_string();

        let tx_server = tokio::spawn(async move {
            let (stream, _) = tx_listener.accept().await.expect("accept tx");
            let mut ws = accept_async(stream).await.expect("websocket handshake");
            let subscribe = ws
                .next()
                .await
                .expect("subscribe frame")
                .expect("subscribe message");
            match subscribe {
                WsMessage::Text(text) => assert!(text.contains("transactionSubscribe")),
                other => panic!("expected tx subscribe text frame, got {other:?}"),
            }
            ws.send(WsMessage::Text(
                String::from(r#"{"jsonrpc":"2.0","id":1,"result":42}"#).into(),
            ))
            .await
            .expect("tx ack");
            ws.send(WsMessage::Text(
                String::from_utf8(tx_payload)
                    .expect("tx payload utf8")
                    .into(),
            ))
            .await
            .expect("tx notification");
            ws.close(None).await.expect("tx close");
        });

        let logs_server = tokio::spawn(async move {
            let (stream, _) = logs_listener.accept().await.expect("accept logs");
            let mut ws = accept_async(stream).await.expect("websocket handshake");
            let subscribe = ws
                .next()
                .await
                .expect("subscribe frame")
                .expect("subscribe message");
            match subscribe {
                WsMessage::Text(text) => assert!(text.contains("logsSubscribe")),
                other => panic!("expected logs subscribe text frame, got {other:?}"),
            }
            ws.send(WsMessage::Text(
                String::from(r#"{"jsonrpc":"2.0","id":1,"result":42}"#).into(),
            ))
            .await
            .expect("logs ack");
            ws.send(WsMessage::Text(logs_payload.into()))
                .await
                .expect("logs notification");
            ws.close(None).await.expect("logs close");
        });

        let (fan_in, mut rx) = create_provider_stream_fan_in(8);
        let tx_handle = fan_in
            .spawn_websocket_source(
                &WebsocketTransactionConfig::new(format!("ws://{tx_addr}"))
                    .with_max_reconnect_attempts(1)
                    .with_reconnect_delay(Duration::from_millis(10)),
            )
            .await
            .expect("spawn websocket transaction source");
        let logs_handle = fan_in
            .spawn_websocket_logs_source(
                &WebsocketLogsConfig::new(format!("ws://{logs_addr}"))
                    .with_max_reconnect_attempts(1)
                    .with_reconnect_delay(Duration::from_millis(10)),
            )
            .await
            .expect("spawn websocket logs source");

        let mut saw_transaction = false;
        let mut saw_logs = false;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && (!saw_transaction || !saw_logs) {
            let update = timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("provider update timeout")
                .expect("provider update");
            match update {
                ProviderStreamUpdate::SerializedTransaction(event) => {
                    saw_transaction = true;
                    assert_eq!(event.slot, 55);
                }
                ProviderStreamUpdate::TransactionLog(event) => {
                    saw_logs = true;
                    assert_eq!(event.slot, 101);
                    assert_eq!(event.signature, log_signature.into());
                }
                ProviderStreamUpdate::Health(_) => {}
                other => panic!("unexpected fan-in update: {other:?}"),
            }
        }
        assert!(saw_transaction);
        assert!(saw_logs);

        tx_handle.abort();
        logs_handle.abort();
        tx_handle.await.ok();
        logs_handle.await.ok();
        tx_server.await.expect("tx server task");
        logs_server.await.expect("logs server task");
    }

    #[tokio::test]
    async fn websocket_fan_in_preserves_source_identity_for_same_kind_sources() {
        let listener_a = TcpListener::bind("127.0.0.1:0").await.expect("listener a");
        let addr_a = listener_a.local_addr().expect("addr a");
        let payload_a = sample_notification_payload();

        let listener_b = TcpListener::bind("127.0.0.1:0").await.expect("listener b");
        let addr_b = listener_b.local_addr().expect("addr b");
        let payload_b = sample_notification_payload();

        let server_a = tokio::spawn(async move {
            let (stream, _) = listener_a.accept().await.expect("accept a");
            let mut ws = accept_async(stream).await.expect("websocket handshake a");
            let subscribe = ws
                .next()
                .await
                .expect("subscribe a")
                .expect("subscribe msg a");
            match subscribe {
                WsMessage::Text(text) => assert!(text.contains("transactionSubscribe")),
                other => panic!("expected subscribe text frame, got {other:?}"),
            }
            ws.send(WsMessage::Text(
                String::from(r#"{"jsonrpc":"2.0","id":1,"result":42}"#).into(),
            ))
            .await
            .expect("ack a");
            ws.send(WsMessage::Text(
                String::from_utf8(payload_a).expect("payload utf8 a").into(),
            ))
            .await
            .expect("notification a");
            ws.close(None).await.expect("close a");
        });

        let server_b = tokio::spawn(async move {
            let (stream, _) = listener_b.accept().await.expect("accept b");
            let mut ws = accept_async(stream).await.expect("websocket handshake b");
            let subscribe = ws
                .next()
                .await
                .expect("subscribe b")
                .expect("subscribe msg b");
            match subscribe {
                WsMessage::Text(text) => assert!(text.contains("transactionSubscribe")),
                other => panic!("expected subscribe text frame, got {other:?}"),
            }
            ws.send(WsMessage::Text(
                String::from(r#"{"jsonrpc":"2.0","id":1,"result":42}"#).into(),
            ))
            .await
            .expect("ack b");
            ws.send(WsMessage::Text(
                String::from_utf8(payload_b).expect("payload utf8 b").into(),
            ))
            .await
            .expect("notification b");
            ws.close(None).await.expect("close b");
        });

        let (fan_in, mut rx) = create_provider_stream_fan_in(8);
        let handle_a = fan_in
            .spawn_websocket_source(
                &WebsocketTransactionConfig::new(format!("ws://{addr_a}"))
                    .with_max_reconnect_attempts(1)
                    .with_reconnect_delay(Duration::from_millis(10)),
            )
            .await
            .expect("spawn source a");
        let handle_b = fan_in
            .spawn_websocket_source(
                &WebsocketTransactionConfig::new(format!("ws://{addr_b}"))
                    .with_max_reconnect_attempts(1)
                    .with_reconnect_delay(Duration::from_millis(10)),
            )
            .await
            .expect("spawn source b");

        let mut sources = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && sources.len() < 2 {
            let update = timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("provider update timeout")
                .expect("provider update");
            match update {
                ProviderStreamUpdate::SerializedTransaction(event) => {
                    sources.push(
                        event
                            .provider_source
                            .expect("serialized transaction should retain provider source"),
                    );
                }
                ProviderStreamUpdate::Health(_) => {}
                other => panic!("unexpected update: {other:?}"),
            }
        }

        assert_eq!(sources.len(), 2);
        assert_eq!(
            sources[0].kind,
            crate::provider_stream::ProviderSourceId::WebsocketTransaction
        );
        assert_eq!(
            sources[1].kind,
            crate::provider_stream::ProviderSourceId::WebsocketTransaction
        );
        assert_ne!(sources[0], sources[1]);

        handle_a.abort();
        handle_a.await.ok();
        handle_b.abort();
        handle_b.await.ok();
        server_a.await.expect("server a task");
        server_b.await.expect("server b task");
    }

    #[tokio::test]
    async fn websocket_fan_in_rejects_duplicate_stable_source_identity() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("addr");
        let payload = sample_notification_payload();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut ws = accept_async(stream).await.expect("websocket handshake");
            let subscribe = ws.next().await.expect("subscribe").expect("subscribe msg");
            match subscribe {
                WsMessage::Text(text) => assert!(text.contains("transactionSubscribe")),
                other => panic!("expected subscribe text frame, got {other:?}"),
            }
            ws.send(WsMessage::Text(
                String::from(r#"{"jsonrpc":"2.0","id":1,"result":42}"#).into(),
            ))
            .await
            .expect("ack");
            ws.send(WsMessage::Text(
                String::from_utf8(payload).expect("payload utf8").into(),
            ))
            .await
            .expect("notification");
            ws.close(None).await.expect("close");
        });

        let (fan_in, _rx) = create_provider_stream_fan_in(8);
        let first = fan_in
            .spawn_websocket_source(
                &WebsocketTransactionConfig::new(format!("ws://{addr}"))
                    .with_source_instance("shared-primary")
                    .with_max_reconnect_attempts(1)
                    .with_reconnect_delay(Duration::from_millis(10)),
            )
            .await
            .expect("spawn first source");

        let error = fan_in
            .spawn_websocket_source(
                &WebsocketTransactionConfig::new("ws://127.0.0.1:9")
                    .with_source_instance("shared-primary"),
            )
            .await
            .expect_err("duplicate source identity should be rejected");

        match error {
            WebsocketTransactionError::DuplicateSourceIdentity(error) => {
                assert_eq!(error.0.kind, ProviderSourceId::WebsocketTransaction);
                assert_eq!(error.0.instance_str(), "shared-primary");
            }
            other => panic!("expected duplicate source identity error, got {other:?}"),
        }

        first.abort();
        first.await.ok();
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn websocket_spawn_fails_fast_on_dead_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        drop(listener);

        let (tx, mut rx) = create_provider_stream_queue(4);
        let error =
            spawn_websocket_source(&WebsocketTransactionConfig::new(format!("ws://{addr}")), tx)
                .await
                .expect_err("dead endpoint should fail during preflight");
        assert!(error.to_string().contains("IO error") || error.to_string().contains("Connection"));

        let first = timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("first provider update timeout")
            .expect("first provider update");
        let ProviderStreamUpdate::Health(first) = first else {
            panic!("expected initial health update");
        };
        assert_eq!(first.status, ProviderSourceHealthStatus::Reconnecting);
        assert_eq!(
            first.reason,
            ProviderSourceHealthReason::InitialConnectPending
        );

        let second = timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("second provider update timeout")
            .expect("second provider update");
        let ProviderStreamUpdate::Health(second) = second else {
            panic!("expected removal health update");
        };
        assert_eq!(second.status, ProviderSourceHealthStatus::Removed);
    }

    #[tokio::test]
    async fn websocket_spawn_fails_fast_on_dead_endpoint_with_full_queue() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        drop(listener);

        let (tx, mut rx) = create_provider_stream_queue(1);
        tx.try_send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
            source: ProviderSourceIdentity::new(ProviderSourceId::WebsocketTransaction, "busy"),
            readiness: ProviderSourceReadiness::Optional,
            status: ProviderSourceHealthStatus::Healthy,
            reason: ProviderSourceHealthReason::SubscriptionAckReceived,
            message: "occupied".to_owned(),
        }))
        .expect("fill provider queue");

        let error = timeout(
            Duration::from_secs(1),
            spawn_websocket_source(&WebsocketTransactionConfig::new(format!("ws://{addr}")), tx),
        )
        .await
        .expect("spawn should not block on full queue")
        .expect_err("dead endpoint should still fail during preflight");
        assert!(error.to_string().contains("IO error") || error.to_string().contains("Connection"));

        let occupied = timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("occupied update timeout")
            .expect("occupied update");
        let ProviderStreamUpdate::Health(occupied) = occupied else {
            panic!("expected occupied health update");
        };
        assert_eq!(occupied.message, "occupied");

        let removed = timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("removed update timeout")
            .expect("removed update");
        let ProviderStreamUpdate::Health(removed) = removed else {
            panic!("expected removal health update");
        };
        assert_eq!(removed.status, ProviderSourceHealthStatus::Removed);
    }

    #[tokio::test]
    async fn websocket_spawn_fails_fast_without_replay_http_endpoint() {
        let (tx, _rx) = create_provider_stream_queue(1);
        let error = spawn_websocket_source(
            &WebsocketTransactionConfig::new("http://example.invalid"),
            tx,
        )
        .await
        .expect_err("missing replay http endpoint should fail during preflight");
        assert!(
            error
                .to_string()
                .contains("websocket reconnect replay requires")
        );
    }

    #[test]
    #[ignore = "profiling fixture for websocket transaction parsing A/B"]
    fn websocket_transaction_parse_profile_fixture() {
        let iterations = profile_iterations(200_000);

        let payload = sample_notification_payload();

        let baseline_started = Instant::now();
        for _ in 0..iterations {
            let mut frame = payload.clone();
            let event = materialize_transaction_baseline(
                &mut frame,
                WebsocketTransactionCommitment::Confirmed,
            )
            .expect("baseline parse")
            .expect("baseline event");
            std::hint::black_box(event);
        }
        let baseline_elapsed = baseline_started.elapsed();

        let optimized_started = Instant::now();
        let mut frame_bytes = Vec::new();
        let mut json_buffers = SimdJsonBuffers::default();
        let mut tx_bytes = Vec::new();
        let mut watermarks = ProviderCommitmentWatermarks::default();
        for _ in 0..iterations {
            let frame = frame_bytes_mut(&mut frame_bytes, &payload);
            let event = parse_transaction_notification(
                frame,
                &mut json_buffers,
                &mut tx_bytes,
                WebsocketTransactionCommitment::Confirmed,
                &mut watermarks,
            )
            .expect("optimized parse")
            .expect("optimized event");
            std::hint::black_box(event);
        }
        let optimized_elapsed = optimized_started.elapsed();

        eprintln!(
            "websocket_transaction_parse_profile_fixture iterations={} baseline_us={} optimized_us={}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
        );
    }

    #[test]
    #[ignore = "profiling fixture for baseline websocket transaction parsing"]
    fn websocket_transaction_parse_baseline_profile_fixture() {
        let iterations = profile_iterations(200_000);

        let payload = sample_notification_payload();
        for _ in 0..iterations {
            let mut frame = payload.clone();
            let event = materialize_transaction_baseline(
                &mut frame,
                WebsocketTransactionCommitment::Confirmed,
            )
            .expect("baseline parse")
            .expect("baseline event");
            std::hint::black_box(event);
        }
    }

    #[test]
    #[ignore = "profiling fixture for optimized websocket transaction parsing"]
    fn websocket_transaction_parse_optimized_profile_fixture() {
        let iterations = profile_iterations(200_000);

        let payload = sample_notification_payload();
        let mut frame_bytes = Vec::new();
        let mut json_buffers = SimdJsonBuffers::default();
        let mut tx_bytes = Vec::new();
        let mut watermarks = ProviderCommitmentWatermarks::default();
        for _ in 0..iterations {
            let frame = frame_bytes_mut(&mut frame_bytes, &payload);
            let event = parse_transaction_notification(
                frame,
                &mut json_buffers,
                &mut tx_bytes,
                WebsocketTransactionCommitment::Confirmed,
                &mut watermarks,
            )
            .expect("optimized parse")
            .expect("optimized event");
            std::hint::black_box(event);
        }
    }
}
