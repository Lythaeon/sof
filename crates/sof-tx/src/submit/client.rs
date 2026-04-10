//! Submission client implementation and mode orchestration.

use std::{
    collections::HashMap,
    collections::HashSet,
    net::SocketAddr,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc::{self as std_mpsc, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use sof_support::{short_vec::decode_short_u16_len_prefix, time_support::duration_millis_u64};
use sof_types::SignatureBytes;
use tokio::{
    net::TcpStream,
    sync::mpsc,
    task::JoinSet,
    time::{sleep, timeout},
};

use super::{
    DirectSubmitConfig, DirectSubmitTransport, JitoSubmitConfig, JitoSubmitTransport,
    RpcSubmitConfig, RpcSubmitTransport, SignedTx, SubmitError, SubmitMode, SubmitPlan,
    SubmitReliability, SubmitResult, SubmitRoute, SubmitStrategy, SubmitTransportError,
    TxFlowSafetyQuality, TxFlowSafetySource, TxSubmitClientBuilder, TxSubmitContext,
    TxSubmitGuardPolicy, TxSubmitOutcome, TxSubmitOutcomeKind, TxSubmitOutcomeReporter,
    TxToxicFlowRejectionReason, TxToxicFlowTelemetry, TxToxicFlowTelemetrySnapshot,
};
use crate::{
    providers::{
        LeaderProvider, LeaderTarget, RecentBlockhashProvider, RpcRecentBlockhashProvider,
        StaticLeaderProvider,
    },
    routing::{RoutingPolicy, SignatureDeduper, select_targets},
    submit::{JsonRpcTransport, types::TxSuppressionCache},
};

/// Transaction submission client that orchestrates RPC and direct submit modes.
pub struct TxSubmitClient {
    /// Blockhash source used by unsigned submit path.
    blockhash_provider: Arc<dyn RecentBlockhashProvider>,
    /// Optional RPC-backed blockhash source refreshed on demand before unsigned submit.
    on_demand_blockhash_provider: Option<Arc<RpcRecentBlockhashProvider>>,
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
    /// Optional Jito transport.
    jito_transport: Option<Arc<dyn JitoSubmitTransport>>,
    /// RPC tuning.
    rpc_config: RpcSubmitConfig,
    /// Jito tuning.
    jito_config: JitoSubmitConfig,
    /// Direct tuning.
    direct_config: DirectSubmitConfig,
    /// Optional toxic-flow guard source.
    flow_safety_source: Option<Arc<dyn TxFlowSafetySource>>,
    /// Guard policy applied before submit.
    guard_policy: TxSubmitGuardPolicy,
    /// Built-in suppression keys.
    suppression: TxSuppressionCache,
    /// Built-in toxic-flow telemetry sink.
    telemetry: Arc<TxToxicFlowTelemetry>,
    /// Optional external outcome reporter handle.
    outcome_reporter: Option<OutcomeReporterHandle>,
}

impl TxSubmitClient {
    /// Creates a high-level builder for common submit configurations.
    #[must_use]
    pub fn builder() -> TxSubmitClientBuilder {
        TxSubmitClientBuilder::new()
    }

    /// Creates a submission client with no transports preconfigured.
    #[must_use]
    pub fn new(
        blockhash_provider: Arc<dyn RecentBlockhashProvider>,
        leader_provider: Arc<dyn LeaderProvider>,
    ) -> Self {
        Self {
            blockhash_provider,
            on_demand_blockhash_provider: None,
            leader_provider,
            backups: Vec::new(),
            policy: RoutingPolicy::default(),
            deduper: SignatureDeduper::new(Duration::from_secs(10)),
            rpc_transport: None,
            direct_transport: None,
            jito_transport: None,
            rpc_config: RpcSubmitConfig::default(),
            jito_config: JitoSubmitConfig::default(),
            direct_config: DirectSubmitConfig::default(),
            flow_safety_source: None,
            guard_policy: TxSubmitGuardPolicy::default(),
            suppression: TxSuppressionCache::default(),
            telemetry: TxToxicFlowTelemetry::shared(),
            outcome_reporter: None,
        }
    }

    /// Creates a client with an empty leader source for blockhash-only submit paths.
    #[must_use]
    pub fn blockhash_only(blockhash_provider: Arc<dyn RecentBlockhashProvider>) -> Self {
        Self::new(
            blockhash_provider,
            Arc::new(StaticLeaderProvider::default()),
        )
    }

    /// Creates a client with RPC-backed on-demand blockhash sourcing and no leader routing.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError`] when the RPC-backed blockhash provider cannot be created.
    pub fn blockhash_via_rpc(rpc_url: impl Into<String>) -> Result<Self, SubmitTransportError> {
        let blockhash_provider = Arc::new(RpcRecentBlockhashProvider::new(rpc_url.into())?);
        Ok(Self::blockhash_only(blockhash_provider.clone())
            .with_rpc_blockhash_provider(blockhash_provider))
    }

    /// Creates an RPC-only client from one RPC URL used for both blockhash and submission.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError`] when the RPC transport or blockhash provider
    /// cannot be initialized.
    pub fn rpc_only(rpc_url: impl Into<String>) -> Result<Self, SubmitTransportError> {
        let rpc_url = rpc_url.into();
        let client = Self::blockhash_via_rpc(rpc_url.clone())?;
        let rpc_transport = Arc::new(JsonRpcTransport::new(rpc_url)?);
        Ok(client.with_rpc_transport(rpc_transport))
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

    /// Sets Jito transport.
    #[must_use]
    pub fn with_jito_transport(mut self, transport: Arc<dyn JitoSubmitTransport>) -> Self {
        self.jito_transport = Some(transport);
        self
    }

    /// Sets RPC submit tuning.
    #[must_use]
    pub fn with_rpc_config(mut self, config: RpcSubmitConfig) -> Self {
        self.rpc_config = config;
        self
    }

    /// Sets Jito submit tuning.
    #[must_use]
    pub const fn with_jito_config(mut self, config: JitoSubmitConfig) -> Self {
        self.jito_config = config;
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

    /// Sets the toxic-flow guard source used before submission.
    #[must_use]
    pub fn with_flow_safety_source(mut self, source: Arc<dyn TxFlowSafetySource>) -> Self {
        self.flow_safety_source = Some(source);
        self
    }

    /// Sets the toxic-flow guard policy.
    #[must_use]
    pub const fn with_guard_policy(mut self, policy: TxSubmitGuardPolicy) -> Self {
        self.guard_policy = policy;
        self
    }

    /// Sets an optional external outcome reporter.
    #[must_use]
    pub fn with_outcome_reporter(mut self, reporter: Arc<dyn TxSubmitOutcomeReporter>) -> Self {
        self.outcome_reporter = Some(OutcomeReporterHandle::new(reporter));
        self
    }

    /// Registers an RPC-backed blockhash provider to refresh on demand for unsigned submit paths.
    #[must_use]
    pub fn with_rpc_blockhash_provider(
        mut self,
        provider: Arc<RpcRecentBlockhashProvider>,
    ) -> Self {
        self.on_demand_blockhash_provider = Some(provider);
        self
    }

    /// Returns the current built-in toxic-flow telemetry snapshot.
    #[must_use]
    pub fn toxic_flow_telemetry(&self) -> TxToxicFlowTelemetrySnapshot {
        self.telemetry.snapshot()
    }

    /// Records one external terminal outcome against the built-in telemetry and optional reporter.
    pub fn record_external_outcome(&self, outcome: &TxSubmitOutcome) {
        record_external_outcome_shared(&self.telemetry, self.outcome_reporter.as_ref(), outcome);
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
        self.submit_signed_with_context(signed_tx, mode, TxSubmitContext::default())
            .await
    }

    /// Submits externally signed transaction bytes through one route plan.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitError`] when decoding, dedupe, routing, or submission fails.
    pub async fn submit_signed_via(
        &mut self,
        signed_tx: SignedTx,
        plan: SubmitPlan,
    ) -> Result<SubmitResult, SubmitError> {
        self.submit_signed_with_context_via(signed_tx, plan, TxSubmitContext::default())
            .await
    }

    /// Submits externally signed transaction bytes with explicit toxic-flow context.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitError`] when decoding, dedupe, toxic-flow guards, routing, or submission
    /// fails.
    pub async fn submit_signed_with_context(
        &mut self,
        signed_tx: SignedTx,
        mode: SubmitMode,
        context: TxSubmitContext,
    ) -> Result<SubmitResult, SubmitError> {
        self.submit_signed_with_context_via(signed_tx, SubmitPlan::from(mode), context)
            .await
    }

    /// Submits externally signed transaction bytes with explicit toxic-flow context and route
    /// plan.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitError`] when decoding, dedupe, toxic-flow guards, routing, or submission
    /// fails.
    pub async fn submit_signed_with_context_via(
        &mut self,
        signed_tx: SignedTx,
        plan: SubmitPlan,
        context: TxSubmitContext,
    ) -> Result<SubmitResult, SubmitError> {
        let tx_bytes = match signed_tx {
            SignedTx::VersionedTransactionBytes(bytes) => bytes,
            SignedTx::WireTransactionBytes(bytes) => bytes,
        };
        let signature = extract_first_signature(&tx_bytes)?;
        self.submit_bytes(tx_bytes, signature, plan, context).await
    }

    /// Refreshes any configured on-demand RPC blockhash source and returns the latest bytes.
    ///
    /// This is intended for explicit compatibility layers that build Solana-native transactions
    /// outside the core byte-oriented `sof-tx` API surface.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError`] when the RPC-backed blockhash refresh fails.
    pub async fn refresh_latest_blockhash_bytes(
        &self,
    ) -> Result<Option<[u8; 32]>, SubmitTransportError> {
        if let Some(provider) = &self.on_demand_blockhash_provider {
            let _ = provider.refresh().await?;
        }
        Ok(self.latest_blockhash_bytes())
    }

    /// Returns the latest cached recent blockhash bytes when available.
    #[must_use]
    pub fn latest_blockhash_bytes(&self) -> Option<[u8; 32]> {
        self.blockhash_provider.latest_blockhash()
    }

    /// Submits raw tx bytes after dedupe check.
    async fn submit_bytes(
        &mut self,
        tx_bytes: Vec<u8>,
        signature: Option<SignatureBytes>,
        plan: SubmitPlan,
        context: TxSubmitContext,
    ) -> Result<SubmitResult, SubmitError> {
        let plan = plan.into_normalized();
        self.validate_submit_plan(&plan)?;
        self.enforce_toxic_flow_guards(signature, &plan, &context)?;
        self.enforce_dedupe(signature)?;
        let tx_bytes = Arc::<[u8]>::from(tx_bytes);
        match plan.strategy {
            SubmitStrategy::OrderedFallback => {
                self.submit_routes_in_order(tx_bytes, signature, plan).await
            }
            SubmitStrategy::AllAtOnce => {
                self.submit_routes_all_at_once(tx_bytes, signature, plan)
                    .await
            }
        }
    }

    /// Applies signature dedupe policy.
    fn enforce_dedupe(&mut self, signature: Option<SignatureBytes>) -> Result<(), SubmitError> {
        if let Some(signature) = signature {
            let now = Instant::now();
            if !self.deduper.check_and_insert(signature, now) {
                return Err(SubmitError::DuplicateSignature);
            }
        }
        Ok(())
    }

    /// Applies toxic-flow guard policy before transport.
    fn enforce_toxic_flow_guards(
        &mut self,
        signature: Option<SignatureBytes>,
        plan: &SubmitPlan,
        context: &TxSubmitContext,
    ) -> Result<(), SubmitError> {
        let legacy_mode = plan.legacy_mode();
        let now = SystemTime::now();
        let opportunity_age_ms = context
            .opportunity_created_at
            .and_then(|created_at| now.duration_since(created_at).ok())
            .map(duration_millis_u64);
        if let Some(age_ms) = opportunity_age_ms
            && let Some(max_age) = self.guard_policy.max_opportunity_age
        {
            let max_allowed_ms = duration_millis_u64(max_age);
            if age_ms > max_allowed_ms {
                return Err(self.reject_with_outcome(
                    TxToxicFlowRejectionReason::OpportunityStale {
                        age_ms,
                        max_allowed_ms,
                    },
                    TxSubmitOutcomeKind::RejectedDueToStaleness,
                    RejectionMetadata {
                        signature,
                        plan: plan.clone(),
                        legacy_mode,
                        state_version: None,
                        opportunity_age_ms,
                    },
                ));
            }
        }

        if self.suppression.is_suppressed(
            &context.suppression_keys,
            now,
            self.guard_policy.suppression_ttl,
        ) {
            return Err(self.reject_with_outcome(
                TxToxicFlowRejectionReason::Suppressed,
                TxSubmitOutcomeKind::Suppressed,
                RejectionMetadata {
                    signature,
                    plan: plan.clone(),
                    legacy_mode,
                    state_version: None,
                    opportunity_age_ms,
                },
            ));
        }

        if let Some(source) = &self.flow_safety_source {
            let snapshot = source.toxic_flow_snapshot();
            if self.guard_policy.reject_on_replay_recovery_pending
                && snapshot.replay_recovery_pending
            {
                return Err(self.reject_with_outcome(
                    TxToxicFlowRejectionReason::ReplayRecoveryPending,
                    TxSubmitOutcomeKind::RejectedDueToReplayRecovery,
                    RejectionMetadata {
                        signature,
                        plan: plan.clone(),
                        legacy_mode,
                        state_version: snapshot.current_state_version,
                        opportunity_age_ms,
                    },
                ));
            }
            if self.guard_policy.require_stable_control_plane
                && !matches!(snapshot.quality, TxFlowSafetyQuality::Stable)
            {
                let outcome_kind = match snapshot.quality {
                    TxFlowSafetyQuality::ReorgRisk | TxFlowSafetyQuality::Provisional => {
                        TxSubmitOutcomeKind::RejectedDueToReorgRisk
                    }
                    TxFlowSafetyQuality::Stale => TxSubmitOutcomeKind::RejectedDueToStaleness,
                    TxFlowSafetyQuality::Degraded
                    | TxFlowSafetyQuality::IncompleteControlPlane
                    | TxFlowSafetyQuality::Stable => TxSubmitOutcomeKind::Suppressed,
                };
                return Err(self.reject_with_outcome(
                    TxToxicFlowRejectionReason::UnsafeControlPlane {
                        quality: snapshot.quality,
                    },
                    outcome_kind,
                    RejectionMetadata {
                        signature,
                        plan: plan.clone(),
                        legacy_mode,
                        state_version: snapshot.current_state_version,
                        opportunity_age_ms,
                    },
                ));
            }
            if let (Some(decision_version), Some(current_version), Some(max_allowed)) = (
                context.decision_state_version,
                snapshot.current_state_version,
                self.guard_policy.max_state_version_drift,
            ) {
                let drift = current_version.saturating_sub(decision_version);
                if drift > max_allowed {
                    return Err(self.reject_with_outcome(
                        TxToxicFlowRejectionReason::StateDrift { drift, max_allowed },
                        TxSubmitOutcomeKind::RejectedDueToStateDrift,
                        RejectionMetadata {
                            signature,
                            plan: plan.clone(),
                            legacy_mode,
                            state_version: Some(current_version),
                            opportunity_age_ms,
                        },
                    ));
                }
            }
        }

        self.suppression.insert_all(&context.suppression_keys, now);
        Ok(())
    }

    /// Builds one rejection error while recording telemetry and reporting.
    fn reject_with_outcome(
        &self,
        reason: TxToxicFlowRejectionReason,
        outcome_kind: TxSubmitOutcomeKind,
        metadata: RejectionMetadata,
    ) -> SubmitError {
        let outcome = TxSubmitOutcome {
            kind: outcome_kind,
            signature: metadata.signature,
            route: None,
            plan: metadata.plan,
            legacy_mode: metadata.legacy_mode,
            rpc_signature: None,
            jito_signature: None,
            jito_bundle_id: None,
            state_version: metadata.state_version,
            opportunity_age_ms: metadata.opportunity_age_ms,
        };
        self.record_external_outcome(&outcome);
        SubmitError::ToxicFlow { reason }
    }

    /// Validates that every configured route has the required transport wiring.
    fn validate_submit_plan(&self, plan: &SubmitPlan) -> Result<(), SubmitError> {
        if plan.routes.is_empty() {
            return Err(SubmitError::InternalSync {
                message: "submit plan must contain at least one route".to_owned(),
            });
        }
        for route in &plan.routes {
            match route {
                SubmitRoute::Rpc if self.rpc_transport.is_none() => {
                    return Err(SubmitError::MissingRpcTransport);
                }
                SubmitRoute::Jito if self.jito_transport.is_none() => {
                    return Err(SubmitError::MissingJitoTransport);
                }
                SubmitRoute::Direct if self.direct_transport.is_none() => {
                    return Err(SubmitError::MissingDirectTransport);
                }
                SubmitRoute::Rpc | SubmitRoute::Jito | SubmitRoute::Direct => {}
            }
        }
        Ok(())
    }

    /// Executes one route plan in order and returns the first successful route.
    async fn submit_routes_in_order(
        &self,
        tx_bytes: Arc<[u8]>,
        signature: Option<SignatureBytes>,
        plan: SubmitPlan,
    ) -> Result<SubmitResult, SubmitError> {
        let legacy_mode = plan.legacy_mode();
        let mut last_error = None;
        let task_context = self.route_task_context();
        for (route_idx, route) in plan.routes.iter().copied().enumerate() {
            let next_idx = route_idx.saturating_add(1);
            let has_later_routes = plan.routes.get(next_idx).is_some();
            let direct_mode = if has_later_routes {
                DirectExecutionMode::Fallback
            } else {
                DirectExecutionMode::Standalone
            };
            match submit_one_route_task(
                route,
                Arc::clone(&tx_bytes),
                task_context.clone(),
                direct_mode,
            )
            .await
            {
                Ok(outcome) => {
                    self.record_route_outcome(signature, &plan, &outcome);
                    if matches!(outcome.route, SubmitRoute::Direct) {
                        self.spawn_agave_rebroadcast(
                            Arc::clone(&tx_bytes),
                            &self.direct_config.clone().normalized(),
                        );
                        if has_later_routes
                            && self.direct_config.hybrid_rpc_broadcast
                            && plan
                                .routes
                                .iter()
                                .skip(next_idx)
                                .any(|next| *next == SubmitRoute::Rpc)
                        {
                            self.spawn_background_rpc_broadcast(
                                Arc::clone(&tx_bytes),
                                signature,
                                plan.clone(),
                            );
                        }
                    }
                    return Ok(SubmitResult {
                        signature,
                        plan,
                        legacy_mode,
                        first_success_route: Some(outcome.route),
                        successful_routes: vec![outcome.route],
                        direct_target: outcome.direct_target,
                        rpc_signature: outcome.rpc_signature,
                        jito_signature: outcome.jito_signature,
                        jito_bundle_id: outcome.jito_bundle_id,
                        used_fallback_route: route_idx > 0,
                        selected_target_count: outcome.selected_target_count,
                        selected_identity_count: outcome.selected_identity_count,
                    });
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| SubmitError::InternalSync {
            message: "ordered submit plan completed without a route outcome".to_owned(),
        }))
    }

    /// Executes every configured route at once and returns on the first successful route.
    async fn submit_routes_all_at_once(
        &self,
        tx_bytes: Arc<[u8]>,
        signature: Option<SignatureBytes>,
        plan: SubmitPlan,
    ) -> Result<SubmitResult, SubmitError> {
        let legacy_mode = plan.legacy_mode();
        let task_context = self.route_task_context();
        let direct_transport = self.direct_transport.clone();
        let leader_provider = self.leader_provider.clone();
        let backups = self.backups.clone();
        let policy = self.policy;
        let direct_config = self.direct_config.clone().normalized();
        let (result_tx, mut result_rx) = mpsc::unbounded_channel();
        for (route_idx, route) in plan.routes.iter().copied().enumerate() {
            let task_context = task_context.clone();
            let tx_bytes = Arc::clone(&tx_bytes);
            let result_tx = result_tx.clone();
            let telemetry = Arc::clone(&self.telemetry);
            let reporter = self.outcome_reporter.clone();
            let flow_safety_source = self.flow_safety_source.clone();
            let plan_for_task = plan.clone();
            let direct_transport = direct_transport.clone();
            let leader_provider = leader_provider.clone();
            let backups = backups.clone();
            let direct_config = direct_config.clone();
            tokio::spawn(async move {
                let result = submit_one_route_task(
                    route,
                    Arc::clone(&tx_bytes),
                    task_context,
                    DirectExecutionMode::Standalone,
                )
                .await;
                if let Ok(outcome) = &result {
                    record_route_outcome_shared(
                        &telemetry,
                        reporter.as_ref(),
                        flow_safety_source.as_ref(),
                        signature,
                        &plan_for_task,
                        outcome,
                    );
                    if matches!(outcome.route, SubmitRoute::Direct)
                        && direct_config.agave_rebroadcast_enabled
                        && !direct_config.agave_rebroadcast_window.is_zero()
                        && let Some(direct_transport) = direct_transport
                    {
                        spawn_agave_rebroadcast_task(
                            Arc::clone(&tx_bytes),
                            direct_transport,
                            leader_provider,
                            backups,
                            policy,
                            direct_config.clone(),
                        );
                    }
                }
                drop(result_tx.send((route_idx, result)));
            });
        }
        drop(result_tx);

        let mut errors_by_route: Vec<Option<SubmitError>> = std::iter::repeat_with(|| None)
            .take(plan.routes.len())
            .collect();
        while let Some((route_idx, result)) = result_rx.recv().await {
            match result {
                Ok(outcome) => {
                    return Ok(SubmitResult {
                        signature,
                        plan,
                        legacy_mode,
                        first_success_route: Some(outcome.route),
                        successful_routes: vec![outcome.route],
                        direct_target: outcome.direct_target,
                        rpc_signature: outcome.rpc_signature,
                        jito_signature: outcome.jito_signature,
                        jito_bundle_id: outcome.jito_bundle_id,
                        used_fallback_route: false,
                        selected_target_count: outcome.selected_target_count,
                        selected_identity_count: outcome.selected_identity_count,
                    });
                }
                Err(error) => {
                    if let Some(slot) = errors_by_route.get_mut(route_idx) {
                        *slot = Some(error);
                    }
                }
            }
        }

        Err(errors_by_route
            .into_iter()
            .flatten()
            .next()
            .unwrap_or_else(|| SubmitError::InternalSync {
                message: "all-at-once submit plan completed without a route outcome".to_owned(),
            }))
    }

    /// Records one accepted route as a terminal telemetry outcome.
    fn record_route_outcome(
        &self,
        signature: Option<SignatureBytes>,
        plan: &SubmitPlan,
        outcome: &RouteSubmitOutcome,
    ) {
        record_route_outcome_shared(
            &self.telemetry,
            self.outcome_reporter.as_ref(),
            self.flow_safety_source.as_ref(),
            signature,
            plan,
            outcome,
        );
    }

    /// Clones the current route transports and config into one per-attempt task context.
    fn route_task_context(&self) -> RouteTaskContext {
        RouteTaskContext {
            rpc_transport: self.rpc_transport.clone(),
            jito_transport: self.jito_transport.clone(),
            direct_transport: self.direct_transport.clone(),
            leader_provider: self.leader_provider.clone(),
            backups: Arc::from(self.backups.clone()),
            policy: self.policy,
            rpc_config: self.rpc_config.clone(),
            jito_config: self.jito_config.clone(),
            direct_config: self.direct_config.clone().normalized(),
        }
    }

    /// Starts the post-ack rebroadcast worker when that reliability mode is enabled.
    fn spawn_agave_rebroadcast(&self, tx_bytes: Arc<[u8]>, direct_config: &DirectSubmitConfig) {
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

    /// Starts one best-effort background RPC rebroadcast without delaying direct success.
    fn spawn_background_rpc_broadcast(
        &self,
        tx_bytes: Arc<[u8]>,
        signature: Option<SignatureBytes>,
        plan: SubmitPlan,
    ) {
        let Some(rpc) = self.rpc_transport.clone() else {
            return;
        };
        let rpc_config = self.rpc_config.clone();
        let telemetry = Arc::clone(&self.telemetry);
        let reporter = self.outcome_reporter.clone();
        let flow_safety_source = self.flow_safety_source.clone();
        tokio::spawn(async move {
            if let Ok(rpc_signature) = rpc.submit_rpc(tx_bytes.as_ref(), &rpc_config).await {
                let outcome = RouteSubmitOutcome {
                    route: SubmitRoute::Rpc,
                    direct_target: None,
                    rpc_signature: Some(rpc_signature),
                    jito_signature: None,
                    jito_bundle_id: None,
                    selected_target_count: 0,
                    selected_identity_count: 0,
                };
                record_route_outcome_shared(
                    &telemetry,
                    reporter.as_ref(),
                    flow_safety_source.as_ref(),
                    signature,
                    &plan,
                    &outcome,
                );
            }
        });
    }
}

/// Carries toxic-flow rejection metadata into the shared rejection helper.
#[derive(Debug, Clone)]
struct RejectionMetadata {
    /// Signature being evaluated, when already known.
    signature: Option<SignatureBytes>,
    /// Route plan that was being attempted.
    plan: SubmitPlan,
    /// Matching legacy preset when the plan is one exact compatibility shape.
    legacy_mode: Option<SubmitMode>,
    /// Flow-safety state version attached to the rejection, when any.
    state_version: Option<u64>,
    /// Age of the triggering opportunity in milliseconds, when tracked.
    opportunity_age_ms: Option<u64>,
}

/// Cloned submit transports and route config for one route execution task.
#[derive(Clone)]
struct RouteTaskContext {
    /// RPC submit transport, when configured.
    rpc_transport: Option<Arc<dyn RpcSubmitTransport>>,
    /// Jito submit transport, when configured.
    jito_transport: Option<Arc<dyn JitoSubmitTransport>>,
    /// Direct submit transport, when configured.
    direct_transport: Option<Arc<dyn DirectSubmitTransport>>,
    /// Source of leader targets.
    leader_provider: Arc<dyn LeaderProvider>,
    /// Backup targets configured alongside the leader provider.
    backups: Arc<[LeaderTarget]>,
    /// Route selection policy for direct submission.
    policy: RoutingPolicy,
    /// RPC submit tuning.
    rpc_config: RpcSubmitConfig,
    /// Jito submit tuning.
    jito_config: JitoSubmitConfig,
    /// Direct submit tuning.
    direct_config: DirectSubmitConfig,
}

/// Selects whether direct route execution is standalone or part of a fallback chain.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum DirectExecutionMode {
    /// Execute direct submission using the direct-only retry budget.
    Standalone,
    /// Execute direct submission using the hybrid fallback retry budget.
    Fallback,
}

/// One successful route execution before it is folded into the public submit result.
#[derive(Debug)]
struct RouteSubmitOutcome {
    /// Route that accepted the submission.
    route: SubmitRoute,
    /// Direct target that accepted the submission, when any.
    direct_target: Option<LeaderTarget>,
    /// RPC signature metadata for accepted RPC submission.
    rpc_signature: Option<String>,
    /// Jito transaction signature metadata when available.
    jito_signature: Option<String>,
    /// Jito bundle id metadata when available.
    jito_bundle_id: Option<String>,
    /// Number of direct targets considered for this attempt.
    selected_target_count: usize,
    /// Number of unique identities represented by the direct targets.
    selected_identity_count: usize,
}

/// Records one structured outcome through built-in telemetry and any external reporter.
fn record_external_outcome_shared(
    telemetry: &Arc<TxToxicFlowTelemetry>,
    reporter: Option<&OutcomeReporterHandle>,
    outcome: &TxSubmitOutcome,
) {
    telemetry.record(outcome);
    if let Some(reporter) = reporter {
        reporter.dispatch(telemetry, outcome.clone());
    }
}

/// Records one accepted route as a terminal telemetry outcome using shared sinks.
fn record_route_outcome_shared(
    telemetry: &Arc<TxToxicFlowTelemetry>,
    reporter: Option<&OutcomeReporterHandle>,
    flow_safety_source: Option<&Arc<dyn TxFlowSafetySource>>,
    signature: Option<SignatureBytes>,
    plan: &SubmitPlan,
    outcome: &RouteSubmitOutcome,
) {
    let kind = match outcome.route {
        SubmitRoute::Rpc => TxSubmitOutcomeKind::RpcAccepted,
        SubmitRoute::Jito => TxSubmitOutcomeKind::JitoAccepted,
        SubmitRoute::Direct => TxSubmitOutcomeKind::DirectAccepted,
    };
    let outcome = TxSubmitOutcome {
        kind,
        signature,
        route: Some(outcome.route),
        plan: plan.clone(),
        legacy_mode: plan.legacy_mode(),
        rpc_signature: outcome.rpc_signature.clone(),
        jito_signature: outcome.jito_signature.clone(),
        jito_bundle_id: outcome.jito_bundle_id.clone(),
        state_version: flow_safety_source
            .and_then(|source| source.toxic_flow_snapshot().current_state_version),
        opportunity_age_ms: None,
    };
    record_external_outcome_shared(telemetry, reporter, &outcome);
}

/// Outcome reporter lifecycle state stored by one client.
#[derive(Clone)]
enum OutcomeReporterHandle {
    /// Ready shared dispatcher for this reporter instance.
    Ready(Arc<OutcomeReporterDispatcher>),
    /// Reporter worker could not be created.
    Unavailable,
}

impl OutcomeReporterHandle {
    /// Creates one handle for the provided reporter.
    fn new(reporter: Arc<dyn TxSubmitOutcomeReporter>) -> Self {
        match OutcomeReporterDispatcher::shared(reporter) {
            Ok(dispatcher) => Self::Ready(dispatcher),
            Err(error) => {
                eprintln!("sof-tx: failed to start external outcome reporter worker: {error}");
                Self::Unavailable
            }
        }
    }

    /// Dispatches one outcome without extending the submit hot path.
    fn dispatch(&self, telemetry: &Arc<TxToxicFlowTelemetry>, outcome: TxSubmitOutcome) {
        match self {
            Self::Ready(dispatcher) => match dispatcher.dispatch(outcome) {
                ReporterDispatchStatus::Enqueued => {}
                ReporterDispatchStatus::DroppedFull => telemetry.record_reporter_drop(),
                ReporterDispatchStatus::Unavailable => telemetry.record_reporter_unavailable(),
            },
            Self::Unavailable => telemetry.record_reporter_unavailable(),
        }
    }
}

/// Shared worker state for one concrete reporter instance.
struct OutcomeReporterDispatcher {
    /// Bounded FIFO channel to the reporter worker.
    tx: SyncSender<TxSubmitOutcome>,
    /// Ensures queue saturation is surfaced without spamming stderr.
    queue_full_warned: AtomicBool,
    /// Ensures worker disconnect is surfaced without spamming stderr.
    unavailable_warned: AtomicBool,
}

impl OutcomeReporterDispatcher {
    /// Maximum number of pending outcomes kept for the external reporter.
    #[cfg(not(test))]
    const QUEUE_CAPACITY: usize = 1024;
    #[cfg(test)]
    const QUEUE_CAPACITY: usize = 8;

    /// Creates one shared dispatcher and worker thread.
    fn shared(reporter: Arc<dyn TxSubmitOutcomeReporter>) -> Result<Arc<Self>, std::io::Error> {
        let key = reporter_identity(&reporter);
        let registry = outcome_reporter_registry();
        let mut registry = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry.retain(|_key, dispatcher| dispatcher.strong_count() > 0);
        if let Some(existing) = registry.get(&key).and_then(Weak::upgrade) {
            return Ok(existing);
        }

        let (tx, rx) = std_mpsc::sync_channel::<TxSubmitOutcome>(Self::QUEUE_CAPACITY);
        thread::Builder::new()
            .name("sof-tx-outcome-reporter".to_owned())
            .spawn(move || {
                while let Ok(outcome) = rx.recv() {
                    drop(catch_unwind(AssertUnwindSafe(|| {
                        reporter.record_outcome(&outcome);
                    })));
                }
            })?;

        let dispatcher = Arc::new(Self {
            tx,
            queue_full_warned: AtomicBool::new(false),
            unavailable_warned: AtomicBool::new(false),
        });
        let _ = registry.insert(key, Arc::downgrade(&dispatcher));
        Ok(dispatcher)
    }

    /// Enqueues one outcome without blocking the submit caller.
    fn dispatch(&self, outcome: TxSubmitOutcome) -> ReporterDispatchStatus {
        match self.tx.try_send(outcome) {
            Ok(()) => {
                self.queue_full_warned.store(false, Ordering::Relaxed);
                ReporterDispatchStatus::Enqueued
            }
            Err(TrySendError::Disconnected(_)) => {
                if !self.unavailable_warned.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "sof-tx: external outcome reporter worker stopped; dropping reporter outcomes"
                    );
                }
                ReporterDispatchStatus::Unavailable
            }
            Err(TrySendError::Full(_)) => {
                if !self.queue_full_warned.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "sof-tx: external outcome reporter queue is full; dropping reporter outcomes until it drains"
                    );
                }
                ReporterDispatchStatus::DroppedFull
            }
        }
    }
}

/// Result of one best-effort reporter dispatch attempt.
enum ReporterDispatchStatus {
    /// Outcome was queued successfully.
    Enqueued,
    /// Outcome was dropped because the queue was full.
    DroppedFull,
    /// Outcome could not be queued because the reporter worker was unavailable.
    Unavailable,
}

/// Shared registry of per-reporter dispatchers.
static OUTCOME_REPORTER_REGISTRY: OnceLock<Mutex<HashMap<usize, Weak<OutcomeReporterDispatcher>>>> =
    OnceLock::new();

/// Returns the shared dispatcher registry.
fn outcome_reporter_registry() -> &'static Mutex<HashMap<usize, Weak<OutcomeReporterDispatcher>>> {
    OUTCOME_REPORTER_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Stable identity key for one reporter instance.
fn reporter_identity(reporter: &Arc<dyn TxSubmitOutcomeReporter>) -> usize {
    Arc::as_ptr(reporter) as *const () as usize
}

/// Extracts the first transaction signature from serialized transaction bytes.
fn extract_first_signature(tx_bytes: &[u8]) -> Result<Option<SignatureBytes>, SubmitError> {
    let Some((signature_count, offset)) = decode_short_u16_len_prefix(tx_bytes) else {
        return Err(decode_signed_bytes_error(
            "transaction bytes did not contain a valid signature vector prefix",
        ));
    };
    if signature_count == 0 {
        return Ok(None);
    }
    let signature_end = offset.saturating_add(64);
    let Some(signature_bytes) = tx_bytes.get(offset..signature_end) else {
        return Err(decode_signed_bytes_error(
            "transaction bytes ended before the first signature completed",
        ));
    };
    let mut signature = [0_u8; 64];
    signature.copy_from_slice(signature_bytes);
    Ok(Some(SignatureBytes::new(signature)))
}

/// Builds one signed-byte decode error from a static message.
fn decode_signed_bytes_error(message: &'static str) -> SubmitError {
    SubmitError::DecodeSignedBytes {
        source: Box::new(bincode::ErrorKind::Custom(message.to_owned())),
    }
}

/// Executes one concrete submit route with the cloned task context for that attempt.
async fn submit_one_route_task(
    route: SubmitRoute,
    tx_bytes: Arc<[u8]>,
    task_context: RouteTaskContext,
    direct_mode: DirectExecutionMode,
) -> Result<RouteSubmitOutcome, SubmitError> {
    match route {
        SubmitRoute::Rpc => {
            let rpc = task_context
                .rpc_transport
                .ok_or(SubmitError::MissingRpcTransport)?;
            let rpc_signature = rpc
                .submit_rpc(tx_bytes.as_ref(), &task_context.rpc_config)
                .await
                .map_err(|source| SubmitError::Rpc { source })?;
            Ok(RouteSubmitOutcome {
                route,
                direct_target: None,
                rpc_signature: Some(rpc_signature),
                jito_signature: None,
                jito_bundle_id: None,
                selected_target_count: 0,
                selected_identity_count: 0,
            })
        }
        SubmitRoute::Jito => {
            let jito = task_context
                .jito_transport
                .ok_or(SubmitError::MissingJitoTransport)?;
            let response = jito
                .submit_jito(tx_bytes.as_ref(), &task_context.jito_config)
                .await
                .map_err(|source| SubmitError::Jito { source })?;
            Ok(RouteSubmitOutcome {
                route,
                direct_target: None,
                rpc_signature: None,
                jito_signature: response.transaction_signature,
                jito_bundle_id: response.bundle_id,
                selected_target_count: 0,
                selected_identity_count: 0,
            })
        }
        SubmitRoute::Direct => {
            let direct = task_context
                .direct_transport
                .ok_or(SubmitError::MissingDirectTransport)?;
            let attempt_timeout = direct_attempt_timeout(&task_context.direct_config);
            let attempt_count = match direct_mode {
                DirectExecutionMode::Standalone => {
                    task_context.direct_config.direct_submit_attempts
                }
                DirectExecutionMode::Fallback => task_context.direct_config.hybrid_direct_attempts,
            };
            let mut last_error = None;
            for attempt_idx in 0..attempt_count {
                let mut targets = select_and_rank_targets(
                    task_context.leader_provider.as_ref(),
                    task_context.backups.as_ref(),
                    task_context.policy,
                    &task_context.direct_config,
                )
                .await;
                rotate_targets_for_attempt(&mut targets, attempt_idx, task_context.policy);
                let (selected_target_count, selected_identity_count) = summarize_targets(&targets);
                if targets.is_empty() {
                    if matches!(direct_mode, DirectExecutionMode::Fallback) {
                        break;
                    }
                    return Err(SubmitError::NoDirectTargets);
                }
                match timeout(
                    attempt_timeout,
                    direct.submit_direct(
                        tx_bytes.as_ref(),
                        &targets,
                        task_context.policy,
                        &task_context.direct_config,
                    ),
                )
                .await
                {
                    Ok(Ok(target)) => {
                        return Ok(RouteSubmitOutcome {
                            route,
                            direct_target: Some(target),
                            rpc_signature: None,
                            jito_signature: None,
                            jito_bundle_id: None,
                            selected_target_count,
                            selected_identity_count,
                        });
                    }
                    Ok(Err(source)) => last_error = Some(source),
                    Err(_elapsed) => {
                        last_error = Some(SubmitTransportError::Failure {
                            message: format!(
                                "direct submit attempt timed out after {}ms",
                                attempt_timeout.as_millis()
                            ),
                        });
                    }
                }
                if attempt_idx < attempt_count.saturating_sub(1) {
                    sleep(task_context.direct_config.rebroadcast_interval).await;
                }
            }
            Err(SubmitError::Direct {
                source: last_error.unwrap_or_else(|| SubmitTransportError::Failure {
                    message: "direct submit attempts exhausted".to_owned(),
                }),
            })
        }
    }
}

#[cfg(not(test))]
/// Replays successful direct submissions for a bounded Agave-like persistence window.
fn spawn_agave_rebroadcast_task(
    tx_bytes: Arc<[u8]>,
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
                    direct_transport.submit_direct(
                        tx_bytes.as_ref(),
                        &targets,
                        policy,
                        &direct_config,
                    ),
                )
                .await,
            );
        }
    });
}

