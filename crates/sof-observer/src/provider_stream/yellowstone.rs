#![allow(clippy::missing_docs_in_private_items)]

//! Yellowstone gRPC adapters for SOF processed provider-stream ingress.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
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
use tokio::task::JoinHandle;
use yellowstone_grpc_client::{GeyserGrpcBuilderError, GeyserGrpcClient, GeyserGrpcClientError};
use yellowstone_grpc_proto::prelude::{
    CommitmentLevel, SlotStatus, SubscribeRequest, SubscribeRequestFilterAccounts,
    SubscribeRequestFilterSlots, SubscribeRequestFilterTransactions, SubscribeRequestPing,
    SubscribeUpdate, subscribe_update::UpdateOneof,
};

use crate::{
    event::{ForkSlotStatus, TxCommitmentStatus, TxKind},
    framework::{
        AccountUpdateEvent, SlotStatusEvent, TransactionEvent, TransactionStatusEvent,
        pubkey_bytes, signature_bytes_opt,
    },
    provider_stream::{
        ProviderCommitmentWatermarks, ProviderReplayMode, ProviderSourceHealthEvent,
        ProviderSourceHealthReason, ProviderSourceHealthStatus, ProviderSourceId,
        ProviderStreamFanIn, ProviderStreamSender, ProviderStreamUpdate,
        classify_provider_transaction_kind,
    },
};

const INTERNAL_SLOT_FILTER: &str = "__sof_internal_slots";

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

/// Connection and filter config for Yellowstone transaction subscriptions.
#[derive(Clone, Debug)]
pub struct YellowstoneGrpcConfig {
    endpoint: String,
    x_token: Option<String>,
    stream: YellowstoneGrpcStream,
    commitment: YellowstoneGrpcCommitment,
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
            stream: YellowstoneGrpcStream::Transaction,
            commitment: YellowstoneGrpcCommitment::Processed,
            vote: None,
            failed: None,
            signature: None,
            account_include: Vec::new(),
            account_exclude: Vec::new(),
            account_required: Vec::new(),
            accounts: Vec::new(),
            owners: Vec::new(),
            require_txn_signature: false,
            max_decoding_message_size: 64 * 1024 * 1024,
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

    /// Selects the Yellowstone stream family this config should subscribe to.
    #[must_use]
    pub const fn with_stream(mut self, stream: YellowstoneGrpcStream) -> Self {
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

    #[cfg_attr(not(test), allow(dead_code))]
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
        }
    }

    const fn stream_kind(&self) -> YellowstoneGrpcStreamKind {
        match self.stream {
            YellowstoneGrpcStream::Transaction => YellowstoneGrpcStreamKind::Transaction,
            YellowstoneGrpcStream::TransactionStatus => {
                YellowstoneGrpcStreamKind::TransactionStatus
            }
            YellowstoneGrpcStream::Accounts => YellowstoneGrpcStreamKind::Accounts,
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
}

/// Connection and replay config for Yellowstone slot subscriptions.
#[derive(Clone, Debug)]
pub struct YellowstoneGrpcSlotsConfig {
    endpoint: String,
    x_token: Option<String>,
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
    /// Slot stream.
    Slots,
}

impl std::fmt::Display for YellowstoneGrpcStreamKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transaction => f.write_str("transaction"),
            Self::TransactionStatus => f.write_str("transaction-status"),
            Self::Accounts => f.write_str("account"),
            Self::Slots => f.write_str("slot"),
        }
    }
}

type YellowstoneSubscribeSink = std::pin::Pin<
    Box<dyn futures_util::Sink<SubscribeRequest, Error = futures_channel::mpsc::SendError> + Send>,
>;
type YellowstoneUpdateStream = std::pin::Pin<
    Box<
        dyn futures_util::Stream<
                Item = Result<SubscribeUpdate, yellowstone_grpc_proto::tonic::Status>,
            > + Send,
    >,
