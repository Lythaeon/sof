//! Transaction submission client and mode orchestration.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use solana_signature::Signature;
use solana_signer::signers::Signers;
use solana_transaction::versioned::VersionedTransaction;
use thiserror::Error;
use tokio::{net::UdpSocket, time::timeout};

use crate::{
    builder::{BuilderError, TxBuilder},
    providers::{LeaderProvider, LeaderTarget, RecentBlockhashProvider},
    routing::{RoutingPolicy, SignatureDeduper, select_targets},
};

/// Runtime submit mode.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SubmitMode {
    /// Submit only through JSON-RPC.
    RpcOnly,
    /// Submit only through direct leader/validator targets.
    DirectOnly,
    /// Submit direct first, then RPC fallback on failure.
    Hybrid,
}

/// Signed transaction payload variants accepted by submit APIs.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SignedTx {
    /// Bincode-serialized `VersionedTransaction` bytes.
    VersionedTransactionBytes(Vec<u8>),
    /// Wire-format transaction bytes.
    WireTransactionBytes(Vec<u8>),
}

/// RPC submit tuning.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RpcSubmitConfig {
    /// Skip preflight simulation when true.
    pub skip_preflight: bool,
    /// Optional preflight commitment string.
    pub preflight_commitment: Option<String>,
}

impl Default for RpcSubmitConfig {
    fn default() -> Self {
        Self {
            skip_preflight: true,
            preflight_commitment: None,
        }
    }
}

/// Direct submit tuning.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DirectSubmitConfig {
    /// Per-target send timeout.
    pub per_target_timeout: Duration,
    /// Global send budget for one submission.
    pub global_timeout: Duration,
}

impl Default for DirectSubmitConfig {
    fn default() -> Self {
        Self {
            per_target_timeout: Duration::from_millis(300),
            global_timeout: Duration::from_millis(1_200),
        }
    }
}

/// Low-level transport errors surfaced by submit backends.
#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum SubmitTransportError {
    /// Invalid transport configuration.
    #[error("transport configuration invalid: {message}")]
    Config {
        /// Human-readable description.
        message: String,
    },
    /// Transport operation failed.
    #[error("transport failure: {message}")]
    Failure {
        /// Human-readable description.
        message: String,
    },
}

/// Submission-level errors.
#[derive(Debug, Error)]
pub enum SubmitError {
    /// Could not build/sign transaction for builder submit path.
    #[error("failed to build/sign transaction: {source}")]
    Build {
        /// Builder-layer failure.
        source: BuilderError,
    },
    /// No blockhash available for builder submit path.
    #[error("blockhash provider returned no recent blockhash")]
    MissingRecentBlockhash,
    /// Signed bytes could not be decoded into a transaction.
    #[error("failed to decode signed transaction bytes: {source}")]
    DecodeSignedBytes {
        /// Bincode decode error.
        source: Box<bincode::ErrorKind>,
    },
    /// Duplicate signature was suppressed by dedupe window.
    #[error("duplicate signature suppressed by dedupe window")]
    DuplicateSignature,
    /// RPC mode requested but no RPC transport was configured.
    #[error("rpc transport is not configured")]
    MissingRpcTransport,
    /// Direct mode requested but no direct transport was configured.
    #[error("direct transport is not configured")]
    MissingDirectTransport,
    /// No direct targets resolved from routing inputs.
    #[error("no direct targets resolved from leader/backups")]
    NoDirectTargets,
    /// Direct transport failure.
    #[error("direct submit failed: {source}")]
    Direct {
        /// Direct transport error.
        source: SubmitTransportError,
    },
    /// RPC transport failure.
    #[error("rpc submit failed: {source}")]
    Rpc {
        /// RPC transport error.
        source: SubmitTransportError,
    },
    /// Internal synchronization failure.
    #[error("internal synchronization failure: {message}")]
    InternalSync {
        /// Synchronization error details.
        message: String,
    },
}

