use super::*;
use async_trait::async_trait;
use std::{
    sync::atomic::AtomicUsize,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
struct PluginA;

#[async_trait]
impl ObserverPlugin for PluginA {
    fn name(&self) -> &'static str {
        "plugin-a"
    }
}

#[derive(Clone, Copy)]
struct PluginB;

#[async_trait]
impl ObserverPlugin for PluginB {
    fn name(&self) -> &'static str {
        "plugin-b"
    }
}

#[test]
fn builder_registers_multiple_plugins() {
    let host = PluginHostBuilder::new()
        .with_plugin(PluginA)
        .with_plugin(PluginB)
        .build();
    assert_eq!(host.plugin_names(), vec!["plugin-a", "plugin-b"]);
}

#[test]
fn builder_registers_multiple_plugins_from_iterator() {
    let host = PluginHostBuilder::new()
        .with_plugins([PluginA, PluginA])
        .with_plugin(PluginB)
        .build();
    assert_eq!(host.len(), 3);
}

#[derive(Clone, Copy)]
struct PanicPlugin;

#[async_trait]
impl ObserverPlugin for PanicPlugin {
    fn name(&self) -> &'static str {
        "panic-plugin"
    }

    async fn on_dataset(&self, _event: DatasetEvent) {
        panic!("panic-plugin failed");
    }
}

#[derive(Clone)]
struct CounterPlugin {
    counter: Arc<AtomicUsize>,
}

