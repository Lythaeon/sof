//! Standalone profiler for generic plugin-hook dispatch latency.

use std::str::FromStr;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Instant;

use async_trait::async_trait;
use sof::event::ForkSlotStatus;
use sof::framework::{
    ObserverPlugin, PluginConfig, PluginDispatchMode, PluginHostBuilder,
    events::{
        ClusterTopologyEvent, ControlPlaneSource, DatasetEvent, ObservedRecentBlockhashEvent,
        SlotStatusEvent,
    },
};

/// Default iterations when not overridden by the environment.
const DEFAULT_ITERATIONS: usize = 50_000;

/// One generic hook family measured by this profiler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HookScenario {
    /// Dataset callback dispatched through the generic plugin worker.
    Dataset,
    /// Recent-blockhash callback dispatched through the generic plugin worker.
    RecentBlockhash,
    /// Slot-status callback dispatched through the generic plugin worker.
    SlotStatus,
    /// Cluster-topology callback dispatched through the generic plugin worker.
    ClusterTopology,
}

impl HookScenario {
    /// Stable profiler label.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Dataset => "dataset",
            Self::RecentBlockhash => "recent_blockhash",
            Self::SlotStatus => "slot_status",
            Self::ClusterTopology => "cluster_topology",
        }
    }

    /// Static plugin config needed to subscribe to this hook family.
    fn plugin_config(self) -> PluginConfig {
        match self {
            Self::Dataset => PluginConfig::new().with_dataset(),
            Self::RecentBlockhash => PluginConfig::new().with_recent_blockhash(),
            Self::SlotStatus => PluginConfig::new().with_slot_status(),
            Self::ClusterTopology => PluginConfig::new().with_cluster_topology(),
        }
    }
}

impl FromStr for HookScenario {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "dataset" => Ok(Self::Dataset),
            "recent_blockhash" => Ok(Self::RecentBlockhash),
            "slot_status" => Ok(Self::SlotStatus),
            "cluster_topology" => Ok(Self::ClusterTopology),
            _ => Err(format!(
                "unknown scenario {value:?}; expected dataset, recent_blockhash, slot_status, or cluster_topology",
            )),
        }
    }
}

/// One profiler result.
struct ProfileResult {
    /// Stable scenario label.
    scenario: &'static str,
    /// Stable dispatch-mode label.
    mode: &'static str,
    /// Plugin count used in the run.
    plugins: usize,
    /// Fixed-point nanoseconds per iteration scaled by 1000.
    ns_per_iteration_x1000: u128,
}

/// Counter-based plugin used to measure dispatch completion.
#[derive(Clone)]
struct CountingPlugin {
    /// Hook family this plugin subscribes to.
    scenario: HookScenario,
    /// Shared completion counter incremented by every callback.
    counter: Arc<AtomicUsize>,
}

#[async_trait]
impl ObserverPlugin for CountingPlugin {
    fn config(&self) -> PluginConfig {
        self.scenario.plugin_config()
    }

    async fn on_dataset(&self, _event: DatasetEvent) {
        self.counter.fetch_add(1, Ordering::Relaxed);
    }

    async fn on_recent_blockhash(&self, _event: ObservedRecentBlockhashEvent) {
        self.counter.fetch_add(1, Ordering::Relaxed);
    }

    async fn on_slot_status(&self, _event: SlotStatusEvent) {
        self.counter.fetch_add(1, Ordering::Relaxed);
    }