/// Summary of a successful submission.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SubmitResult {
    /// Signature parsed from submitted transaction bytes.
    pub signature: Option<Signature>,
    /// Mode selected by caller.
    pub mode: SubmitMode,
    /// Target chosen by direct path when applicable.
    pub direct_target: Option<LeaderTarget>,
    /// RPC-returned signature string when RPC path succeeded.
    pub rpc_signature: Option<String>,
    /// True when RPC fallback was used from hybrid mode.
    pub used_rpc_fallback: bool,
}

/// RPC transport interface.
#[async_trait]
pub trait RpcSubmitTransport: Send + Sync {
    /// Submits transaction bytes to RPC and returns signature string.
    async fn submit_rpc(
        &self,
        tx_bytes: &[u8],
        config: &RpcSubmitConfig,
    ) -> Result<String, SubmitTransportError>;
}

/// Direct transport interface.
#[async_trait]
pub trait DirectSubmitTransport: Send + Sync {
    /// Submits transaction bytes to direct targets and returns the first successful target.
    async fn submit_direct(
        &self,
        tx_bytes: &[u8],
        targets: &[LeaderTarget],
        policy: RoutingPolicy,
        config: &DirectSubmitConfig,
    ) -> Result<LeaderTarget, SubmitTransportError>;
}

/// JSON-RPC transport implementation for `sendTransaction`.
#[derive(Debug, Clone)]
pub struct JsonRpcTransport {
    /// HTTP client used for RPC calls.
    client: reqwest::Client,
    /// Target JSON-RPC endpoint URL.
    rpc_url: String,
}

impl JsonRpcTransport {
    /// Creates a JSON-RPC transport.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError::Config`] when HTTP client creation fails.
    pub fn new(rpc_url: impl Into<String>) -> Result<Self, SubmitTransportError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| SubmitTransportError::Config {
                message: error.to_string(),
            })?;
        Ok(Self {
            client,
            rpc_url: rpc_url.into(),
        })
    }
}

/// JSON-RPC envelope.
#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    /// Result value for successful calls.
    result: Option<String>,
    /// Error payload for failed calls.
    error: Option<JsonRpcError>,
}

/// JSON-RPC error object.
#[derive(Debug, Deserialize)]
struct JsonRpcError {
    /// JSON-RPC error code.
    code: i64,
    /// Human-readable message.
    message: String,
}

#[async_trait]
impl RpcSubmitTransport for JsonRpcTransport {
    async fn submit_rpc(
        &self,
        tx_bytes: &[u8],
        config: &RpcSubmitConfig,
    ) -> Result<String, SubmitTransportError> {
        #[derive(Debug, Serialize)]
        struct RpcConfig<'config> {
            /// Transaction encoding format.
            encoding: &'config str,
            /// Optional preflight skip flag.
            #[serde(rename = "skipPreflight")]
            skip_preflight: bool,
            /// Optional preflight commitment.
            #[serde(
                rename = "preflightCommitment",
                skip_serializing_if = "Option::is_none"
            )]
            preflight_commitment: Option<&'config str>,
        }

        let encoded_tx = BASE64_STANDARD.encode(tx_bytes);
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendTransaction",
            "params": [
                encoded_tx,
                RpcConfig {
                    encoding: "base64",
                    skip_preflight: config.skip_preflight,
                    preflight_commitment: config.preflight_commitment.as_deref(),
                }
            ]
        });

        let response = self
            .client
            .post(&self.rpc_url)
            .json(&payload)
            .send()
            .await
            .map_err(|error| SubmitTransportError::Failure {
                message: error.to_string(),
            })?;

        let response =
            response
                .error_for_status()
                .map_err(|error| SubmitTransportError::Failure {
                    message: error.to_string(),
                })?;

        let parsed: JsonRpcResponse =
            response
                .json()
                .await
                .map_err(|error| SubmitTransportError::Failure {
                    message: error.to_string(),
                })?;

        if let Some(signature) = parsed.result {
            return Ok(signature);
        }
        if let Some(error) = parsed.error {
            return Err(SubmitTransportError::Failure {
                message: format!("rpc error {}: {}", error.code, error.message),
            });
        }

        Err(SubmitTransportError::Failure {
            message: "rpc returned neither result nor error".to_owned(),
        })
    }
}

