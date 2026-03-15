//! High-level builder for configuring `TxSubmitClient`.

use std::sync::Arc;

use super::{
    DirectSubmitConfig, DirectSubmitTransport, JitoJsonRpcTransport, JitoSubmitConfig,
    JitoSubmitTransport, JsonRpcTransport, RpcSubmitConfig, RpcSubmitTransport, SubmitReliability,
    SubmitTransportError, TxFlowSafetySource, TxSubmitClient, TxSubmitGuardPolicy,
    TxSubmitOutcomeReporter, UdpDirectTransport,
};
use crate::{
    providers::{
        LeaderProvider, RecentBlockhashProvider, RpcRecentBlockhashProvider, StaticLeaderProvider,
        StaticRecentBlockhashProvider,
    },
    routing::RoutingPolicy,
};

/// High-level builder for common `TxSubmitClient` configurations.
pub struct TxSubmitClientBuilder {
    /// Source used by unsigned submit paths.
    blockhash_provider: Arc<dyn RecentBlockhashProvider>,
    /// Optional RPC-backed blockhash source refreshed on demand.
    on_demand_blockhash_provider: Option<Arc<RpcRecentBlockhashProvider>>,
    /// Source used by direct and hybrid paths.
    leader_provider: Arc<dyn LeaderProvider>,
    /// Optional backup validators.
    backups: Vec<crate::providers::LeaderTarget>,
    /// Direct routing policy.
    policy: RoutingPolicy,
    /// Optional RPC submit transport.
    rpc_transport: Option<Arc<dyn RpcSubmitTransport>>,
    /// Optional Jito submit transport.
    jito_transport: Option<Arc<dyn JitoSubmitTransport>>,
    /// Optional direct submit transport.
    direct_transport: Option<Arc<dyn DirectSubmitTransport>>,
    /// RPC submit tuning.
    rpc_config: RpcSubmitConfig,
    /// Jito submit tuning.
    jito_config: JitoSubmitConfig,
    /// Direct submit tuning.
    direct_config: DirectSubmitConfig,
    /// Optional direct/hybrid reliability profile.
    reliability: Option<SubmitReliability>,
    /// Optional toxic-flow guard source.
    flow_safety_source: Option<Arc<dyn TxFlowSafetySource>>,
    /// Toxic-flow guard policy.
    guard_policy: TxSubmitGuardPolicy,
    /// Optional external outcome reporter.
    outcome_reporter: Option<Arc<dyn TxSubmitOutcomeReporter>>,
}

impl Default for TxSubmitClientBuilder {
    fn default() -> Self {
        Self {
            blockhash_provider: Arc::new(StaticRecentBlockhashProvider::new(None)),
            on_demand_blockhash_provider: None,
            leader_provider: Arc::new(StaticLeaderProvider::default()),
            backups: Vec::new(),
            policy: RoutingPolicy::default(),
            rpc_transport: None,
            jito_transport: None,
            direct_transport: None,
            rpc_config: RpcSubmitConfig::default(),
            jito_config: JitoSubmitConfig::default(),
            direct_config: DirectSubmitConfig::default(),
            reliability: None,
            flow_safety_source: None,
            guard_policy: TxSubmitGuardPolicy::default(),
            outcome_reporter: None,
        }
    }
}

impl TxSubmitClientBuilder {
    /// Creates a new builder with empty static providers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the explicit blockhash provider.
    #[must_use]
    pub fn with_blockhash_provider(mut self, provider: Arc<dyn RecentBlockhashProvider>) -> Self {
        self.blockhash_provider = provider;
        self.on_demand_blockhash_provider = None;
        self
    }

    /// Uses one RPC endpoint as an on-demand blockhash source for unsigned submits.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError`] when the RPC-backed provider cannot be created.
    pub fn with_blockhash_via_rpc(
        mut self,
        rpc_url: impl Into<String>,
    ) -> Result<Self, SubmitTransportError> {
        let provider = Arc::new(RpcRecentBlockhashProvider::new(rpc_url.into())?);
        self.blockhash_provider = provider.clone();
        self.on_demand_blockhash_provider = Some(provider);
        Ok(self)
    }

    /// Sets the explicit leader provider.
    #[must_use]
    pub fn with_leader_provider(mut self, provider: Arc<dyn LeaderProvider>) -> Self {
        self.leader_provider = provider;
        self
    }

    /// Resets the leader source to an empty static provider.
    #[must_use]
    pub fn without_leaders(mut self) -> Self {
        self.leader_provider = Arc::new(StaticLeaderProvider::default());
        self
    }

