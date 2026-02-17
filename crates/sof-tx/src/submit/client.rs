//! Submission client implementation and mode orchestration.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use solana_signature::Signature;
use solana_signer::signers::Signers;
use solana_transaction::versioned::VersionedTransaction;

use super::{
    DirectSubmitConfig, DirectSubmitTransport, RpcSubmitConfig, RpcSubmitTransport, SignedTx,
    SubmitError, SubmitMode, SubmitResult,
};
use crate::{
    builder::TxBuilder,
    providers::{LeaderProvider, LeaderTarget, RecentBlockhashProvider},
    routing::{RoutingPolicy, SignatureDeduper, select_targets},
};

/// Transaction submission client that orchestrates RPC and direct submit modes.
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
