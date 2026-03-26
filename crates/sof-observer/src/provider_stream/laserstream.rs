#![allow(clippy::missing_docs_in_private_items)]

//! Helius LaserStream adapters for SOF processed provider-stream ingress.
//!
//! LaserStream uses the Yellowstone-compatible gRPC subscription model. SOF's
//! built-in adapter intentionally stays on the shared transaction subscription
//! surface: commitment, signature, vote/failed flags, and account
//! include/exclude/required filters.

use std::{collections::HashMap, sync::Arc, time::Duration};

use futures_util::StreamExt;
use helius_laserstream::{
    ChannelOptions, LaserstreamConfig as ClientConfig, LaserstreamError as ClientError, grpc,
    subscribe,
};
use laserstream_core_proto::convert_from::create_tx_versioned;
use laserstream_core_proto::prelude::Transaction as LaserStreamTransaction;
use solana_pubkey::Pubkey;
use solana_sdk_ids::{compute_budget, vote};
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use thiserror::Error;
use tokio::task::JoinHandle;

use crate::{
    event::{TxCommitmentStatus, TxKind},
    framework::TransactionEvent,
    provider_stream::{ProviderStreamSender, ProviderStreamUpdate},
};

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
    max_decoding_message_size: Option<usize>,
    max_encoding_message_size: Option<usize>,
    max_reconnect_attempts: Option<u32>,
    replay: bool,
}

impl LaserStreamConfig {
    /// Creates a transaction-stream config for one LaserStream endpoint.
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
            vote: Some(false),
            failed: Some(false),
            signature: None,
            account_include: Vec::new(),
            account_exclude: Vec::new(),
            account_required: Vec::new(),
            connect_timeout: Some(Duration::from_secs(10)),
            timeout: Some(Duration::from_secs(30)),
            max_decoding_message_size: Some(64 * 1024 * 1024),
            max_encoding_message_size: None,
            max_reconnect_attempts: None,
            replay: true,
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

    /// Sets replay behavior on reconnects.
    #[must_use]
    pub const fn with_replay(mut self, replay: bool) -> Self {
        self.replay = replay;
        self
    }

    pub(crate) fn subscribe_request(&self) -> grpc::SubscribeRequest {
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
            transactions: HashMap::from([("sof".to_owned(), filter)]),
            commitment: Some(self.commitment.as_proto() as i32),
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
            .with_replay(self.replay);
        if let Some(attempts) = self.max_reconnect_attempts {
            config = config.with_max_reconnect_attempts(attempts);
        }
        config
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
    /// Provider-stream queue is closed.
    #[error("provider-stream queue closed")]
    QueueClosed,
}

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
/// let handle = spawn_laserstream_transaction_source(
///     &config,
///     tx,
/// );
/// handle.abort();
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn spawn_laserstream_transaction_source(
    config: &LaserStreamConfig,
    sender: ProviderStreamSender,
) -> JoinHandle<Result<(), LaserStreamError>> {
    let commitment = config.commitment.as_tx_commitment();
    let request = config.subscribe_request();
    let (stream, _handle) = subscribe(config.client_config(), request);
    tokio::spawn(async move {
        tokio::pin!(stream);
        while let Some(update) = stream.next().await {
            let update = update?;
            if let Some(grpc::subscribe_update::UpdateOneof::Transaction(tx_update)) =
                update.update_oneof
            {
                let event = transaction_event_from_update(
                    tx_update.slot,
                    tx_update.transaction,
                    commitment,
                )?;
                sender
                    .send(ProviderStreamUpdate::Transaction(event))
                    .await
                    .map_err(|_error| LaserStreamError::QueueClosed)?;
            }
        }
        Ok(())
    })
}

fn transaction_event_from_update(
    slot: u64,
    transaction: Option<grpc::SubscribeUpdateTransactionInfo>,
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
        kind: classify_tx_kind(&tx),
        tx: Arc::new(tx),
    })
}

fn convert_transaction(
    tx: LaserStreamTransaction,
) -> Result<VersionedTransaction, LaserStreamError> {
    create_tx_versioned(tx).map_err(LaserStreamError::Convert)
}

fn classify_tx_kind(tx: &VersionedTransaction) -> TxKind {
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

#[cfg(all(test, feature = "provider-grpc"))]
mod tests {
    use super::*;
    use crate::provider_stream::yellowstone::{YellowstoneGrpcCommitment, YellowstoneGrpcConfig};

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
}
