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
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use thiserror::Error;
use tokio::task::JoinHandle;

use crate::{
    event::{TxCommitmentStatus, TxKind},
    framework::TransactionEvent,
    provider_stream::{
        ProviderStreamSender, ProviderStreamUpdate, classify_provider_transaction_kind,
    },
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
    let is_vote = transaction.is_vote;
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
        signature: tx.signatures.first().copied(),
        kind: if is_vote {
            TxKind::VoteOnly
        } else {
            classify_provider_transaction_kind(&tx)
        },
        tx: Arc::new(tx),
    })
}

fn convert_transaction(
    tx: LaserStreamTransaction,
) -> Result<VersionedTransaction, LaserStreamError> {
    create_tx_versioned(tx).map_err(LaserStreamError::Convert)
}

#[cfg(all(test, feature = "provider-grpc"))]
mod tests {
    use super::*;
    use crate::provider_stream::yellowstone::{YellowstoneGrpcCommitment, YellowstoneGrpcConfig};
    use laserstream_core_proto::prelude::{
        CompiledInstruction as ProtoCompiledInstruction, Message as ProtoMessage,
        MessageAddressTableLookup as ProtoMessageAddressTableLookup,
        MessageHeader as ProtoMessageHeader, SubscribeUpdateTransactionInfo,
        Transaction as ProtoTransaction,
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

    fn proto_transaction_from_versioned(tx: &VersionedTransaction) -> ProtoTransaction {
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
        ProtoTransaction {
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

    #[test]
    fn laserstream_transaction_event_from_update_decodes_transaction() {
        let event =
            transaction_event_from_update(77, Some(sample_update()), TxCommitmentStatus::Confirmed)
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
