#![allow(clippy::missing_docs_in_private_items)]

//! Websocket `transactionSubscribe` adapters for SOF provider-stream ingress.
//!
//! This adapter keeps the same transaction semantics as Yellowstone and
//! LaserStream by requesting full base64 transaction payloads and converting
//! them into [`crate::framework::TransactionEvent`] values before dispatch.

use std::{borrow::Cow, str::FromStr, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use simd_json::serde::from_slice as simd_from_slice;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use thiserror::Error;
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message as WsMessage};

use crate::{
    event::TxCommitmentStatus,
    framework::TransactionEvent,
    provider_stream::{
        ProviderCommitmentWatermarks, ProviderStreamSender, ProviderStreamUpdate,
        SerializedTransactionEvent, classify_provider_transaction_kind,
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
    replay_max_slots: u64,
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
            replay_max_slots: 128,
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
        self
    }

    /// Sets the maximum reconnect backfill window in slots.
    #[must_use]
    pub const fn with_replay_max_slots(mut self, slots: u64) -> Self {
        self.replay_max_slots = slots;
        self
    }

    pub(crate) fn subscribe_request(&self) -> Value {
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
}

/// Websocket `transactionSubscribe` error surface.
#[derive(Debug, Error)]
pub enum WebsocketTransactionError {
    /// Websocket transport failure.
    #[error(transparent)]
    Transport(#[from] tokio_tungstenite::tungstenite::Error),
    /// Upstream payload shape/protocol failure.
    #[error("websocket transaction protocol error: {0}")]
    Protocol(String),
    /// Provider payload could not be converted into a SOF transaction event.
    #[error("websocket transaction conversion failed: {0}")]
    Convert(&'static str),
    /// Provider-stream queue is closed.
    #[error("provider-stream queue closed")]
    QueueClosed,
}

/// Spawns one websocket `transactionSubscribe` source into a SOF provider-stream queue.
///
/// # Examples
///
/// ```no_run
/// use sof::provider_stream::{
///     create_provider_stream_queue,
///     websocket::{spawn_websocket_transaction_source, WebsocketTransactionConfig},
/// };
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let (tx, _rx) = create_provider_stream_queue(1024);
/// let config = WebsocketTransactionConfig::new("wss://mainnet.helius-rpc.com/?api-key=example");
/// let handle = spawn_websocket_transaction_source(&config, tx);
/// handle.abort();
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn spawn_websocket_transaction_source(
    config: &WebsocketTransactionConfig,
    sender: ProviderStreamSender,
) -> JoinHandle<Result<(), WebsocketTransactionError>> {
    let config = config.clone();
    tokio::spawn(async move {
        let mut attempts = 0_u32;
        let mut last_seen_slot = None;
        let mut watermarks = ProviderCommitmentWatermarks::default();
        loop {
            let mut session_established = false;
            match run_websocket_transaction_connection(
                &config,
                &sender,
                &mut last_seen_slot,
                &mut watermarks,
                &mut session_established,
            )
            .await
            {
                Ok(()) => {
                    let error = WebsocketTransactionError::Protocol(
                        "websocket transaction stream ended unexpectedly".to_owned(),
                    );
                    tracing::warn!(%error, endpoint = config.endpoint(), "provider stream websocket session ended; reconnecting");
                }
                Err(WebsocketTransactionError::QueueClosed) => {
                    return Err(WebsocketTransactionError::QueueClosed);
                }
                Err(error) => {
                    tracing::warn!(%error, endpoint = config.endpoint(), "provider stream websocket session ended; reconnecting");
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
                return Err(WebsocketTransactionError::Protocol(format!(
                    "exhausted websocket reconnect attempts after {attempts} failures"
                )));
            }
            tokio::time::sleep(config.reconnect_delay).await;
        }
    })
}

async fn run_websocket_transaction_connection(
    config: &WebsocketTransactionConfig,
    sender: &ProviderStreamSender,
    last_seen_slot: &mut Option<u64>,
    watermarks: &mut ProviderCommitmentWatermarks,
    session_established: &mut bool,
) -> Result<(), WebsocketTransactionError> {
    *session_established = false;
    let (stream, _response) = connect_async(config.endpoint()).await?;
    let (mut write, mut read) = stream.split();

    write
        .send(WsMessage::Text(
            config.subscribe_request().to_string().into(),
        ))
        .await?;

    wait_for_subscription_ack(&mut read).await?;
    *session_established = true;
    if config.replay_on_reconnect && last_seen_slot.is_some() {
        replay_websocket_gap(config, sender, last_seen_slot, watermarks).await?;
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
                    tokio::time::sleep_until(last_progress + timeout).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                return Err(WebsocketTransactionError::Protocol(
                    "websocket transaction stream stalled without inbound progress".to_owned(),
                ));
            }
            maybe_frame = read.next() => {
                let Some(frame) = maybe_frame else {
                    return Ok(());
                };
                let frame = frame?;
                last_progress = tokio::time::Instant::now();
                match frame {
                    WsMessage::Text(text) => {
                        if let Some(update) =
                            parse_transaction_notification(
                                frame_bytes_mut(&mut scratch.frame_bytes, text.as_str().as_bytes()),
                                &mut scratch.tx_bytes,
                                config.commitment,
                                watermarks,
                            )?
                        {
                            *last_seen_slot =
                                Some((*last_seen_slot).unwrap_or(update.slot).max(update.slot));
                            sender
                                .send(ProviderStreamUpdate::SerializedTransaction(update))
                                .await
                                .map_err(|_error| WebsocketTransactionError::QueueClosed)?;
                        }
                    }
                    WsMessage::Binary(bytes) => {
                        if let Some(update) =
                            parse_transaction_notification(
                                frame_bytes_mut(&mut scratch.frame_bytes, bytes.as_ref()),
                                &mut scratch.tx_bytes,
                                config.commitment,
                                watermarks,
                            )?
                        {
                            *last_seen_slot =
                                Some((*last_seen_slot).unwrap_or(update.slot).max(update.slot));
                            sender
                                .send(ProviderStreamUpdate::SerializedTransaction(update))
                                .await
                                .map_err(|_error| WebsocketTransactionError::QueueClosed)?;
                        }
                    }
                    WsMessage::Ping(payload) => {
                        write.send(WsMessage::Pong(payload)).await?;
                    }
                    WsMessage::Pong(_) => {}
                    WsMessage::Close(frame) => {
                        return Err(WebsocketTransactionError::Protocol(format!(
                            "websocket closed: {frame:?}"
                        )));
                    }
                    _ => {}
                }
            }
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
                    "websocket closed before subscription ack".to_owned(),
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
                    return Err(WebsocketTransactionError::Protocol(format!(
                        "websocket closed before subscription ack: {frame:?}"
                    )));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_elapsed| {
        WebsocketTransactionError::Protocol(
            "timed out waiting for websocket subscription ack".to_owned(),
        )
    })?
}

fn handle_subscription_text(bytes: &mut [u8]) -> Result<bool, WebsocketTransactionError> {
    let value: WebsocketSubscriptionAck = simd_from_slice(bytes).map_err(|error| {
        WebsocketTransactionError::Protocol(format!("invalid websocket json: {error}"))
    })?;
    if let Some(error) = value.error {
        return Err(WebsocketTransactionError::Protocol(format!(
            "websocket subscription error: {error}"
        )));
    }
    Ok(value.id == Some(1) && value.result.is_some())
}

fn parse_transaction_notification(
    bytes: &mut [u8],
    tx_bytes: &mut Vec<u8>,
    commitment_status: WebsocketTransactionCommitment,
    watermarks: &mut ProviderCommitmentWatermarks,
) -> Result<Option<SerializedTransactionEvent>, WebsocketTransactionError> {
    let value: WebsocketTransactionEnvelopeMessage = simd_from_slice(bytes).map_err(|error| {
        WebsocketTransactionError::Protocol(format!("invalid websocket json: {error}"))
    })?;
    if let Some(error) = value.error {
        return Err(WebsocketTransactionError::Protocol(format!(
            "websocket provider error: {error}"
        )));
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
    let signature = notification
        .signature
        .and_then(|signature| Signature::from_str(&signature).ok());
    watermarks
        .observe_transaction_commitment(notification.slot, commitment_status.as_tx_commitment());
    let tx_payload = std::mem::take(tx_bytes).into_boxed_slice();
    Ok(Some(SerializedTransactionEvent {
        slot: notification.slot,
        commitment_status: commitment_status.as_tx_commitment(),
        confirmed_slot: watermarks.confirmed_slot,
        finalized_slot: watermarks.finalized_slot,
        signature,
        bytes: tx_payload,
    }))
}

fn materialize_transaction_baseline(
    bytes: &mut [u8],
    commitment_status: WebsocketTransactionCommitment,
) -> Result<Option<TransactionEvent>, WebsocketTransactionError> {
    let value: WebsocketTransactionEnvelopeMessage = simd_from_slice(bytes).map_err(|error| {
        WebsocketTransactionError::Protocol(format!("invalid websocket json: {error}"))
    })?;
    if let Some(error) = value.error {
        return Err(WebsocketTransactionError::Protocol(format!(
            "websocket provider error: {error}"
        )));
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
        signature,
        kind: classify_provider_transaction_kind(&tx),
        tx: Arc::new(tx),
    }))
}

#[derive(Default)]
struct WebsocketParseScratch {
    frame_bytes: Vec<u8>,
    tx_bytes: Vec<u8>,
}

fn frame_bytes_mut<'a>(buffer: &'a mut Vec<u8>, bytes: &[u8]) -> &'a mut [u8] {
    buffer.clear();
    buffer.extend_from_slice(bytes);
    buffer.as_mut_slice()
}