    /// Sets optional backup validators.
    #[must_use]
    pub fn with_backups(mut self, backups: Vec<crate::providers::LeaderTarget>) -> Self {
        self.backups = backups;
        self
    }

    /// Sets the direct routing policy.
    #[must_use]
    pub const fn with_routing_policy(mut self, policy: RoutingPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Uses one RPC endpoint for both on-demand blockhash refresh and RPC submission.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError`] when the blockhash provider or RPC transport cannot be
    /// created.
    pub fn with_rpc_defaults(
        self,
        rpc_url: impl Into<String>,
    ) -> Result<Self, SubmitTransportError> {
        let rpc_url = rpc_url.into();
        let transport = Arc::new(JsonRpcTransport::new(rpc_url.clone())?);
        Ok(self
            .with_blockhash_via_rpc(rpc_url)?
            .with_rpc_transport(transport))
    }

    /// Sets the explicit RPC submit transport.
    #[must_use]
    pub fn with_rpc_transport(mut self, transport: Arc<dyn RpcSubmitTransport>) -> Self {
        self.rpc_transport = Some(transport);
        self
    }

    /// Uses the default Jito JSON-RPC transport and one RPC endpoint for on-demand blockhashes.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError`] when the blockhash provider or Jito transport cannot be
    /// created.
    pub fn with_jito_defaults(
        self,
        rpc_url: impl Into<String>,
    ) -> Result<Self, SubmitTransportError> {
        Ok(self
            .with_blockhash_via_rpc(rpc_url)?
            .with_jito_transport(Arc::new(JitoJsonRpcTransport::new()?)))
    }

    /// Sets the explicit Jito submit transport.
    #[must_use]
    pub fn with_jito_transport(mut self, transport: Arc<dyn JitoSubmitTransport>) -> Self {
        self.jito_transport = Some(transport);
        self
    }

    /// Uses the default UDP direct transport.
    #[must_use]
    pub fn with_direct_udp(mut self) -> Self {
        self.direct_transport = Some(Arc::new(UdpDirectTransport::new()));
        self
    }

    /// Sets the explicit direct submit transport.
    #[must_use]
    pub fn with_direct_transport(mut self, transport: Arc<dyn DirectSubmitTransport>) -> Self {
        self.direct_transport = Some(transport);
        self
    }

    /// Sets the RPC submit tuning.
    #[must_use]
    pub fn with_rpc_config(mut self, config: RpcSubmitConfig) -> Self {
        self.rpc_config = config;
        self
    }

    /// Sets the Jito submit tuning.
    #[must_use]
    pub const fn with_jito_config(mut self, config: JitoSubmitConfig) -> Self {
        self.jito_config = config;
        self
    }

    /// Sets the direct submit tuning.
    #[must_use]
    pub const fn with_direct_config(mut self, config: DirectSubmitConfig) -> Self {
        self.direct_config = config;
        self.reliability = None;
        self
    }

    /// Applies a direct/hybrid reliability preset.
    #[must_use]
    pub const fn with_reliability(mut self, reliability: SubmitReliability) -> Self {
        self.reliability = Some(reliability);
        self
    }

    /// Sets the toxic-flow guard source.
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
        self.outcome_reporter = Some(reporter);
        self
    }

    /// Builds the configured `TxSubmitClient`.
    #[must_use]
    pub fn build(self) -> TxSubmitClient {
        let mut client = TxSubmitClient::new(self.blockhash_provider, self.leader_provider)
            .with_backups(self.backups)
            .with_routing_policy(self.policy)
            .with_rpc_config(self.rpc_config)
            .with_jito_config(self.jito_config)
            .with_guard_policy(self.guard_policy);
        if let Some(provider) = self.on_demand_blockhash_provider {
            client = client.with_rpc_blockhash_provider(provider);
        }
        if let Some(reliability) = self.reliability {
            client = client.with_reliability(reliability);
        } else {
            client = client.with_direct_config(self.direct_config);
        }
        if let Some(transport) = self.rpc_transport {
            client = client.with_rpc_transport(transport);
        }
        if let Some(transport) = self.jito_transport {
            client = client.with_jito_transport(transport);
        }
        if let Some(transport) = self.direct_transport {
            client = client.with_direct_transport(transport);
        }
        if let Some(source) = self.flow_safety_source {
            client = client.with_flow_safety_source(source);
        }
        if let Some(reporter) = self.outcome_reporter {
            client = client.with_outcome_reporter(reporter);
        }
        client
    }
}