/// UDP direct transport implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct UdpDirectTransport;

#[async_trait]
impl DirectSubmitTransport for UdpDirectTransport {
    async fn submit_direct(
        &self,
        tx_bytes: &[u8],
        targets: &[LeaderTarget],
        policy: RoutingPolicy,
        config: &DirectSubmitConfig,
    ) -> Result<LeaderTarget, SubmitTransportError> {
        if targets.is_empty() {
            return Err(SubmitTransportError::Config {
                message: "no targets provided".to_owned(),
            });
        }

        let socket =
            UdpSocket::bind("0.0.0.0:0")
                .await
                .map_err(|error| SubmitTransportError::Failure {
                    message: error.to_string(),
                })?;

        let deadline = Instant::now()
            .checked_add(config.global_timeout)
            .ok_or_else(|| SubmitTransportError::Failure {
                message: "failed to calculate direct-submit deadline".to_owned(),
            })?;
        for chunk in targets.chunks(policy.normalized().max_parallel_sends) {
            for target in chunk {
                let now = Instant::now();
                if now >= deadline {
                    return Err(SubmitTransportError::Failure {
                        message: "global direct-submit timeout exceeded".to_owned(),
                    });
                }
                let remaining = deadline.saturating_duration_since(now);
                let per_target = remaining.min(config.per_target_timeout);
                let send_result =
                    timeout(per_target, socket.send_to(tx_bytes, target.tpu_addr)).await;
                match send_result {
                    Ok(Ok(_bytes_sent)) => return Ok(target.clone()),
                    Ok(Err(_send_error)) => {}
                    Err(_elapsed) => {}
                }
            }
        }

        Err(SubmitTransportError::Failure {
            message: "all direct targets failed".to_owned(),
        })
    }
}

/// Submission client supporting rpc/direct/hybrid modes.
pub struct TxSubmitClient {
    /// Blockhash source used by builder submit path.
    blockhash_provider: Arc<dyn RecentBlockhashProvider>,
    /// Leader source used by direct/hybrid paths.
    leader_provider: Arc<dyn LeaderProvider>,
    /// Optional backup validator targets.
    backups: Vec<LeaderTarget>,
    /// Direct routing policy.
    policy: RoutingPolicy,
    /// Signature dedupe window.
    deduper: Mutex<SignatureDeduper>,
    /// Optional RPC transport.
    rpc_transport: Option<Arc<dyn RpcSubmitTransport>>,
    /// Optional direct transport.
    direct_transport: Option<Arc<dyn DirectSubmitTransport>>,
    /// RPC tuning.
    rpc_config: RpcSubmitConfig,
    /// Direct tuning.
    direct_config: DirectSubmitConfig,
}

impl TxSubmitClient {
    /// Creates a submission client with no transports preconfigured.
    #[must_use]
    pub fn new(
        blockhash_provider: Arc<dyn RecentBlockhashProvider>,
        leader_provider: Arc<dyn LeaderProvider>,
    ) -> Self {
        Self {
            blockhash_provider,
            leader_provider,
            backups: Vec::new(),
            policy: RoutingPolicy::default(),
            deduper: Mutex::new(SignatureDeduper::new(Duration::from_secs(10))),
            rpc_transport: None,
            direct_transport: None,
            rpc_config: RpcSubmitConfig::default(),
            direct_config: DirectSubmitConfig::default(),
        }
    }

    /// Sets optional backup validators.
    #[must_use]
    pub fn with_backups(mut self, backups: Vec<LeaderTarget>) -> Self {
        self.backups = backups;
        self
    }

