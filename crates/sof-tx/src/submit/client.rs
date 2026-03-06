//! Submission client implementation and mode orchestration.

use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use solana_signature::Signature;
use solana_signer::signers::Signers;
use solana_transaction::versioned::VersionedTransaction;
use tokio::{
    net::TcpStream,
    task::JoinSet,
    time::{sleep, timeout},
};

use super::{
    DirectSubmitConfig, DirectSubmitTransport, RpcSubmitConfig, RpcSubmitTransport, SignedTx,
    SubmitError, SubmitMode, SubmitReliability, SubmitResult,
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
    deduper: SignatureDeduper,
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
            deduper: SignatureDeduper::new(Duration::from_secs(10)),
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
        self.deduper = SignatureDeduper::new(ttl);
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
        self.direct_config = config.normalized();
        self
    }

    /// Sets direct/hybrid reliability profile.
    #[must_use]
    pub const fn with_reliability(mut self, reliability: SubmitReliability) -> Self {
        self.direct_config = DirectSubmitConfig::from_reliability(reliability);
        self
    }

    /// Builds, signs, and submits a transaction in one API call.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitError`] when blockhash lookup, signing, dedupe, routing, or submission
    /// fails.
    pub async fn submit_builder<T>(
        &mut self,
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
        &mut self,
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
        &mut self,
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
        &mut self,
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
    fn enforce_dedupe(&mut self, signature: Option<Signature>) -> Result<(), SubmitError> {
        if let Some(signature) = signature {
            let now = Instant::now();
            if !self.deduper.check_and_insert(signature, now) {
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
            selected_target_count: 0,
            selected_identity_count: 0,
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
        let direct_config = self.direct_config.clone().normalized();
        let mut last_error = None;
        let attempt_timeout = direct_attempt_timeout(&direct_config);

        for attempt_idx in 0..direct_config.direct_submit_attempts {
            let mut targets = self.select_direct_targets(&direct_config).await;
            rotate_targets_for_attempt(&mut targets, attempt_idx, self.policy);
            let (selected_target_count, selected_identity_count) = summarize_targets(&targets);
            if targets.is_empty() {
                return Err(SubmitError::NoDirectTargets);
            }
            match timeout(
                attempt_timeout,
                direct.submit_direct(&tx_bytes, &targets, self.policy, &direct_config),
            )
            .await
            {
                Ok(Ok(target)) => {
                    self.spawn_agave_rebroadcast(tx_bytes.clone(), &direct_config);
                    return Ok(SubmitResult {
                        signature,
                        mode,
                        direct_target: Some(target),
                        rpc_signature: None,
                        used_rpc_fallback: false,
                        selected_target_count,
                        selected_identity_count,
                    });
                }
                Ok(Err(source)) => last_error = Some(source),
                Err(_elapsed) => {
                    last_error = Some(super::SubmitTransportError::Failure {
                        message: format!(
                            "direct submit attempt timed out after {}ms",
                            attempt_timeout.as_millis()
                        ),
                    });
                }
            }
            if attempt_idx < direct_config.direct_submit_attempts.saturating_sub(1) {
                sleep(direct_config.rebroadcast_interval).await;
            }
        }

        Err(SubmitError::Direct {
            source: last_error.unwrap_or_else(|| super::SubmitTransportError::Failure {
                message: "direct submit attempts exhausted".to_owned(),
            }),
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

        let direct_config = self.direct_config.clone().normalized();
        let attempt_timeout = direct_attempt_timeout(&direct_config);
        for attempt_idx in 0..direct_config.hybrid_direct_attempts {
            let mut targets = self.select_direct_targets(&direct_config).await;
            rotate_targets_for_attempt(&mut targets, attempt_idx, self.policy);
            let (selected_target_count, selected_identity_count) = summarize_targets(&targets);
            if targets.is_empty() {
                break;
            }
            if let Ok(Ok(target)) = timeout(
                attempt_timeout,
                direct.submit_direct(&tx_bytes, &targets, self.policy, &direct_config),
            )
            .await
            {
                self.spawn_agave_rebroadcast(tx_bytes.clone(), &direct_config);
                if direct_config.hybrid_rpc_broadcast
                    && let Ok(rpc_signature) = rpc.submit_rpc(&tx_bytes, &self.rpc_config).await
                {
                    return Ok(SubmitResult {
                        signature,
                        mode,
                        direct_target: Some(target),
                        rpc_signature: Some(rpc_signature),
                        used_rpc_fallback: false,
                        selected_target_count,
                        selected_identity_count,
                    });
                }
                return Ok(SubmitResult {
                    signature,
                    mode,
                    direct_target: Some(target),
                    rpc_signature: None,
                    used_rpc_fallback: false,
                    selected_target_count,
                    selected_identity_count,
                });
            }
            if attempt_idx < direct_config.hybrid_direct_attempts.saturating_sub(1) {
                sleep(direct_config.rebroadcast_interval).await;
            }
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
            selected_target_count: 0,
            selected_identity_count: 0,
        })
    }

    /// Resolves and ranks the direct targets for the next submission attempt.
    async fn select_direct_targets(&self, direct_config: &DirectSubmitConfig) -> Vec<LeaderTarget> {
        select_and_rank_targets(
            self.leader_provider.as_ref(),
            &self.backups,
            self.policy,
            direct_config,
        )
        .await
    }

    /// Starts the post-ack rebroadcast worker when that reliability mode is enabled.
    fn spawn_agave_rebroadcast(&self, tx_bytes: Vec<u8>, direct_config: &DirectSubmitConfig) {
        if !direct_config.agave_rebroadcast_enabled
            || direct_config.agave_rebroadcast_window.is_zero()
        {
            return;
        }
        let Some(direct_transport) = self.direct_transport.clone() else {
            return;
        };
        spawn_agave_rebroadcast_task(
            tx_bytes,
            direct_transport,
            self.leader_provider.clone(),
            self.backups.clone(),
            self.policy,
            direct_config.clone(),
        );
    }
}

#[cfg(not(test))]
/// Replays successful direct submissions for a bounded Agave-like persistence window.
fn spawn_agave_rebroadcast_task(
    tx_bytes: Vec<u8>,
    direct_transport: Arc<dyn DirectSubmitTransport>,
    leader_provider: Arc<dyn LeaderProvider>,
    backups: Vec<LeaderTarget>,
    policy: RoutingPolicy,
    direct_config: DirectSubmitConfig,
) {
    tokio::spawn(async move {
        let deadline = Instant::now()
            .checked_add(direct_config.agave_rebroadcast_window)
            .unwrap_or_else(Instant::now);
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }

            let sleep_for = deadline
                .saturating_duration_since(now)
                .min(direct_config.agave_rebroadcast_interval);
            if !sleep_for.is_zero() {
                sleep(sleep_for).await;
            }

            if Instant::now() >= deadline {
                break;
            }

            let targets = select_and_rank_targets(
                leader_provider.as_ref(),
                backups.as_slice(),
                policy,
                &direct_config,
            )
            .await;
            if targets.is_empty() {
                continue;
            }

            drop(
                timeout(
                    direct_attempt_timeout(&direct_config),
                    direct_transport.submit_direct(&tx_bytes, &targets, policy, &direct_config),
                )
                .await,
            );
        }
    });
}

#[cfg(test)]
/// Test-only stub that disables background rebroadcasting for deterministic assertions.
fn spawn_agave_rebroadcast_task(
    _tx_bytes: Vec<u8>,
    _direct_transport: Arc<dyn DirectSubmitTransport>,
    _leader_provider: Arc<dyn LeaderProvider>,
    _backups: Vec<LeaderTarget>,
    _policy: RoutingPolicy,
    _direct_config: DirectSubmitConfig,
) {
}

/// Selects routing targets and applies optional latency-aware ranking.
async fn select_and_rank_targets(
    leader_provider: &dyn LeaderProvider,
    backups: &[LeaderTarget],
    policy: RoutingPolicy,
    direct_config: &DirectSubmitConfig,
) -> Vec<LeaderTarget> {
    let targets = select_targets(leader_provider, backups, policy);
    rank_targets_by_latency(targets, direct_config).await
}

/// Reorders the probe set by observed TCP connect latency while preserving the tail order.
async fn rank_targets_by_latency(
    targets: Vec<LeaderTarget>,
    direct_config: &DirectSubmitConfig,
) -> Vec<LeaderTarget> {
    if targets.len() <= 1 || !direct_config.latency_aware_targeting {
        return targets;
    }

    let probe_count = targets
        .len()
        .min(direct_config.latency_probe_max_targets.max(1));
    let mut latencies = vec![None; probe_count];
    let mut probes = JoinSet::new();
    for (idx, target) in targets.iter().take(probe_count).cloned().enumerate() {
        let cfg = direct_config.clone();
        probes.spawn(async move { (idx, probe_target_latency(&target, &cfg).await) });
    }
    while let Some(result) = probes.join_next().await {
        if let Ok((idx, latency)) = result
            && idx < latencies.len()
            && let Some(slot) = latencies.get_mut(idx)
        {
            *slot = latency;
        }
    }

    let mut ranked = targets
        .iter()
        .take(probe_count)
        .cloned()
        .enumerate()
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(idx, _target)| {
        (
            latencies.get(*idx).copied().flatten().unwrap_or(u128::MAX),
            *idx,
        )
    });

    let mut output = ranked
        .into_iter()
        .map(|(_idx, target)| target)
        .collect::<Vec<_>>();
    output.extend(targets.iter().skip(probe_count).cloned());
    output
}