    async fn on_cluster_topology(&self, _event: ClusterTopologyEvent) {
        self.counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// Entry point for the generic plugin-dispatch profiler.
fn main() -> Result<(), String> {
    let iterations = std::env::var("SOF_PLUGIN_DISPATCH_PROFILE_ITERS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_ITERATIONS);
    let selected_scenario = std::env::args().nth(1);
    let scenarios = [
        HookScenario::Dataset,
        HookScenario::RecentBlockhash,
        HookScenario::SlotStatus,
        HookScenario::ClusterTopology,
    ];
    let modes = [
        (PluginDispatchMode::Sequential, "sequential", 1usize),
        (
            PluginDispatchMode::BoundedConcurrent(4),
            "bounded_concurrent",
            4usize,
        ),
    ];

    let mut matched = false;
    for scenario in scenarios {
        if let Some(selected) = selected_scenario.as_deref()
            && selected != scenario.as_str()
        {
            continue;
        }
        matched = true;
        for (mode, mode_name, plugin_count) in modes {
            let result = profile_scenario(scenario, mode, mode_name, plugin_count, iterations);
            let whole = result.ns_per_iteration_x1000 / 1000;
            let fraction = result.ns_per_iteration_x1000 % 1000;
            println!(
                "scenario={} mode={} plugins={} iterations={} ns_per_iteration={}.{:03}",
                result.scenario, result.mode, result.plugins, iterations, whole, fraction,
            );
        }
    }

    if !matched {
        return Err(format!(
            "unknown scenario {:?}; expected dataset, recent_blockhash, slot_status, or cluster_topology",
            selected_scenario,
        ));
    }
    Ok(())
}

/// Profiles one hook family and dispatch mode.
fn profile_scenario(
    scenario: HookScenario,
    mode: PluginDispatchMode,
    mode_name: &'static str,
    plugin_count: usize,
    iterations: usize,
) -> ProfileResult {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut builder = PluginHostBuilder::new().with_dispatch_mode(mode);
    for _ in 0..plugin_count {
        builder = builder.add_plugin(CountingPlugin {
            scenario,
            counter: Arc::clone(&counter),
        });
    }
    let host = builder.build();

    let started_at = Instant::now();
    for _ in 0..iterations {
        let expected = counter.load(Ordering::Relaxed).saturating_add(plugin_count);
        dispatch_scenario_event(&host, scenario, expected as u64);
        while counter.load(Ordering::Relaxed) < expected {
            std::hint::spin_loop();
        }
    }
    let ns_per_iteration_x1000 = started_at
        .elapsed()
        .as_nanos()
        .saturating_mul(1000)
        .checked_div(iterations as u128)
        .unwrap_or(0);

    ProfileResult {
        scenario: scenario.as_str(),
        mode: mode_name,
        plugins: plugin_count,
        ns_per_iteration_x1000,
    }
}

/// Dispatches one representative event for the requested hook family.
fn dispatch_scenario_event(host: &sof::framework::PluginHost, scenario: HookScenario, seed: u64) {
    match scenario {
        HookScenario::Dataset => host.on_dataset(DatasetEvent {
            slot: 91_000,
            start_index: 0,
            end_index: 31,
            last_in_slot: false,
            shreds: 32,
            payload_len: 4096,
            tx_count: 64,
        }),
        HookScenario::RecentBlockhash => host.on_recent_blockhash(ObservedRecentBlockhashEvent {
            slot: 91_000_u64.saturating_add(seed),
            recent_blockhash: synthetic_blockhash(seed),
            dataset_tx_count: 64,
        }),
        HookScenario::SlotStatus => host.on_slot_status(SlotStatusEvent {
            slot: 91_000_u64.saturating_add(seed),
            parent_slot: Some(90_999_u64.saturating_add(seed)),
            previous_status: Some(ForkSlotStatus::Processed),
            status: ForkSlotStatus::Confirmed,
            tip_slot: Some(91_000_u64.saturating_add(seed)),
            confirmed_slot: Some(91_000_u64.saturating_add(seed)),
            finalized_slot: Some(90_992_u64.saturating_add(seed)),
        }),
        HookScenario::ClusterTopology => host.on_cluster_topology(ClusterTopologyEvent {
            source: ControlPlaneSource::GossipBootstrap,
            slot: Some(91_000_u64.saturating_add(seed)),
            epoch: Some(777),
            active_entrypoint: Some("127.0.0.1:8001".to_owned()),
            total_nodes: 0,
            added_nodes: vec![],
            removed_pubkeys: vec![],
            updated_nodes: vec![],
            snapshot_nodes: vec![],
        }),
    }
}

/// Builds a deterministic 32-byte blockhash-like payload from one seed.
fn synthetic_blockhash(seed: u64) -> [u8; 32] {
    let mut blockhash = [0_u8; 32];
    for chunk in blockhash.chunks_exact_mut(8) {
        chunk.copy_from_slice(&seed.to_le_bytes());
    }
    blockhash
}
