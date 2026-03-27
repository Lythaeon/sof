#![allow(clippy::missing_docs_in_private_items)]

//! Helius LaserStream adapters for SOF processed provider-stream ingress.
//!
//! LaserStream uses the Yellowstone-compatible gRPC subscription model. SOF's
//! built-in adapter intentionally stays on the shared transaction subscription
//! surface: commitment, signature, vote/failed flags, and account
//! include/exclude/required filters.

use std::{
    collections::HashMap,
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

use crate::{
    event::{TxCommitmentStatus, TxKind},
    framework::{TransactionEvent, signature_bytes_opt},
    provider_stream::{
        ProviderCommitmentWatermarks, ProviderReplayMode, ProviderSourceHealthEvent,
        ProviderSourceHealthReason, ProviderSourceHealthStatus, ProviderSourceId,
        ProviderStreamSender, ProviderStreamUpdate, classify_provider_transaction_kind,
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
    commitment: LaserStreamCommitment,
    vote: Option<bool>,
    failed: Option<bool>,
    signature: Option<Signature>,
    account_include: Vec<Pubkey>,
    account_exclude: Vec<Pubkey>,
    account_required: Vec<Pubkey>,
    connect_timeout: Option<Duration>,
    timeout: Option<Duration>,
    stall_timeout: Option<Duration>,
    max_decoding_message_size: Option<usize>,
    max_encoding_message_size: Option<usize>,
    reconnect_delay: Duration,
    max_reconnect_attempts: Option<u32>,
    replay_mode: ProviderReplayMode,
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
            commitment: LaserStreamCommitment::Processed,
            vote: None,
            failed: None,
            signature: None,
            account_include: Vec::new(),
            account_exclude: Vec::new(),
            account_required: Vec::new(),
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

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn subscribe_request(&self) -> grpc::SubscribeRequest {
        self.subscribe_request_with_state(0)
    }

    pub(crate) fn subscribe_request_with_state(&self, tracked_slot: u64) -> grpc::SubscribeRequest {
        let filter = grpc::SubscribeRequestFilterTransactions {
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
        };
        grpc::SubscribeRequest {
            slots: HashMap::from([(
                INTERNAL_WATERMARK_SLOT_FILTER.to_owned(),
                grpc::SubscribeRequestFilterSlots {
                    filter_by_commitment: Some(true),
                    ..grpc::SubscribeRequestFilterSlots::default()
                },
            )]),
            transactions: HashMap::from([("sof".to_owned(), filter)]),
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
    /// Stream status or transport errors from the SDK.
    #[error(transparent)]
    Client(#[from] ClientError),
    /// Provider update could not be converted into a SOF event.
    #[error("laserstream transaction conversion failed: {0}")]
    Convert(&'static str),
    /// LaserStream protocol/runtime failure.
    #[error("laserstream protocol error: {0}")]
    Protocol(String),
    /// Provider-stream queue is closed.
    #[error("provider-stream queue closed")]
    QueueClosed,
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

/// Spawns one LaserStream transaction forwarder into a SOF provider-stream queue.
///
/// # Examples
///
/// ```no_run
/// use sof::provider_stream::{
///     create_provider_stream_queue,
///     laserstream::{spawn_laserstream_transaction_source, LaserStreamConfig},
/// };
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let (tx, _rx) = create_provider_stream_queue(1024);
/// let config =
///     LaserStreamConfig::new("https://laserstream-mainnet-fra.helius-rpc.com", "api-key");
/// let handle = spawn_laserstream_transaction_source(config, tx).await?;
/// handle.abort();
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// Returns any connection/bootstrap error before the forwarder task starts.
pub async fn spawn_laserstream_transaction_source(
    config: LaserStreamConfig,
    sender: ProviderStreamSender,
) -> Result<JoinHandle<Result<(), LaserStreamError>>, LaserStreamError> {
    let first_session =
        connect_and_subscribe_once(&config, config.subscribe_request_with_state(0)).await?;
    Ok(tokio::spawn(async move {
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
                Ok((subscribe_tx, stream)) => match run_laserstream_transaction_connection(
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
                        let detail = "laserstream stream ended unexpectedly".to_owned();
                        tracing::warn!(
                            endpoint = config.endpoint(),
                            detail,
                            "provider stream laserstream session ended unexpectedly; reconnecting"
                        );
                        send_provider_health(
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
                        send_provider_health(
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
                    send_provider_health(
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
                send_provider_health(
                    &sender,
                    ProviderSourceHealthStatus::Unhealthy,
                    ProviderSourceHealthReason::ReconnectBudgetExhausted,
                    detail.clone(),
                )
                .await?;
                return Err(LaserStreamError::Protocol(detail));
            }
            tokio::time::sleep(config.reconnect_delay).await;
        }
    }))
}

async fn run_laserstream_transaction_connection(
    config: &LaserStreamConfig,
    sender: &ProviderStreamSender,
    tracked_slot: &mut u64,
    watermarks: &mut ProviderCommitmentWatermarks,
    session_established: &mut bool,
    _subscribe_tx: LaserStreamSubscribeSink,
    mut stream: LaserStreamUpdateStream,
) -> Result<(), LaserStreamError> {
    *session_established = false;
    let commitment = config.commitment.as_tx_commitment();
    *session_established = true;
    send_provider_health(
        sender,
        ProviderSourceHealthStatus::Healthy,
        ProviderSourceHealthReason::SubscriptionAckReceived,
        "subscription acknowledged".to_owned(),
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
                return Err(LaserStreamError::Protocol(
                    "laserstream stream stalled without inbound progress".to_owned(),
                ));
            }
            maybe_update = stream.next() => {
                let Some(update) = maybe_update else {
                    return Ok(());
                };
                let update = update
                    .map_err(|error| LaserStreamError::Protocol(format!("laserstream stream status: {error}")))?;
                last_progress = Instant::now();
                match update.update_oneof {
                    Some(grpc::subscribe_update::UpdateOneof::Slot(slot_update)) => {
                        *tracked_slot = (*tracked_slot).max(slot_update.slot);
                        if update
                            .filters
                            .iter()
                            .any(|filter| filter == INTERNAL_WATERMARK_SLOT_FILTER)
                        {
                            match grpc::SlotStatus::try_from(slot_update.status).ok() {
                                Some(grpc::SlotStatus::SlotConfirmed) => {
                                    watermarks.observe_confirmed_slot(slot_update.slot);
                                }
                                Some(grpc::SlotStatus::SlotFinalized) => {
                                    watermarks.observe_finalized_slot(slot_update.slot);
                                }
                                _ => {}
                            }
                        }
                    }
                    Some(grpc::subscribe_update::UpdateOneof::Transaction(tx_update)) => {
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
                            .map_err(|_error| LaserStreamError::QueueClosed)?;
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn send_provider_health(
    sender: &ProviderStreamSender,
    status: ProviderSourceHealthStatus,
    reason: ProviderSourceHealthReason,
    message: String,
) -> Result<(), LaserStreamError> {
    sender
        .send(ProviderStreamUpdate::Health(ProviderSourceHealthEvent {
            source: ProviderSourceId::LaserStream,
            status,
            reason,
            message,
        }))
        .await
        .map_err(|_error| LaserStreamError::QueueClosed)
}

const fn laserstream_health_reason(error: &LaserStreamError) -> ProviderSourceHealthReason {
    match error {
        LaserStreamError::Client(_) => ProviderSourceHealthReason::UpstreamTransportFailure,
        LaserStreamError::Convert(_) | LaserStreamError::Protocol(_) => {
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
        .map_err(|error| LaserStreamError::Protocol(error.to_string()))?;

    let mut endpoint = Endpoint::from_shared(config.endpoint.clone())
        .map_err(|error| LaserStreamError::Protocol(format!("failed to parse endpoint: {error}")))?
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
    endpoint = endpoint
        .tls_config(ClientTlsConfig::new().with_enabled_roots())
        .map_err(|error| LaserStreamError::Protocol(format!("TLS config error: {error}")))?;

    let channel = endpoint
        .connect()
        .await
        .map_err(|error| LaserStreamError::Protocol(format!("connection failed: {error}")))?;
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
    subscribe_tx.send(request).await.map_err(|error| {
        LaserStreamError::Protocol(format!(
            "failed to send initial subscription request: {error}"
        ))
    })?;
    let response = geyser_client
        .subscribe(subscribe_rx)
        .await
        .map_err(|error| LaserStreamError::Protocol(format!("subscription failed: {error}")))?;

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
    let signature = Signature::try_from(transaction.signature.as_slice())
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
        confirmed_slot: watermarks.confirmed_slot,
        finalized_slot: watermarks.finalized_slot,
        signature: signature_bytes_opt(signature),
        kind: if is_vote {
            TxKind::VoteOnly
        } else {
            classify_provider_transaction_kind(&tx)
        },
        tx: Arc::new(tx),
    })
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
mod tests {
    use super::*;
    use crate::provider_stream::yellowstone::{YellowstoneGrpcCommitment, YellowstoneGrpcConfig};
    use laserstream_core_proto::prelude::{
        CompiledInstruction as ProtoCompiledInstruction, Message as ProtoMessage,
        MessageAddressTableLookup as ProtoMessageAddressTableLookup,
        MessageHeader as ProtoMessageHeader, SubscribeUpdateTransactionInfo,
    };
    use solana_instruction::Instruction;
    use solana_keypair::Keypair;
    use solana_message::{Message, VersionedMessage};
    use solana_sdk_ids::{compute_budget, system_program, vote};
    use solana_signer::Signer;
    use std::time::Instant;

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

    fn transaction_event_from_update_baseline(
        slot: u64,
        transaction: Option<SubscribeUpdateTransactionInfo>,
        commitment_status: TxCommitmentStatus,
    ) -> Result<TransactionEvent, LaserStreamError> {
        let transaction =
            transaction.ok_or(LaserStreamError::Convert("missing transaction payload"))?;
        let signature = Signature::try_from(transaction.signature.as_slice())
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
