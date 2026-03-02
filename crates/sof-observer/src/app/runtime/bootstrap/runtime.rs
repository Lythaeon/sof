use super::*;

pub(in crate::app::runtime) struct ReceiverRuntime {
    pub(in crate::app::runtime) static_receiver_handles: Vec<JoinHandle<()>>,
    pub(in crate::app::runtime) gossip_receiver_handles: Vec<JoinHandle<()>>,
    #[cfg(feature = "gossip-bootstrap")]
    pub(in crate::app::runtime) gossip_runtime: Option<GossipRuntime>,
    #[cfg(feature = "gossip-bootstrap")]
    pub(in crate::app::runtime) gossip_identity: Arc<Keypair>,
    #[cfg(feature = "gossip-bootstrap")]
    pub(in crate::app::runtime) active_gossip_entrypoint: Option<String>,
    #[cfg(feature = "gossip-bootstrap")]
    pub(in crate::app::runtime) gossip_runtime_primary_port_range: Option<PortRange>,
    #[cfg(feature = "gossip-bootstrap")]
    pub(in crate::app::runtime) gossip_runtime_secondary_port_range: Option<PortRange>,
    #[cfg(feature = "gossip-bootstrap")]
    pub(in crate::app::runtime) gossip_runtime_active_port_range: Option<PortRange>,
    #[cfg(feature = "gossip-bootstrap")]
    pub(in crate::app::runtime) repair_client: Option<crate::repair::GossipRepairClient>,
    pub(in crate::app::runtime) tx_event_rx: mpsc::Receiver<TxObservedEvent>,
}

#[cfg(feature = "gossip-bootstrap")]
pub(in crate::app::runtime) struct GossipRuntime {
    pub(in crate::app::runtime) exit: Arc<AtomicBool>,
    pub(in crate::app::runtime) gossip_service: Option<GossipService>,
    pub(in crate::app::runtime) cluster_info: Arc<ClusterInfo>,
    pub(in crate::app::runtime) ingest_telemetry: ingest::ReceiverTelemetry,
}

#[cfg(feature = "gossip-bootstrap")]
impl Drop for GossipRuntime {
    fn drop(&mut self) {
        self.exit.store(true, Ordering::Relaxed);
        if let Some(gossip_service) = self.gossip_service.take()
            && gossip_service.join().is_err()
        {
            // Gossip service already terminated.
        }
    }
}

impl Drop for ReceiverRuntime {
    fn drop(&mut self) {
        for handle in &self.static_receiver_handles {
            handle.abort();
        }
        for handle in &self.gossip_receiver_handles {
            handle.abort();
        }
    }
}

#[cfg(feature = "gossip-bootstrap")]
impl ReceiverRuntime {
    pub(in crate::app::runtime) fn replace_gossip_runtime(
        &mut self,
        receiver_handles: Vec<JoinHandle<()>>,
        runtime: GossipRuntime,
        repair_client: Option<crate::repair::GossipRepairClient>,
        active_entrypoint: Option<String>,
        active_port_range: Option<PortRange>,
    ) {
        for handle in &self.gossip_receiver_handles {
            handle.abort();
        }
        self.gossip_receiver_handles = receiver_handles;
        self.gossip_runtime = Some(runtime);
        self.repair_client = repair_client;
        self.active_gossip_entrypoint = active_entrypoint;
        self.gossip_runtime_active_port_range = active_port_range;
    }

    pub(in crate::app::runtime) async fn stop_gossip_runtime(&mut self) {
        let mut handles = Vec::new();
        handles.append(&mut self.gossip_receiver_handles);
        for handle in handles {
            handle.abort();
            if handle.await.is_err() {
                // Receiver task was already aborted/cancelled.
            }
        }
        self.gossip_runtime = None;
        self.repair_client = None;
        self.active_gossip_entrypoint = None;
        self.gossip_runtime_active_port_range = None;
    }
}
