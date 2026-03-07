//! `sof` plugin adapter that bridges runtime observations into `sof-tx` providers.

use std::net::SocketAddr;

use async_trait::async_trait;
use sof::framework::{
    ClusterTopologyEvent, LeaderScheduleEvent, ObservedRecentBlockhashEvent, ObserverPlugin,
    PluginHost,
};
use solana_pubkey::Pubkey;

use crate::{
    adapters::common::{
        TxProviderAdapterConfig, TxProviderAdapterCore, TxProviderControlPlaneSnapshot,
        TxProviderFlowSafetyPolicy, TxProviderFlowSafetyReport, take_next_leader_identity_targets,
    },
    providers::{LeaderProvider, LeaderTarget, RecentBlockhashProvider},
};

/// Configuration for [`PluginHostTxProviderAdapter`].
pub type PluginHostTxProviderAdapterConfig = TxProviderAdapterConfig;

/// Shared adapter that can be registered as a SOF plugin and used as tx providers.
///
/// This type ingests SOF control-plane hooks (`on_recent_blockhash`, `on_cluster_topology`,
/// `on_leader_schedule`) and exposes state through [`RecentBlockhashProvider`] and
/// [`LeaderProvider`].
#[derive(Debug, Clone)]
pub struct PluginHostTxProviderAdapter {
    /// Shared tx-provider state and reduction logic.
    core: TxProviderAdapterCore,
}

impl PluginHostTxProviderAdapter {
    /// Creates a new adapter with the provided config.
    #[must_use]
    pub fn new(config: PluginHostTxProviderAdapterConfig) -> Self {
        Self {
            core: TxProviderAdapterCore::new(config),
        }
    }

    /// Seeds adapter state from already-observed values in `PluginHost`.
    ///
    /// This is useful when attaching the adapter after runtime state has already started
    /// accumulating.
    pub fn prime_from_plugin_host(&self, host: &mut PluginHost) {
        self.core.prime_from_plugin_host(host);
    }

    /// Inserts or updates one TPU address mapping for a leader identity.
    pub fn set_leader_tpu_addr(&self, identity: Pubkey, tpu_addr: SocketAddr) {
        self.core.set_leader_tpu_addr(identity, tpu_addr);
    }

    /// Removes one TPU address mapping for a leader identity.
    pub fn remove_leader_tpu_addr(&self, identity: Pubkey) {
        self.core.remove_leader_tpu_addr(identity);
    }

    /// Returns current leader plus up to `next_leaders` future leaders, in slot order.
    #[must_use]
    fn leader_window(&self, next_leaders: usize) -> Vec<LeaderTarget> {
        self.core.leader_window(next_leaders)
    }

    /// Returns the current control-plane freshness snapshot.
    #[must_use]
    pub fn control_plane_snapshot(&self) -> TxProviderControlPlaneSnapshot {
        self.core.control_plane_snapshot()
    }

    /// Evaluates the current control-plane state against one flow-safety policy.
    #[must_use]
    pub fn evaluate_flow_safety(
        &self,
        policy: TxProviderFlowSafetyPolicy,
    ) -> TxProviderFlowSafetyReport {
        self.core.evaluate_flow_safety(policy)
    }

    /// Flushes all currently queued updates.
    #[cfg(test)]
    pub async fn flush(&self) {}
}

impl Default for PluginHostTxProviderAdapter {
    fn default() -> Self {
        Self::new(PluginHostTxProviderAdapterConfig::default())
    }
}

impl RecentBlockhashProvider for PluginHostTxProviderAdapter {
    fn latest_blockhash(&self) -> Option<[u8; 32]> {
        self.core.latest_blockhash()
    }
}

impl LeaderProvider for PluginHostTxProviderAdapter {
    fn current_leader(&self) -> Option<LeaderTarget> {
        self.leader_window(0).into_iter().next()
    }

    fn next_leaders(&self, n: usize) -> Vec<LeaderTarget> {
        if n == 0 {
            return Vec::new();
        }
        take_next_leader_identity_targets(self.leader_window(n), n)
    }
}