#[cfg(test)]
/// Test-only stub that disables background rebroadcasting for deterministic assertions.
fn spawn_agave_rebroadcast_task(
    _tx_bytes: Arc<[u8]>,
    _direct_transport: Arc<dyn DirectSubmitTransport>,
    _leader_provider: Arc<dyn LeaderProvider>,
    _backups: Vec<LeaderTarget>,
    _policy: RoutingPolicy,
    _direct_config: DirectSubmitConfig,
) {
}

/// Selects routing targets and applies optional latency-aware ranking.
async fn select_and_rank_targets(
    leader_provider: &(impl LeaderProvider + ?Sized),
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

    let probe_timeout = direct_config.latency_probe_timeout;
    let probe_port = direct_config.latency_probe_port;
    let probe_count = targets
        .len()
        .min(direct_config.latency_probe_max_targets.max(1));
    let mut latencies = vec![None; probe_count];
    let mut probes = JoinSet::new();
    for (idx, target) in targets.iter().take(probe_count).cloned().enumerate() {
        probes.spawn(async move {
            (
                idx,
                probe_target_latency(&target, probe_port, probe_timeout).await,
            )
        });
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
    probe_port: Option<u16>,
    probe_timeout: Duration,
) -> Option<u128> {
    let mut ports = vec![target.tpu_addr.port()];
    if let Some(port) = probe_port
        && port != target.tpu_addr.port()
    {
        ports.push(port);
    }

    let ip = target.tpu_addr.ip();
    let mut best = None::<u128>;
    for port in ports {
        if let Some(latency) = probe_tcp_latency(ip, port, probe_timeout).await {
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

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct NoopOutcomeReporter;

    impl TxSubmitOutcomeReporter for NoopOutcomeReporter {
        fn record_outcome(&self, _outcome: &TxSubmitOutcome) {}
    }

    #[test]
    fn reporter_dispatcher_reuses_existing_instance() {
        let reporter: Arc<dyn TxSubmitOutcomeReporter> = Arc::new(NoopOutcomeReporter);
        let first = match OutcomeReporterDispatcher::shared(Arc::clone(&reporter)) {
            Ok(dispatcher) => dispatcher,
            Err(error) => panic!("first dispatcher failed: {error}"),
        };
        let second = match OutcomeReporterDispatcher::shared(reporter) {
            Ok(dispatcher) => dispatcher,
            Err(error) => panic!("second dispatcher failed: {error}"),
        };

        assert!(Arc::ptr_eq(&first, &second));
    }
}