#[async_trait]
impl ObserverPlugin for CounterPlugin {
    fn name(&self) -> &'static str {
        "counter-plugin"
    }

    async fn on_dataset(&self, _event: DatasetEvent) {
        self.counter.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct RecentBlockhashCounterPlugin {
    counter: Arc<AtomicUsize>,
}

#[async_trait]
impl ObserverPlugin for RecentBlockhashCounterPlugin {
    fn name(&self) -> &'static str {
        "recent-blockhash-counter-plugin"
    }

    async fn on_recent_blockhash(&self, _event: ObservedRecentBlockhashEvent) {
        self.counter.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn dispatch_continues_after_plugin_panic() {
    let counter = Arc::new(AtomicUsize::new(0));
    let host = PluginHostBuilder::new()
        .add_plugin(PanicPlugin)
        .add_plugin(CounterPlugin {
            counter: Arc::clone(&counter),
        })
        .build();
    host.on_dataset(DatasetEvent {
        slot: 1,
        start_index: 0,
        end_index: 0,
        last_in_slot: false,
        shreds: 1,
        payload_len: 1,
        tx_count: 0,
    });
    assert!(wait_until_counter(
        counter.as_ref(),
        1,
        Duration::from_secs(2)
    ));
}

#[test]
fn concurrent_multi_plugin_dispatch_is_consistent() {
    let counter_a = Arc::new(AtomicUsize::new(0));
    let counter_b = Arc::new(AtomicUsize::new(0));
    let threads = 8_usize;
    let iterations = 2_000_usize;
    let expected = threads.saturating_mul(iterations);
    let host = Arc::new(
        PluginHostBuilder::new()
            .with_event_queue_capacity(expected.saturating_mul(2))
            .add_plugin(CounterPlugin {
                counter: counter_a.clone(),
            })
            .add_plugin(CounterPlugin {
                counter: counter_b.clone(),
            })
            .build(),
    );
    let mut joins = Vec::with_capacity(threads);
    for _ in 0..threads {
        let worker_host = host.clone();
        joins.push(thread::spawn(move || {
            for _ in 0..iterations {
                worker_host.on_dataset(DatasetEvent {
                    slot: 9,
                    start_index: 0,
                    end_index: 0,
                    last_in_slot: false,
                    shreds: 1,
                    payload_len: 1,
                    tx_count: 1,
                });
            }
        }));
    }
    for join in joins {
        assert!(join.join().is_ok());
    }
    assert!(wait_until_counter(
        counter_a.as_ref(),
        expected,
        Duration::from_secs(5)
    ));
    assert!(wait_until_counter(
        counter_b.as_ref(),
        expected,
        Duration::from_secs(5)
    ));
    assert_eq!(host.dropped_event_count(), 0);
}

#[test]
fn bounded_concurrent_dispatch_mode_is_consistent() {
    let counter_a = Arc::new(AtomicUsize::new(0));
    let counter_b = Arc::new(AtomicUsize::new(0));
    let threads = 8_usize;
    let iterations = 2_000_usize;
    let expected = threads.saturating_mul(iterations);
    let host = Arc::new(
        PluginHostBuilder::new()
            .with_event_queue_capacity(expected.saturating_mul(2))
            .with_dispatch_mode(PluginDispatchMode::BoundedConcurrent(8))
            .add_plugin(CounterPlugin {
                counter: counter_a.clone(),
            })
            .add_plugin(CounterPlugin {
                counter: counter_b.clone(),
            })
            .build(),
    );
    let mut joins = Vec::with_capacity(threads);
    for _ in 0..threads {
        let worker_host = host.clone();
        joins.push(thread::spawn(move || {
            for _ in 0..iterations {
                worker_host.on_dataset(DatasetEvent {
                    slot: 9,
                    start_index: 0,
                    end_index: 0,
                    last_in_slot: false,
                    shreds: 1,
                    payload_len: 1,
                    tx_count: 1,
                });
            }
        }));
    }
    for join in joins {
        assert!(join.join().is_ok());
    }
    assert!(wait_until_counter(
        counter_a.as_ref(),
        expected,
        Duration::from_secs(5)
    ));
    assert!(wait_until_counter(
        counter_b.as_ref(),
        expected,
        Duration::from_secs(5)
    ));
    assert_eq!(host.dropped_event_count(), 0);
}

#[test]
fn recent_blockhash_hook_is_deduplicated_and_stateful() {
    let counter = Arc::new(AtomicUsize::new(0));
    let host = PluginHostBuilder::new()
        .add_plugin(RecentBlockhashCounterPlugin {
            counter: Arc::clone(&counter),
        })
        .build();

    host.on_recent_blockhash(ObservedRecentBlockhashEvent {
        slot: 10,
        recent_blockhash: [1_u8; 32],
        dataset_tx_count: 8,
    });
    host.on_recent_blockhash(ObservedRecentBlockhashEvent {
        slot: 10,
        recent_blockhash: [1_u8; 32],
        dataset_tx_count: 8,
    });
    host.on_recent_blockhash(ObservedRecentBlockhashEvent {
        slot: 11,
        recent_blockhash: [1_u8; 32],
        dataset_tx_count: 9,
    });
    host.on_recent_blockhash(ObservedRecentBlockhashEvent {
        slot: 11,
        recent_blockhash: [2_u8; 32],
        dataset_tx_count: 9,
    });
    host.on_recent_blockhash(ObservedRecentBlockhashEvent {
        slot: 9,
        recent_blockhash: [3_u8; 32],
        dataset_tx_count: 4,
    });

    assert!(wait_until_counter(
        counter.as_ref(),
        2,
        Duration::from_secs(2)
    ));
    assert_eq!(
        host.latest_observed_recent_blockhash(),
        Some((11, [2_u8; 32]))
    );
}

#[test]
fn latest_observed_tpu_leader_is_stateful() {
    let host = PluginHostBuilder::new().build();
    let leader_a = Pubkey::new_from_array([7_u8; 32]);
    let leader_b = Pubkey::new_from_array([8_u8; 32]);

    host.on_leader_schedule(LeaderScheduleEvent {
        source: crate::framework::ControlPlaneSource::GossipBootstrap,
        slot: Some(100),
        epoch: None,
        added_leaders: vec![LeaderScheduleEntry {
            slot: 100,
            leader: leader_a,
        }],
        removed_slots: Vec::new(),
        updated_leaders: Vec::new(),
        snapshot_leaders: Vec::new(),
    });
    assert_eq!(
        host.latest_observed_tpu_leader(),
        Some(LeaderScheduleEntry {
            slot: 100,
            leader: leader_a,
        })
    );

    host.on_leader_schedule(LeaderScheduleEvent {
        source: crate::framework::ControlPlaneSource::GossipBootstrap,
        slot: Some(99),
        epoch: None,
        added_leaders: vec![LeaderScheduleEntry {
            slot: 99,
            leader: leader_b,
        }],
        removed_slots: Vec::new(),
        updated_leaders: Vec::new(),
        snapshot_leaders: Vec::new(),
    });
    assert_eq!(
        host.latest_observed_tpu_leader(),
        Some(LeaderScheduleEntry {
            slot: 100,
            leader: leader_a,
        })
    );

    host.on_leader_schedule(LeaderScheduleEvent {
        source: crate::framework::ControlPlaneSource::GossipBootstrap,
        slot: Some(100),
        epoch: None,
        added_leaders: Vec::new(),
        removed_slots: Vec::new(),
        updated_leaders: vec![LeaderScheduleEntry {
            slot: 100,
            leader: leader_b,
        }],
        snapshot_leaders: Vec::new(),
    });
    assert_eq!(
        host.latest_observed_tpu_leader(),
        Some(LeaderScheduleEntry {
            slot: 100,
            leader: leader_b,
        })
    );
}

fn wait_until_counter(counter: &AtomicUsize, expected: usize, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if counter.load(Ordering::Relaxed) == expected {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}