/// Probes a target's candidate ports and keeps the best observed connect latency.
async fn probe_target_latency(
    target: &LeaderTarget,
    direct_config: &DirectSubmitConfig,
) -> Option<u128> {
    let mut ports = vec![target.tpu_addr.port()];
    if let Some(port) = direct_config.latency_probe_port
        && port != target.tpu_addr.port()
    {
        ports.push(port);
    }

    let ip = target.tpu_addr.ip();
    let mut best = None::<u128>;
    for port in ports {
        if let Some(latency) =
            probe_tcp_latency(ip, port, direct_config.latency_probe_timeout).await
        {
            best = Some(best.map_or(latency, |current| current.min(latency)));
        }
    }
    best
}

/// Measures one TCP connect attempt and returns elapsed milliseconds on success.
async fn probe_tcp_latency(
    ip: std::net::IpAddr,
    port: u16,
    timeout_duration: Duration,
) -> Option<u128> {
    let start = Instant::now();
    let addr = SocketAddr::new(ip, port);
    let stream = timeout(timeout_duration, TcpStream::connect(addr))
        .await
        .ok()?
        .ok()?;
    drop(stream);
    Some(start.elapsed().as_millis())
}

/// Summarizes the selected target list for observability.
fn summarize_targets(targets: &[LeaderTarget]) -> (usize, usize) {
    let selected_target_count = targets.len();
    let selected_identity_count = targets
        .iter()
        .filter_map(|target| target.identity)
        .collect::<HashSet<_>>()
        .len();
    (selected_target_count, selected_identity_count)
}

/// Rotates the target ordering between attempts to spread retries across candidates.
fn rotate_targets_for_attempt(
    targets: &mut [LeaderTarget],
    attempt_idx: usize,
    policy: RoutingPolicy,
) {
    if attempt_idx == 0 || targets.len() <= 1 {
        return;
    }

    let normalized = policy.normalized();
    let stride = normalized.max_parallel_sends.max(1);
    let rotation = attempt_idx
        .saturating_mul(stride)
        .checked_rem(targets.len())
        .unwrap_or(0);
    if rotation > 0 {
        targets.rotate_left(rotation);
    }
}

/// Bounds one submit attempt so retry loops cannot hang indefinitely.
fn direct_attempt_timeout(direct_config: &DirectSubmitConfig) -> Duration {
    direct_config
        .global_timeout
        .saturating_add(direct_config.per_target_timeout)
        .saturating_add(direct_config.rebroadcast_interval)
        .max(Duration::from_secs(8))
}
