//! Plugin example that logs each newly discovered TPU leader with node metadata.
#![doc(hidden)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use sof::framework::{
    ClusterNodeInfo, ClusterTopologyEvent, LeaderScheduleEvent, Plugin, PluginHost,
};
use sof::runtime::RuntimeSetup;
use solana_pubkey::Pubkey;
use thiserror::Error;
use tokio::sync::Mutex;

#[derive(Debug, Default)]
struct TpuLeaderLoggerState {
    nodes: HashMap<Pubkey, ClusterNodeInfo>,
    seen_tpu_leaders: HashSet<Pubkey>,
    pending_leaders: HashSet<Pubkey>,
}

#[derive(Debug, Clone, Default)]
struct TpuLeaderLoggerPlugin {
    state: Arc<Mutex<TpuLeaderLoggerState>>,
}

#[async_trait]
impl Plugin for TpuLeaderLoggerPlugin {
    fn name(&self) -> &'static str {
        "tpu-leader-logger"
    }

    async fn on_cluster_topology(&self, event: ClusterTopologyEvent) {
        let mut state = self.state.lock().await;

        if !event.snapshot_nodes.is_empty() {
            state.nodes.clear();
            for node in &event.snapshot_nodes {
                let _ = state.nodes.insert(node.pubkey, node.clone());
            }
        } else {
            for pubkey in &event.removed_pubkeys {
                let _ = state.nodes.remove(pubkey);
            }
            for node in event.added_nodes.iter().chain(event.updated_nodes.iter()) {
                let _ = state.nodes.insert(node.pubkey, node.clone());
            }
        }

        let pending: Vec<Pubkey> = state.pending_leaders.iter().copied().collect();
        for leader in pending {
            if try_log_new_tpu_leader(
                &mut state,
                leader,
                event.slot.unwrap_or_default(),
                event.active_entrypoint.as_deref(),
            ) {
                let _ = state.pending_leaders.remove(&leader);
            }
        }
    }

    async fn on_leader_schedule(&self, event: LeaderScheduleEvent) {
        let mut state = self.state.lock().await;
        for assignment in event
            .added_leaders
            .iter()
            .chain(event.updated_leaders.iter())
            .chain(event.snapshot_leaders.iter())
        {
            if !try_log_new_tpu_leader(&mut state, assignment.leader, assignment.slot, None) {
                let _ = state.pending_leaders.insert(assignment.leader);
            }
        }
    }
}

fn try_log_new_tpu_leader(
    state: &mut TpuLeaderLoggerState,
    leader: Pubkey,
    slot: u64,
    active_entrypoint: Option<&str>,
) -> bool {
    if state.seen_tpu_leaders.contains(&leader) {
        return true;
    }
    let Some(node) = state.nodes.get(&leader) else {
        return false;
    };
    let Some(tpu_addr) = node.tpu else {
        return false;
    };

    tracing::info!(
        leader = %leader,
        slot,
        tpu_addr = %tpu_addr,
        tpu_ip = %tpu_addr.ip(),
        tpu_port = tpu_addr.port(),
        gossip_addr = ?node.gossip,
        tvu_addr = ?node.tvu,
        rpc_addr = ?node.rpc,
        wallclock = node.wallclock,
        shred_version = node.shred_version,
        active_entrypoint = active_entrypoint.unwrap_or(""),
        "new tpu leader discovered"
    );
    let _ = state.seen_tpu_leaders.insert(leader);
    true
}

#[derive(Debug, Error)]
enum TpuLeaderLoggerExampleError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error(transparent)]
    Runtime(#[from] sof::runtime::RuntimeError),
}

const fn require_release_mode() -> Result<(), TpuLeaderLoggerExampleError> {
    if cfg!(debug_assertions) {
        return Err(TpuLeaderLoggerExampleError::ReleaseModeRequired {
            command: "cargo run --release -p sof --example tpu_leader_logger --features gossip-bootstrap",
        });
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), TpuLeaderLoggerExampleError> {
    require_release_mode()?;
    let host = PluginHost::builder()
        .add_plugin(TpuLeaderLoggerPlugin::default())
        .build();
    let setup = RuntimeSetup::new()
        .with_repair_enabled(false)
        .with_verify_shreds(true);
    tracing::info!(plugins = ?host.plugin_names(), "starting SOF runtime with plugin host");
    Ok(sof::runtime::run_async_with_plugin_host_and_setup(host, &setup).await?)
}