async fn replay_websocket_gap(
    config: &WebsocketTransactionConfig,
    sender: &ProviderStreamSender,
    last_seen_slot: &mut Option<u64>,
    watermarks: &mut ProviderCommitmentWatermarks,
) -> Result<(), WebsocketTransactionError> {
    let Some(previous_slot) = *last_seen_slot else {
        return Ok(());
    };
    let Some(http_endpoint) = websocket_http_endpoint(config) else {
        tracing::warn!(
            endpoint = config.endpoint(),
            "websocket reconnect backfill is enabled but no HTTP RPC endpoint is available"
        );
        return Ok(());
    };

    let client = reqwest::Client::new();
    let head = rpc_get_slot(&client, &http_endpoint, config.commitment).await?;
    if head <= previous_slot {
        return Ok(());
    }

    let start_slot = previous_slot.saturating_add(1).max(
        head.saturating_add(1)
            .saturating_sub(config.replay_max_slots.max(1)),
    );
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
                .send(ProviderStreamUpdate::Transaction(TransactionEvent {
                    slot,
                    commitment_status: config.commitment.as_tx_commitment(),
                    confirmed_slot: watermarks.confirmed_slot,
                    finalized_slot: watermarks.finalized_slot,
                    signature: tx.signatures.first().copied(),
                    kind,
                    tx: Arc::new(tx),
                }))
                .await
                .map_err(|_error| WebsocketTransactionError::QueueClosed)?;
            *last_seen_slot = Some((*last_seen_slot).unwrap_or(slot).max(slot));
        }
    }
    Ok(())
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
        .map_err(|error| {
            WebsocketTransactionError::Protocol(format!("http rpc getSlot failed: {error}"))
        })?
        .json()
        .await
        .map_err(|error| {
            WebsocketTransactionError::Protocol(format!("http rpc getSlot decode failed: {error}"))
        })?;
    if let Some(error) = response.error {
        return Err(WebsocketTransactionError::Protocol(format!(
            "http rpc getSlot returned error: {error}"
        )));
    }
    response.result.ok_or_else(|| {
        WebsocketTransactionError::Protocol("http rpc getSlot returned no result".to_owned())
    })
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
        .map_err(|error| {
            WebsocketTransactionError::Protocol(format!("http rpc getBlock failed: {error}"))
        })?
        .json()
        .await
        .map_err(|error| {
            WebsocketTransactionError::Protocol(format!("http rpc getBlock decode failed: {error}"))
        })?;
    if let Some(error) = response.error {
        return Err(WebsocketTransactionError::Protocol(format!(
            "http rpc getBlock returned error: {error}"
        )));
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
struct WebsocketTransactionEnvelopeMessage<'a> {
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    #[serde(borrow)]
    params: Option<WebsocketTransactionParams<'a>>,
}