>;

/// Spawns one Yellowstone gRPC transaction forwarder into a SOF provider-stream queue.
///
/// # Examples
///
/// ```no_run
/// use sof::provider_stream::{
///     create_provider_stream_queue,
///     yellowstone::{spawn_yellowstone_grpc_transaction_source, YellowstoneGrpcConfig},
/// };
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let (tx, _rx) = create_provider_stream_queue(1024);
/// let handle = spawn_yellowstone_grpc_transaction_source(
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
pub async fn spawn_yellowstone_grpc_transaction_source(
    config: YellowstoneGrpcConfig,
    sender: ProviderStreamSender,
) -> Result<JoinHandle<Result<(), YellowstoneGrpcError>>, YellowstoneGrpcError> {
    let first_session = establish_yellowstone_session(&config, 0).await?;
    Ok(tokio::spawn(async move {
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
                    &sender,
                    &mut tracked_slot,
                    &mut watermarks,
                    &mut session_established,
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
                            &config,
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
                            &config,
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
                        &config,
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
                    &config,
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

/// Spawns one Yellowstone gRPC slot forwarder into a SOF provider-stream queue.
pub async fn spawn_yellowstone_grpc_slot_source(
    config: YellowstoneGrpcSlotsConfig,
    sender: ProviderStreamSender,
) -> Result<JoinHandle<Result<(), YellowstoneGrpcError>>, YellowstoneGrpcError> {
    let first_session = establish_yellowstone_slot_session(&config, 0).await?;
    Ok(tokio::spawn(async move {
        let mut attempts = 0_u32;
        let mut tracked_slot = 0_u64;
        let mut watermarks = ProviderCommitmentWatermarks::default();
        let mut slot_states = HashMap::new();
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
                    &sender,
                    &mut tracked_slot,
                    &mut watermarks,
                    &mut slot_states,
                    &mut session_established,
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
    sender: &ProviderStreamSender,
    tracked_slot: &mut u64,
    watermarks: &mut ProviderCommitmentWatermarks,
    session_established: &mut bool,
    mut subscribe_tx: YellowstoneSubscribeSink,
    mut stream: YellowstoneUpdateStream,
) -> Result<(), YellowstoneGrpcError> {
    let commitment = config.commitment.as_tx_commitment();
    *session_established = false;
    *session_established = true;
    send_primary_provider_health(
        config,
        sender,
        ProviderSourceHealthStatus::Healthy,
        ProviderSourceHealthReason::SubscriptionAckReceived,
        PROVIDER_SUBSCRIPTION_ACKNOWLEDGED.to_owned(),
    )
    .await?;
    let mut ping = config.ping_interval.map(tokio::time::interval);
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
                        *tracked_slot = (*tracked_slot).max(tx_update.slot);
                        watermarks.observe_transaction_commitment(tx_update.slot, commitment);
                        let event = transaction_event_from_update(
                            tx_update.slot,
                            tx_update.transaction,
                            commitment,
                            *watermarks,
                        )?;
                        sender
                            .send(ProviderStreamUpdate::Transaction(event))
                            .await
                            .map_err(|_error| YellowstoneGrpcError::QueueClosed)?;
                    }
                    Some(UpdateOneof::TransactionStatus(status_update))
                        if config.stream == YellowstoneGrpcStream::TransactionStatus =>
                    {
                        *tracked_slot = (*tracked_slot).max(status_update.slot);
                        watermarks.observe_transaction_commitment(status_update.slot, commitment);
                        let event = transaction_status_event_from_update(
                            commitment,
                            *watermarks,
                            status_update,
                        )?;
                        sender
                            .send(ProviderStreamUpdate::TransactionStatus(event))
                            .await
                            .map_err(|_error| YellowstoneGrpcError::QueueClosed)?;
                    }
                    Some(UpdateOneof::Account(account_update))
                        if config.stream == YellowstoneGrpcStream::Accounts =>
                    {
                        *tracked_slot = (*tracked_slot).max(account_update.slot);
                        observe_non_transaction_commitment(watermarks, account_update.slot, commitment);
                        let event =
                            account_update_event_from_yellowstone(account_update, commitment, *watermarks)?;
                        sender
                            .send(ProviderStreamUpdate::AccountUpdate(event))
                            .await
                            .map_err(|_error| YellowstoneGrpcError::QueueClosed)?;
                    }
                    Some(UpdateOneof::Slot(slot_update)) => {
                        *tracked_slot = (*tracked_slot).max(slot_update.slot);
                        match SlotStatus::try_from(slot_update.status).ok() {
                            Some(SlotStatus::SlotConfirmed) => {
                                watermarks.observe_confirmed_slot(slot_update.slot);
                            }
                            Some(SlotStatus::SlotFinalized) => {
                                watermarks.observe_finalized_slot(slot_update.slot);
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
    sender: &ProviderStreamSender,
    tracked_slot: &mut u64,
    watermarks: &mut ProviderCommitmentWatermarks,
    slot_states: &mut HashMap<u64, ForkSlotStatus>,
    session_established: &mut bool,
    mut subscribe_tx: YellowstoneSubscribeSink,
    mut stream: YellowstoneUpdateStream,
) -> Result<(), YellowstoneGrpcError> {
    *session_established = false;
    *session_established = true;
    send_provider_slot_health(
        sender,
        ProviderSourceHealthStatus::Healthy,
        ProviderSourceHealthReason::SubscriptionAckReceived,
        PROVIDER_SUBSCRIPTION_ACKNOWLEDGED.to_owned(),
    )
    .await?;
    let mut ping = config.ping_interval.map(tokio::time::interval);
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
                        *tracked_slot = (*tracked_slot).max(slot_update.slot);
                        if let Some(event) = slot_status_event_from_update(
                            slot_update.slot,
                            slot_update.parent,
                            SlotStatus::try_from(slot_update.status).ok(),
                            watermarks,
                            slot_states,
                        ) {
                            sender
                                .send(ProviderStreamUpdate::SlotStatus(event))
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
    let mut builder = GeyserGrpcClient::build_from_shared(config.endpoint.clone())?
        .x_token(config.x_token.clone())?
        .max_decoding_message_size(config.max_decoding_message_size);
    if let Some(timeout) = config.connect_timeout {
        builder = builder.connect_timeout(timeout);
    }
    let mut client = builder.connect().await?;
    let (subscribe_tx, stream) = client
        .subscribe_with_request(Some(config.subscribe_request_with_state(tracked_slot)))
        .await?;
    Ok((Box::pin(subscribe_tx), Box::pin(stream)))
}

async fn establish_yellowstone_slot_session(
    config: &YellowstoneGrpcSlotsConfig,
    tracked_slot: u64,
) -> Result<(YellowstoneSubscribeSink, YellowstoneUpdateStream), YellowstoneGrpcError> {
    let mut builder = GeyserGrpcClient::build_from_shared(config.endpoint.clone())?
        .x_token(config.x_token.clone())?
        .max_decoding_message_size(64 * 1024 * 1024);
    if let Some(timeout) = config.connect_timeout {
        builder = builder.connect_timeout(timeout);
    }
    let mut client = builder.connect().await?;
    let (subscribe_tx, stream) = client
        .subscribe_with_request(Some(config.subscribe_request_with_state(tracked_slot)))
        .await?;
    Ok((Box::pin(subscribe_tx), Box::pin(stream)))
}

async fn send_primary_provider_health(
    config: &YellowstoneGrpcConfig,
    sender: &ProviderStreamSender,
    status: ProviderSourceHealthStatus,
    reason: ProviderSourceHealthReason,
    message: String,
) -> Result<(), YellowstoneGrpcError> {
    sender
        .send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
            source: config.source_id(),
            status,
            reason,
            message,
        }))
        .await
        .map_err(|_error| YellowstoneGrpcError::QueueClosed)
}

async fn send_provider_slot_health(
    sender: &ProviderStreamSender,
    status: ProviderSourceHealthStatus,
    reason: ProviderSourceHealthReason,
    message: String,
) -> Result<(), YellowstoneGrpcError> {
    sender
        .send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
            source: ProviderSourceId::YellowstoneGrpcSlots,
            status,
            reason,
            message,
        }))
        .await
        .map_err(|_error| YellowstoneGrpcError::QueueClosed)
}

const fn yellowstone_health_reason(error: &YellowstoneGrpcError) -> ProviderSourceHealthReason {
    match error {
        YellowstoneGrpcError::Build(_)
        | YellowstoneGrpcError::Client(_)
        | YellowstoneGrpcError::Status(_) => ProviderSourceHealthReason::UpstreamTransportFailure,
        YellowstoneGrpcError::Convert(_) | YellowstoneGrpcError::Protocol(_) => {
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
    let signature = if is_vote {
        Signature::try_from(transaction.signature.as_slice())
            .map(Some)
            .map_err(|_error| YellowstoneGrpcError::Convert("invalid signature"))?
    } else {
        None
    };
    let tx = convert_transaction(
        transaction
            .transaction
            .ok_or(YellowstoneGrpcError::Convert(
                "missing versioned transaction",
            ))?,
    )?;
    Ok(TransactionEvent {
        slot,
        commitment_status,
        confirmed_slot: watermarks.confirmed_slot,
        finalized_slot: watermarks.finalized_slot,
        signature: signature_bytes_opt(signature.or_else(|| tx.signatures.first().copied())),
        kind: if is_vote {
            TxKind::VoteOnly
        } else {
            classify_provider_transaction_kind(&tx)
        },
        tx: std::sync::Arc::new(tx),
    })
}

fn transaction_status_event_from_update(
    commitment_status: TxCommitmentStatus,
    watermarks: ProviderCommitmentWatermarks,
    update: yellowstone_grpc_proto::prelude::SubscribeUpdateTransactionStatus,
) -> Result<TransactionStatusEvent, YellowstoneGrpcError> {
    let signature = Signature::try_from(update.signature.as_slice())
        .map_err(|_error| YellowstoneGrpcError::Convert("invalid transaction-status signature"))?;
    Ok(TransactionStatusEvent {
        slot: update.slot,
        commitment_status,
        confirmed_slot: watermarks.confirmed_slot,
        finalized_slot: watermarks.finalized_slot,
        signature: signature.into(),
        is_vote: update.is_vote,
        index: Some(update.index),
        err: update.err.map(|error| format!("{error:?}")),
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
    let pubkey = Pubkey::try_from(account.pubkey.as_slice())
        .map_err(|_error| YellowstoneGrpcError::Convert("invalid account pubkey"))?;
    let owner = Pubkey::try_from(account.owner.as_slice())
        .map_err(|_error| YellowstoneGrpcError::Convert("invalid account owner"))?;
    let txn_signature = match account.txn_signature {
        Some(signature) => Some(
            Signature::try_from(signature.as_slice())
                .map_err(|_error| YellowstoneGrpcError::Convert("invalid account txn signature"))?,
        ),
        None => None,
    };
    Ok(AccountUpdateEvent {
        slot: update.slot,
        commitment_status,
        confirmed_slot: watermarks.confirmed_slot,
        finalized_slot: watermarks.finalized_slot,
        pubkey: pubkey_bytes(pubkey),
        owner: pubkey_bytes(owner),
        lamports: account.lamports,
        executable: account.executable,
        rent_epoch: account.rent_epoch,
        data: account.data.into(),
        write_version: Some(account.write_version),
        txn_signature: signature_bytes_opt(txn_signature),
        is_startup: update.is_startup,
        matched_filter: None,
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
    })
}

impl ProviderStreamFanIn {
    /// Spawns one Yellowstone gRPC transaction source into this fan-in.
    pub async fn spawn_yellowstone_grpc_transaction_source(
        &self,
        config: YellowstoneGrpcConfig,
    ) -> Result<JoinHandle<Result<(), YellowstoneGrpcError>>, YellowstoneGrpcError> {
        spawn_yellowstone_grpc_transaction_source(config, self.sender()).await
    }

    /// Spawns one Yellowstone gRPC slot source into this fan-in.
    pub async fn spawn_yellowstone_grpc_slot_source(
        &self,
        config: YellowstoneGrpcSlotsConfig,
    ) -> Result<JoinHandle<Result<(), YellowstoneGrpcError>>, YellowstoneGrpcError> {
        spawn_yellowstone_grpc_slot_source(config, self.sender()).await
    }
}

#[inline]
fn convert_transaction(
    tx: yellowstone_grpc_proto::prelude::Transaction,
) -> Result<VersionedTransaction, YellowstoneGrpcError> {
    let mut signatures = Vec::with_capacity(tx.signatures.len());
    for signature in tx.signatures {
        signatures.push(Signature::try_from(signature.as_slice()).map_err(|_error| {
            YellowstoneGrpcError::Convert("failed to parse transaction signature")
        })?);
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

#[inline]
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
            Pubkey::try_from(key.as_slice())
                .map_err(|_error| YellowstoneGrpcError::Convert("invalid account key"))?,
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
                account_key: Pubkey::try_from(lookup.account_key.as_slice()).map_err(|_error| {
                    YellowstoneGrpcError::Convert("invalid address table account key")
                })?,
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
mod tests {
    use super::*;
    use crate::event::TxKind;
    use solana_instruction::Instruction;
    use solana_keypair::Keypair;
    use solana_message::{Message as SolanaMessage, VersionedMessage};
    use solana_sdk_ids::system_program;
    use solana_sdk_ids::{compute_budget, vote};
    use solana_signer::Signer;
    use std::time::Instant;
    use yellowstone_grpc_proto::prelude::{
        CompiledInstruction as ProtoCompiledInstruction, Message as ProtoMessage,
        MessageAddressTableLookup as ProtoMessageAddressTableLookup,
        MessageHeader as ProtoMessageHeader, SubscribeUpdateTransactionInfo, Transaction,
    };

    fn profile_iterations(default: usize) -> usize {
        std::env::var("SOF_PROFILE_ITERATIONS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(default)
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

    fn transaction_event_from_update_baseline(
        slot: u64,
        transaction: Option<SubscribeUpdateTransactionInfo>,
        commitment_status: TxCommitmentStatus,
    ) -> Result<TransactionEvent, YellowstoneGrpcError> {
        let transaction =
            transaction.ok_or(YellowstoneGrpcError::Convert("missing transaction payload"))?;
        let signature = Signature::try_from(transaction.signature.as_slice())
            .map(crate::framework::signature_bytes)
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
            tx: std::sync::Arc::new(tx),
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
            std::hint::black_box(event);
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
            std::hint::black_box(event);
        }
        let optimized_elapsed = optimized_started.elapsed();

        eprintln!(
            "yellowstone_transaction_conversion_profile_fixture iterations={} baseline_us={} optimized_us={}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
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
            std::hint::black_box(event);
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
            std::hint::black_box(event);
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
            std::hint::black_box(event);
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
            std::hint::black_box(event);
        }
        let optimized_elapsed = optimized_started.elapsed();

        eprintln!(
            "yellowstone_vote_only_conversion_profile_fixture iterations={} baseline_us={} optimized_us={}",
            iterations,
            baseline_elapsed.as_micros(),
            optimized_elapsed.as_micros(),
        );
    }
}
