#![allow(clippy::missing_docs_in_private_items)]

//! Websocket `transactionSubscribe` adapters for SOF provider-stream ingress.
//!
//! This adapter keeps the same transaction semantics as Yellowstone and
//! LaserStream by requesting full base64 transaction payloads and converting
//! them into [`crate::framework::TransactionEvent`] values before dispatch.

use std::{str::FromStr, sync::Arc, time::Duration};

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
        ProviderStreamSender, ProviderStreamUpdate, classify_provider_transaction_kind,
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
    commitment: WebsocketTransactionCommitment,
    vote: Option<bool>,
    failed: Option<bool>,
    signature: Option<Signature>,
    account_include: Vec<Pubkey>,
    account_exclude: Vec<Pubkey>,
    account_required: Vec<Pubkey>,
    ping_interval: Option<Duration>,
    reconnect_delay: Duration,
    max_reconnect_attempts: Option<u32>,
}

impl WebsocketTransactionConfig {
    /// Creates a websocket transaction-stream config for one endpoint.
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
            commitment: WebsocketTransactionCommitment::Processed,
            vote: Some(false),
            failed: Some(false),
            signature: None,
            account_include: Vec::new(),
            account_exclude: Vec::new(),
            account_required: Vec::new(),
            ping_interval: Some(Duration::from_secs(60)),
            reconnect_delay: Duration::from_secs(1),
            max_reconnect_attempts: None,
        }
    }

    /// Returns the configured websocket endpoint.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
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
        loop {
            match run_websocket_transaction_connection(&config, &sender).await {
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
            attempts = attempts.saturating_add(1);
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
) -> Result<(), WebsocketTransactionError> {
    let (stream, _response) = connect_async(config.endpoint()).await?;
    let (mut write, mut read) = stream.split();

    write
        .send(WsMessage::Text(
            config.subscribe_request().to_string().into(),
        ))
        .await?;

    wait_for_subscription_ack(&mut read).await?;
    let mut ping = config.ping_interval.map(tokio::time::interval);

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
            maybe_frame = read.next() => {
                let Some(frame) = maybe_frame else {
                    return Ok(());
                };
                let frame = frame?;
                match frame {
                    WsMessage::Text(text) => {
                        let mut bytes = text.as_str().as_bytes().to_vec();
                        if let Some(event) =
                            parse_transaction_notification(&mut bytes, config.commitment)?
                        {
                            sender
                                .send(ProviderStreamUpdate::Transaction(event))
                                .await
                                .map_err(|_error| WebsocketTransactionError::QueueClosed)?;
                        }
                    }
                    WsMessage::Binary(bytes) => {
                        let mut bytes = bytes.to_vec();
                        if let Some(event) =
                            parse_transaction_notification(&mut bytes, config.commitment)?
                        {
                            sender
                                .send(ProviderStreamUpdate::Transaction(event))
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
                    let mut bytes = text.as_str().as_bytes().to_vec();
                    if handle_subscription_text(&mut bytes)? {
                        return Ok(());
                    }
                }
                WsMessage::Binary(bytes) => {
                    let mut bytes = bytes.to_vec();
                    if handle_subscription_text(&mut bytes)? {
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
        .decode(notification.transaction.transaction.0)
        .map_err(|_error| {
            WebsocketTransactionError::Convert("invalid base64 transaction payload")
        })?;
    let tx = bincode::deserialize::<VersionedTransaction>(&tx_bytes).map_err(|_error| {
        WebsocketTransactionError::Convert("failed to deserialize transaction")
    })?;
    let signature = notification
        .signature
        .map(|signature| Signature::from_str(&signature))
        .transpose()
        .map_err(|_error| WebsocketTransactionError::Convert("invalid signature"))?
        .or_else(|| tx.signatures.first().copied());
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
struct WebsocketTransactionEnvelopeMessage {
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    params: Option<WebsocketTransactionParams>,
}

#[derive(Debug, Deserialize)]
struct WebsocketTransactionParams {
    result: WebsocketTransactionNotification,
}

#[derive(Debug, Deserialize)]
struct WebsocketTransactionNotification {
    slot: u64,
    #[serde(default)]
    signature: Option<String>,
    transaction: WebsocketTransactionEnvelope,
}

#[derive(Debug, Deserialize)]
struct WebsocketTransactionEnvelope {
    transaction: WebsocketEncodedTransaction,
}

#[derive(Debug, Deserialize)]
struct WebsocketEncodedTransaction(String, String);

#[cfg(all(test, feature = "provider-grpc"))]
mod tests {
    use super::*;
    use crate::event::TxKind;
    use crate::provider_stream::yellowstone::{YellowstoneGrpcCommitment, YellowstoneGrpcConfig};
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use serde_json::json;
    use solana_keypair::Keypair;
    use solana_message::{Message, VersionedMessage};
    use solana_signer::Signer;

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
    fn websocket_transaction_notification_decodes_full_transaction() {
        let signer = Keypair::new();
        let message = Message::new(&[], Some(&signer.pubkey()));
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(message), &[&signer])
            .expect("tx");
        let signature = tx.signatures[0];
        let tx_bytes = bincode::serialize(&tx).expect("serialize tx");
        let payload = json!({
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
        });

        let mut payload = payload.to_string().into_bytes();
        let event =
            parse_transaction_notification(&mut payload, WebsocketTransactionCommitment::Confirmed)
                .expect("notification should parse")
                .expect("transaction event");

        assert_eq!(event.slot, 55);
        assert_eq!(event.signature, Some(signature));
        assert_eq!(event.commitment_status, TxCommitmentStatus::Confirmed);
        assert_eq!(event.kind, TxKind::NonVote);
    }
}