#[async_trait]
impl ObserverPlugin for PluginHostTxProviderAdapter {
    fn name(&self) -> &'static str {
        "sof-tx-provider-adapter"
    }

    fn wants_recent_blockhash(&self) -> bool {
        true
    }

    fn wants_cluster_topology(&self) -> bool {
        true
    }

    fn wants_leader_schedule(&self) -> bool {
        true
    }

    async fn on_recent_blockhash(&self, event: ObservedRecentBlockhashEvent) {
        self.core.apply_recent_blockhash(&event);
    }

    async fn on_cluster_topology(&self, event: ClusterTopologyEvent) {
        self.core.apply_cluster_topology(&event);
    }

    async fn on_leader_schedule(&self, event: LeaderScheduleEvent) {
        self.core.apply_leader_schedule(&event);
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;
    use sof::framework::{ClusterNodeInfo, ControlPlaneSource, LeaderScheduleEntry, PluginHost};

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn node(pubkey: Pubkey, tpu_port: u16) -> ClusterNodeInfo {
        ClusterNodeInfo {
            pubkey,
            wallclock: 0,
            shred_version: 0,
            gossip: None,
            tpu: Some(addr(tpu_port)),
            tpu_quic: None,
            tpu_forwards: None,
            tpu_forwards_quic: None,
            tpu_vote: None,
            tvu: None,
            rpc: None,
        }
    }

    fn node_with_forwards(
        pubkey: Pubkey,
        tpu_port: u16,
        tpu_forwards_port: u16,
    ) -> ClusterNodeInfo {
        ClusterNodeInfo {
            pubkey,
            wallclock: 0,
            shred_version: 0,
            gossip: None,
            tpu: Some(addr(tpu_port)),
            tpu_quic: None,
            tpu_forwards: Some(addr(tpu_forwards_port)),
            tpu_forwards_quic: None,
            tpu_vote: None,
            tvu: None,
            rpc: None,
        }
    }

    fn topology_snapshot(nodes: Vec<ClusterNodeInfo>) -> ClusterTopologyEvent {
        ClusterTopologyEvent {
            source: ControlPlaneSource::Direct,
            slot: Some(100),
            epoch: None,
            active_entrypoint: None,
            total_nodes: nodes.len(),
            added_nodes: Vec::new(),
            removed_pubkeys: Vec::new(),
            updated_nodes: Vec::new(),
            snapshot_nodes: nodes,
        }
    }

    fn leader_snapshot(
        slot: u64,
        snapshot_leaders: Vec<LeaderScheduleEntry>,
    ) -> LeaderScheduleEvent {
        LeaderScheduleEvent {
            source: ControlPlaneSource::Direct,
            slot: Some(slot),
            epoch: None,
            added_leaders: Vec::new(),
            removed_slots: Vec::new(),
            updated_leaders: Vec::new(),
            snapshot_leaders,
        }
    }

    #[tokio::test]
    async fn adapter_opt_in_flags_are_enabled() {
        let adapter = PluginHostTxProviderAdapter::default();
        assert!(adapter.wants_recent_blockhash());
        assert!(adapter.wants_cluster_topology());
        assert!(adapter.wants_leader_schedule());
    }

    #[tokio::test]
    async fn adapter_updates_recent_blockhash_provider() {
        let adapter = PluginHostTxProviderAdapter::default();
        assert_eq!(adapter.latest_blockhash(), None);

        adapter
            .on_recent_blockhash(ObservedRecentBlockhashEvent {
                slot: 10,
                recent_blockhash: [7_u8; 32],
                dataset_tx_count: 1,
            })
            .await;

        assert_eq!(adapter.latest_blockhash(), Some([7_u8; 32]));
    }

    #[tokio::test]
    async fn adapter_maps_topology_and_leaders_into_targets() {
        let adapter = PluginHostTxProviderAdapter::default();
        let leader_a = Pubkey::new_unique();
        let leader_b = Pubkey::new_unique();
        let leader_c = Pubkey::new_unique();

        adapter
            .on_cluster_topology(topology_snapshot(vec![
                node(leader_a, 9001),
                node(leader_b, 9002),
                node(leader_c, 9003),
            ]))
            .await;
        adapter
            .on_leader_schedule(leader_snapshot(
                100,
                vec![
                    LeaderScheduleEntry {
                        slot: 100,
                        leader: leader_a,
                    },
                    LeaderScheduleEntry {
                        slot: 101,
                        leader: leader_b,
                    },
                    LeaderScheduleEntry {
                        slot: 102,
                        leader: leader_c,
                    },
                ],
            ))
            .await;

        let current = adapter.current_leader();
        assert_eq!(current, Some(LeaderTarget::new(Some(leader_a), addr(9007))));

        let next = adapter.next_leaders(2);
        let expected_b = LeaderTarget::new(Some(leader_b), addr(9008));
        let expected_c = LeaderTarget::new(Some(leader_c), addr(9009));
        assert_eq!(next.first(), Some(&expected_b));
        assert!(next.contains(&expected_c));
    }

    #[tokio::test]
    async fn adapter_falls_back_to_topology_when_schedule_is_unmapped() {
        let adapter = PluginHostTxProviderAdapter::default();
        let unmapped_leader = Pubkey::new_unique();
        let topo_a = Pubkey::new_unique();
        let topo_b = Pubkey::new_unique();

        adapter
            .on_cluster_topology(topology_snapshot(vec![
                node(topo_b, 9122),
                node(topo_a, 9121),
            ]))
            .await;
        adapter
            .on_leader_schedule(leader_snapshot(
                100,
                vec![LeaderScheduleEntry {
                    slot: 100,
                    leader: unmapped_leader,
                }],
            ))
            .await;

        let current = adapter.current_leader();
        assert_eq!(current, Some(LeaderTarget::new(Some(topo_a), addr(9127))));

        let next = adapter.next_leaders(1);
        assert_eq!(
            next,
            vec![
                LeaderTarget::new(Some(topo_b), addr(9128)),
                LeaderTarget::new(Some(topo_b), addr(9122)),
            ]
        );
    }

    #[tokio::test]
    async fn adapter_next_leaders_skip_current_identity_and_return_next_identity() {
        let adapter = PluginHostTxProviderAdapter::default();
        let leader_a = Pubkey::new_unique();
        let leader_b = Pubkey::new_unique();

        adapter
            .on_cluster_topology(topology_snapshot(vec![
                node_with_forwards(leader_a, 9041, 9042),
                node(leader_b, 9043),
            ]))
            .await;
        adapter
            .on_leader_schedule(leader_snapshot(
                100,
                vec![
                    LeaderScheduleEntry {
                        slot: 100,
                        leader: leader_a,
                    },
                    LeaderScheduleEntry {
                        slot: 101,
                        leader: leader_b,
                    },
                ],
            ))
            .await;

        let current = adapter.current_leader();
        assert_eq!(current, Some(LeaderTarget::new(Some(leader_a), addr(9047))));

        let next = adapter.next_leaders(1);
        assert_eq!(
            next.first(),
            Some(&LeaderTarget::new(Some(leader_b), addr(9049)))
        );
    }

    #[tokio::test]
    async fn adapter_retains_bounded_leader_slots() {
        let adapter = PluginHostTxProviderAdapter::new(PluginHostTxProviderAdapterConfig {
            max_leader_slots: 2,
            max_next_leaders: 8,
        });
        let leader_a = Pubkey::new_unique();
        let leader_b = Pubkey::new_unique();
        let leader_c = Pubkey::new_unique();

        adapter.set_leader_tpu_addr(leader_a, addr(9011));
        adapter.set_leader_tpu_addr(leader_b, addr(9012));
        adapter.set_leader_tpu_addr(leader_c, addr(9013));
        adapter.flush().await;

        adapter
            .on_leader_schedule(leader_snapshot(
                22,
                vec![
                    LeaderScheduleEntry {
                        slot: 20,
                        leader: leader_a,
                    },
                    LeaderScheduleEntry {
                        slot: 21,
                        leader: leader_b,
                    },
                    LeaderScheduleEntry {
                        slot: 22,
                        leader: leader_c,
                    },
                ],
            ))
            .await;

        let current = adapter.current_leader();
        assert_eq!(current, Some(LeaderTarget::new(Some(leader_c), addr(9019))));

        let next = adapter.next_leaders(1);
        assert_eq!(
            next.first(),
            Some(&LeaderTarget::new(Some(leader_b), addr(9018)))
        );
    }

    #[tokio::test]
    async fn adapter_can_be_primed_from_plugin_host_state() {
        let mut host = PluginHost::builder().build();
        let leader = Pubkey::new_unique();
        host.on_recent_blockhash(ObservedRecentBlockhashEvent {
            slot: 42,
            recent_blockhash: [11_u8; 32],
            dataset_tx_count: 3,
        });
        host.on_leader_schedule(LeaderScheduleEvent {
            source: ControlPlaneSource::Direct,
            slot: Some(42),
            epoch: None,
            added_leaders: vec![LeaderScheduleEntry { slot: 42, leader }],
            removed_slots: Vec::new(),
            updated_leaders: Vec::new(),
            snapshot_leaders: Vec::new(),
        });

        let adapter = PluginHostTxProviderAdapter::default();
        adapter.prime_from_plugin_host(&mut host);
        adapter.flush().await;
        assert_eq!(adapter.latest_blockhash(), Some([11_u8; 32]));
        assert_eq!(adapter.current_leader(), None);

        adapter.set_leader_tpu_addr(leader, addr(9021));
        adapter.flush().await;
        assert_eq!(
            adapter.current_leader(),
            Some(LeaderTarget::new(Some(leader), addr(9027)))
        );
    }

    #[tokio::test]
    async fn adapter_reports_stale_control_plane_state() {
        let adapter = PluginHostTxProviderAdapter::default();
        let leader = Pubkey::new_unique();

        adapter
            .on_recent_blockhash(ObservedRecentBlockhashEvent {
                slot: 10,
                recent_blockhash: [9_u8; 32],
                dataset_tx_count: 1,
            })
            .await;
        adapter
            .on_cluster_topology(topology_snapshot(vec![node(leader, 9331)]))
            .await;
        adapter
            .on_leader_schedule(leader_snapshot(
                200,
                vec![LeaderScheduleEntry { slot: 200, leader }],
            ))
            .await;
        adapter
            .core
            .apply_slot_status(sof::framework::SlotStatusChangedEvent {
                slot: 200,
                parent_slot: Some(199),
                previous_status: Some(sof::event::ForkSlotStatus::Processed),
                status: sof::event::ForkSlotStatus::Confirmed,
            });

        let report = adapter.evaluate_flow_safety(TxProviderFlowSafetyPolicy {
            max_recent_blockhash_slot_lag: Some(16),
            ..TxProviderFlowSafetyPolicy::default()
        });

        assert!(!report.is_safe());
        assert!(report.issues.contains(
            &crate::adapters::TxProviderFlowSafetyIssue::StaleRecentBlockhash {
                slot_lag: 190,
                max_allowed: 16,
            }
        ));
    }
}