#[derive(Debug, Deserialize)]
struct WebsocketTransactionParams<'a> {
    #[serde(borrow)]
    result: WebsocketTransactionNotification<'a>,
}

#[derive(Debug, Deserialize)]
struct WebsocketTransactionNotification<'a> {
    slot: u64,
    #[serde(default)]
    #[serde(borrow)]
    signature: Option<Cow<'a, str>>,
    #[serde(borrow)]
    transaction: WebsocketTransactionEnvelope<'a>,
}

#[derive(Debug, Deserialize)]
struct WebsocketTransactionEnvelope<'a> {
    #[serde(borrow)]
    transaction: WebsocketEncodedTransaction<'a>,
}

#[derive(Debug, Deserialize)]
struct WebsocketEncodedTransaction<'a>(
    #[serde(borrow)] Cow<'a, str>,
    #[serde(borrow)] Cow<'a, str>,
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
mod tests {
    use super::*;
    use crate::event::TxKind;
    use crate::provider_stream::create_provider_stream_queue;
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
        let mut tx_bytes = Vec::new();
        let mut watermarks = ProviderCommitmentWatermarks::default();
        let event = parse_transaction_notification(
            &mut frame_bytes,
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
                    other => panic!("expected subscribe text frame, got {other:?}"),
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
        let handle = spawn_websocket_transaction_source(&config, tx);

        let update = timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("provider update timeout")
            .expect("provider update");
        match update {
            ProviderStreamUpdate::SerializedTransaction(event) => {
                assert_eq!(event.slot, 55);
                assert!(event.signature.is_some());
                assert!(!event.bytes.is_empty());
            }
            other => panic!("expected transaction update, got {other:?}"),
        }

        handle.abort();
        let _ = handle.await;
        server.await.expect("server task");
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
        let mut tx_bytes = Vec::new();
        let mut watermarks = ProviderCommitmentWatermarks::default();
        for _ in 0..iterations {
            let frame = frame_bytes_mut(&mut frame_bytes, &payload);
            let event = parse_transaction_notification(
                frame,
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
        let mut tx_bytes = Vec::new();
        let mut watermarks = ProviderCommitmentWatermarks::default();
        for _ in 0..iterations {
            let frame = frame_bytes_mut(&mut frame_bytes, &payload);
            let event = parse_transaction_notification(
                frame,
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