    /// Sets routing policy.
    #[must_use]
    pub fn with_routing_policy(mut self, policy: RoutingPolicy) -> Self {
        self.policy = policy.normalized();
        self
    }

    /// Sets dedupe TTL.
    #[must_use]
    pub fn with_dedupe_ttl(mut self, ttl: Duration) -> Self {
        self.deduper = Mutex::new(SignatureDeduper::new(ttl));
        self
    }

    /// Sets RPC transport.
    #[must_use]
    pub fn with_rpc_transport(mut self, transport: Arc<dyn RpcSubmitTransport>) -> Self {
        self.rpc_transport = Some(transport);
        self
    }

    /// Sets direct transport.
    #[must_use]
    pub fn with_direct_transport(mut self, transport: Arc<dyn DirectSubmitTransport>) -> Self {
        self.direct_transport = Some(transport);
        self
    }

    /// Sets RPC submit tuning.
    #[must_use]
    pub fn with_rpc_config(mut self, config: RpcSubmitConfig) -> Self {
        self.rpc_config = config;
        self
    }

    /// Sets direct submit tuning.
    #[must_use]
    pub const fn with_direct_config(mut self, config: DirectSubmitConfig) -> Self {
        self.direct_config = config;
        self
    }

    /// Builds, signs, and submits a transaction in one API call.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitError`] when blockhash lookup, signing, dedupe, routing, or submission
    /// fails.
    pub async fn submit_builder<T>(
        &self,
        builder: TxBuilder,
        signers: &T,
        mode: SubmitMode,
    ) -> Result<SubmitResult, SubmitError>
    where
        T: Signers + ?Sized,
    {
        let blockhash = self
            .blockhash_provider
            .latest_blockhash()
            .ok_or(SubmitError::MissingRecentBlockhash)?;
        let tx = builder
            .build_and_sign(blockhash, signers)
            .map_err(|source| SubmitError::Build { source })?;
        self.submit_transaction(tx, mode).await
    }

    /// Submits one signed `VersionedTransaction`.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitError`] when encoding, dedupe, routing, or submission fails.
    pub async fn submit_transaction(
        &self,
        tx: VersionedTransaction,
        mode: SubmitMode,
    ) -> Result<SubmitResult, SubmitError> {
        let signature = tx.signatures.first().copied();
        let tx_bytes =
            bincode::serialize(&tx).map_err(|source| SubmitError::DecodeSignedBytes { source })?;
        self.submit_bytes(tx_bytes, signature, mode).await
    }

    /// Submits externally signed transaction bytes.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitError`] when decoding, dedupe, routing, or submission fails.
    pub async fn submit_signed(
        &self,
        signed_tx: SignedTx,
        mode: SubmitMode,
    ) -> Result<SubmitResult, SubmitError> {
        let tx_bytes = match signed_tx {
            SignedTx::VersionedTransactionBytes(bytes) => bytes,
            SignedTx::WireTransactionBytes(bytes) => bytes,
        };
        let tx: VersionedTransaction = bincode::deserialize(&tx_bytes)
            .map_err(|source| SubmitError::DecodeSignedBytes { source })?;
        let signature = tx.signatures.first().copied();
        self.submit_bytes(tx_bytes, signature, mode).await
    }

    /// Submits raw tx bytes after dedupe check.
    async fn submit_bytes(
        &self,
        tx_bytes: Vec<u8>,
        signature: Option<Signature>,
        mode: SubmitMode,
    ) -> Result<SubmitResult, SubmitError> {
        self.enforce_dedupe(signature)?;
        match mode {
            SubmitMode::RpcOnly => self.submit_rpc_only(tx_bytes, signature, mode).await,
            SubmitMode::DirectOnly => self.submit_direct_only(tx_bytes, signature, mode).await,
            SubmitMode::Hybrid => self.submit_hybrid(tx_bytes, signature, mode).await,
        }
    }

    /// Applies signature dedupe policy.
    fn enforce_dedupe(&self, signature: Option<Signature>) -> Result<(), SubmitError> {
        if let Some(signature) = signature {
            let now = Instant::now();
            let mut deduper =
                self.deduper
                    .lock()
                    .map_err(|poisoned| SubmitError::InternalSync {
                        message: poisoned.to_string(),
                    })?;
            if !deduper.check_and_insert(signature, now) {
                return Err(SubmitError::DuplicateSignature);
            }
        }
        Ok(())
    }

    /// Submits through RPC path only.
    async fn submit_rpc_only(
        &self,
        tx_bytes: Vec<u8>,
        signature: Option<Signature>,
        mode: SubmitMode,
    ) -> Result<SubmitResult, SubmitError> {
        let rpc = self
            .rpc_transport
            .as_ref()
            .ok_or(SubmitError::MissingRpcTransport)?;
        let rpc_signature = rpc
            .submit_rpc(&tx_bytes, &self.rpc_config)
            .await
            .map_err(|source| SubmitError::Rpc { source })?;
        Ok(SubmitResult {
            signature,
            mode,
            direct_target: None,
            rpc_signature: Some(rpc_signature),
            used_rpc_fallback: false,
        })
    }

    /// Submits through direct path only.
    async fn submit_direct_only(
        &self,
        tx_bytes: Vec<u8>,
        signature: Option<Signature>,
        mode: SubmitMode,
    ) -> Result<SubmitResult, SubmitError> {
        let direct = self
            .direct_transport
            .as_ref()
            .ok_or(SubmitError::MissingDirectTransport)?;
        let targets = select_targets(self.leader_provider.as_ref(), &self.backups, self.policy);
        if targets.is_empty() {
            return Err(SubmitError::NoDirectTargets);
        }
        let target = direct
            .submit_direct(&tx_bytes, &targets, self.policy, &self.direct_config)
            .await
            .map_err(|source| SubmitError::Direct { source })?;
        Ok(SubmitResult {
            signature,
            mode,
            direct_target: Some(target),
            rpc_signature: None,
            used_rpc_fallback: false,
        })
    }

    /// Submits through hybrid mode (direct first, RPC fallback).
    async fn submit_hybrid(
        &self,
        tx_bytes: Vec<u8>,
        signature: Option<Signature>,
        mode: SubmitMode,
    ) -> Result<SubmitResult, SubmitError> {
        let direct = self
            .direct_transport
            .as_ref()
            .ok_or(SubmitError::MissingDirectTransport)?;
        let rpc = self
            .rpc_transport
            .as_ref()
            .ok_or(SubmitError::MissingRpcTransport)?;

        let targets = select_targets(self.leader_provider.as_ref(), &self.backups, self.policy);
        if !targets.is_empty()
            && let Ok(target) = direct
                .submit_direct(&tx_bytes, &targets, self.policy, &self.direct_config)
                .await
        {
            return Ok(SubmitResult {
                signature,
                mode,
                direct_target: Some(target),
                rpc_signature: None,
                used_rpc_fallback: false,
            });
        }

        let rpc_signature = rpc
            .submit_rpc(&tx_bytes, &self.rpc_config)
            .await
            .map_err(|source| SubmitError::Rpc { source })?;
        Ok(SubmitResult {
            signature,
            mode,
            direct_target: None,
            rpc_signature: Some(rpc_signature),
            used_rpc_fallback: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;
    use crate::{
        builder::TxBuilder,
        providers::{StaticLeaderProvider, StaticRecentBlockhashProvider},
    };
    use solana_keypair::Keypair;
    use solana_signer::Signer;

    /// Mock RPC transport with configurable response.
    #[derive(Debug)]
    struct MockRpcTransport {
        /// Return value to use.
        result: Result<String, SubmitTransportError>,
        /// Number of submit calls.
        calls: Mutex<u64>,
    }

    #[async_trait]
    impl RpcSubmitTransport for MockRpcTransport {
        async fn submit_rpc(
            &self,
            _tx_bytes: &[u8],
            _config: &RpcSubmitConfig,
        ) -> Result<String, SubmitTransportError> {
            if let Ok(mut calls) = self.calls.lock() {
                *calls = calls.saturating_add(1);
            }
            self.result.clone()
        }
    }

    /// Mock direct transport with configurable response.
    #[derive(Debug)]
    struct MockDirectTransport {
        /// Return value to use.
        result: Result<LeaderTarget, SubmitTransportError>,
        /// Number of submit calls.
        calls: Mutex<u64>,
    }

    #[async_trait]
    impl DirectSubmitTransport for MockDirectTransport {
        async fn submit_direct(
            &self,
            _tx_bytes: &[u8],
            _targets: &[LeaderTarget],
            _policy: RoutingPolicy,
            _config: &DirectSubmitConfig,
        ) -> Result<LeaderTarget, SubmitTransportError> {
            if let Ok(mut calls) = self.calls.lock() {
                *calls = calls.saturating_add(1);
            }
            self.result.clone()
        }
    }

    /// Builds one signed transfer transaction for tests.
    fn signed_transfer_bytes() -> (Vec<u8>, Signature) {
        let payer = Keypair::new();
        let recipient = Keypair::new();
        let tx_result = TxBuilder::new(payer.pubkey())
            .add_instruction(solana_system_interface::instruction::transfer(
                &payer.pubkey(),
                &recipient.pubkey(),
                1,
            ))
            .build_and_sign([9_u8; 32], &[&payer]);

        assert!(tx_result.is_ok());
        let mut bytes = Vec::new();
        let mut signature = Signature::default();
        if let Ok(tx) = tx_result {
            let first = tx.signatures.first();
            assert!(first.is_some());
            if let Some(first) = first {
                signature = *first;
            }
            let encoded_result = bincode::serialize(&tx);
            assert!(encoded_result.is_ok());
            if let Ok(encoded) = encoded_result {
                bytes = encoded;
            }
        }
        (bytes, signature)
    }

    /// Returns a static leader target.
    fn target(port: u16) -> LeaderTarget {
        LeaderTarget::new(None, SocketAddr::from(([127, 0, 0, 1], port)))
    }

    #[tokio::test]
    async fn rpc_only_uses_rpc_transport() {
        let rpc = Arc::new(MockRpcTransport {
            result: Ok("rpc-signature".to_owned()),
            calls: Mutex::new(0),
        });
        let direct = Arc::new(MockDirectTransport {
            result: Ok(target(9001)),
            calls: Mutex::new(0),
        });
        let client = TxSubmitClient::new(
            Arc::new(StaticRecentBlockhashProvider::new(Some([9_u8; 32]))),
            Arc::new(StaticLeaderProvider::new(Some(target(9001)), Vec::new())),
        )
        .with_rpc_transport(rpc.clone())
        .with_direct_transport(direct.clone());

        let (bytes, signature) = signed_transfer_bytes();
        let result = client
            .submit_signed(
                SignedTx::VersionedTransactionBytes(bytes),
                SubmitMode::RpcOnly,
            )
            .await;

        assert!(result.is_ok());
        if let Ok(result) = result {
            assert_eq!(result.signature, Some(signature));
            assert_eq!(result.rpc_signature, Some("rpc-signature".to_owned()));
            assert_eq!(result.direct_target, None);
            assert!(!result.used_rpc_fallback);
        }

        let rpc_calls = rpc.calls.lock().map(|calls| *calls).unwrap_or_default();
        let direct_calls = direct.calls.lock().map(|calls| *calls).unwrap_or_default();
        assert_eq!(rpc_calls, 1);
        assert_eq!(direct_calls, 0);
    }

    #[tokio::test]
    async fn direct_only_uses_direct_transport() {
        let rpc = Arc::new(MockRpcTransport {
            result: Ok("rpc-signature".to_owned()),
            calls: Mutex::new(0),
        });
        let direct_target = target(9011);
        let direct = Arc::new(MockDirectTransport {
            result: Ok(direct_target.clone()),
            calls: Mutex::new(0),
        });
        let client = TxSubmitClient::new(
            Arc::new(StaticRecentBlockhashProvider::new(Some([10_u8; 32]))),
            Arc::new(StaticLeaderProvider::new(
                Some(direct_target.clone()),
                Vec::new(),
            )),
        )
        .with_rpc_transport(rpc.clone())
        .with_direct_transport(direct.clone());

        let (bytes, _signature) = signed_transfer_bytes();
        let result = client
            .submit_signed(
                SignedTx::WireTransactionBytes(bytes),
                SubmitMode::DirectOnly,
            )
            .await;

        assert!(result.is_ok());
        if let Ok(result) = result {
            assert_eq!(result.direct_target, Some(direct_target));
            assert_eq!(result.rpc_signature, None);
            assert!(!result.used_rpc_fallback);
        }

        let rpc_calls = rpc.calls.lock().map(|calls| *calls).unwrap_or_default();
        let direct_calls = direct.calls.lock().map(|calls| *calls).unwrap_or_default();
        assert_eq!(rpc_calls, 0);
        assert_eq!(direct_calls, 1);
    }

    #[tokio::test]
    async fn hybrid_falls_back_to_rpc_when_direct_fails() {
        let rpc = Arc::new(MockRpcTransport {
            result: Ok("rpc-fallback-signature".to_owned()),
            calls: Mutex::new(0),
        });
        let direct = Arc::new(MockDirectTransport {
            result: Err(SubmitTransportError::Failure {
                message: "direct failed".to_owned(),
            }),
            calls: Mutex::new(0),
        });
        let client = TxSubmitClient::new(
            Arc::new(StaticRecentBlockhashProvider::new(Some([11_u8; 32]))),
            Arc::new(StaticLeaderProvider::new(Some(target(9021)), Vec::new())),
        )
        .with_rpc_transport(rpc.clone())
        .with_direct_transport(direct.clone());

        let (bytes, _signature) = signed_transfer_bytes();
        let result = client
            .submit_signed(
                SignedTx::VersionedTransactionBytes(bytes),
                SubmitMode::Hybrid,
            )
            .await;

        assert!(result.is_ok());
        if let Ok(result) = result {
            assert_eq!(result.direct_target, None);
            assert_eq!(
                result.rpc_signature,
                Some("rpc-fallback-signature".to_owned())
            );
            assert!(result.used_rpc_fallback);
        }

        let rpc_calls = rpc.calls.lock().map(|calls| *calls).unwrap_or_default();
        let direct_calls = direct.calls.lock().map(|calls| *calls).unwrap_or_default();
        assert_eq!(rpc_calls, 1);
        assert_eq!(direct_calls, 1);
    }

    #[tokio::test]
    async fn duplicate_signature_is_suppressed() {
        let rpc = Arc::new(MockRpcTransport {
            result: Ok("rpc-signature".to_owned()),
            calls: Mutex::new(0),
        });
        let client = TxSubmitClient::new(
            Arc::new(StaticRecentBlockhashProvider::new(Some([12_u8; 32]))),
            Arc::new(StaticLeaderProvider::new(None, Vec::new())),
        )
        .with_rpc_transport(rpc)
        .with_dedupe_ttl(Duration::from_secs(60));

        let (bytes, _signature) = signed_transfer_bytes();
        let first = client
            .submit_signed(
                SignedTx::VersionedTransactionBytes(bytes.clone()),
                SubmitMode::RpcOnly,
            )
            .await;
        assert!(first.is_ok());

        let second = client
            .submit_signed(
                SignedTx::VersionedTransactionBytes(bytes),
                SubmitMode::RpcOnly,
            )
            .await;
        assert!(second.is_err());
        assert!(matches!(second, Err(SubmitError::DuplicateSignature)));
    }
}
