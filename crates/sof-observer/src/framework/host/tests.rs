use super::*;
use async_trait::async_trait;
use solana_transaction::Transaction;
use std::{
    sync::atomic::{AtomicBool, AtomicUsize},
    time::{Duration, Instant},
};

use crate::event::TxKind;
use crate::framework::{
    PluginConfig, PluginShutdownContext, PluginStartupContext, PluginStartupError,
    TransactionEventRef, TransactionInterest, TxCommitmentStatus,
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

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_dataset()
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

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_dataset()
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

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_recent_blockhash()
    }

    async fn on_recent_blockhash(&self, _event: ObservedRecentBlockhashEvent) {
        self.counter.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct ForkHookCounterPlugin {
    slot_status_counter: Arc<AtomicUsize>,
    reorg_counter: Arc<AtomicUsize>,
}

#[async_trait]
impl ObserverPlugin for ForkHookCounterPlugin {
    fn name(&self) -> &'static str {
        "fork-hook-counter-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_slot_status().with_reorg()
    }

    async fn on_slot_status(&self, _event: SlotStatusEvent) {
        self.slot_status_counter.fetch_add(1, Ordering::Relaxed);
    }

    async fn on_reorg(&self, _event: ReorgEvent) {
        self.reorg_counter.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct FilteringTransactionPlugin {
    accepted: Arc<AtomicUsize>,
    rejected: Arc<AtomicUsize>,
    handled: Arc<AtomicUsize>,
}

#[async_trait]
impl ObserverPlugin for FilteringTransactionPlugin {
    fn name(&self) -> &'static str {
        "filtering-transaction-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction()
    }

    fn accepts_transaction_ref(&self, event: TransactionEventRef<'_>) -> bool {
        let accept = event.slot.is_multiple_of(2);
        if accept {
            self.accepted.fetch_add(1, Ordering::Relaxed);
        } else {
            self.rejected.fetch_add(1, Ordering::Relaxed);
        }
        accept
    }

    fn transaction_interest_ref(&self, event: TransactionEventRef<'_>) -> TransactionInterest {
        if self.accepts_transaction_ref(event) {
            TransactionInterest::Critical
        } else {
            TransactionInterest::Ignore
        }
    }

    async fn on_transaction(&self, _event: &TransactionEvent) {
        tokio::time::sleep(Duration::from_millis(50)).await;
        self.handled.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct PriorityTransactionPlugin {
    critical_handled: Arc<AtomicUsize>,
    background_handled: Arc<AtomicUsize>,
}

#[async_trait]
impl ObserverPlugin for PriorityTransactionPlugin {
    fn name(&self) -> &'static str {
        "priority-transaction-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction()
    }

    fn transaction_interest_ref(&self, event: TransactionEventRef<'_>) -> TransactionInterest {
        if event.slot.is_multiple_of(2) {
            TransactionInterest::Critical
        } else {
            TransactionInterest::Background
        }
    }

    async fn on_transaction(&self, event: &TransactionEvent) {
        if event.slot.is_multiple_of(2) {
            self.critical_handled.fetch_add(1, Ordering::Relaxed);
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        self.background_handled.fetch_add(1, Ordering::Relaxed);
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
fn fork_hooks_dispatch_to_plugins() {
    let slot_status_counter = Arc::new(AtomicUsize::new(0));
    let reorg_counter = Arc::new(AtomicUsize::new(0));
    let host = PluginHostBuilder::new()
        .add_plugin(ForkHookCounterPlugin {
            slot_status_counter: Arc::clone(&slot_status_counter),
            reorg_counter: Arc::clone(&reorg_counter),
        })
        .build();

    host.on_slot_status(SlotStatusEvent {
        slot: 42,
        parent_slot: Some(41),
        previous_status: Some(crate::framework::ForkSlotStatus::Processed),
        status: crate::framework::ForkSlotStatus::Confirmed,
        tip_slot: Some(42),
        confirmed_slot: Some(41),
        finalized_slot: Some(40),
    });
    host.on_reorg(ReorgEvent {
        old_tip: 50,
        new_tip: 60,
        common_ancestor: Some(45),
        detached_slots: vec![50, 49],
        attached_slots: vec![59, 60],
        confirmed_slot: Some(55),
        finalized_slot: Some(52),
    });

    assert!(wait_until_counter(
        slot_status_counter.as_ref(),
        1,
        Duration::from_secs(2),
    ));
    assert!(wait_until_counter(
        reorg_counter.as_ref(),
        1,
        Duration::from_secs(2),
    ));
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

#[test]
fn transaction_prefilter_runs_before_queue_dispatch() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let rejected = Arc::new(AtomicUsize::new(0));
    let handled = Arc::new(AtomicUsize::new(0));
    let host = PluginHostBuilder::new()
        .with_event_queue_capacity(1)
        .add_plugin(FilteringTransactionPlugin {
            accepted: Arc::clone(&accepted),
            rejected: Arc::clone(&rejected),
            handled: Arc::clone(&handled),
        })
        .build();

    host.on_transaction(test_transaction_event(2));
    for slot in (1..=199).step_by(2) {
        host.on_transaction(test_transaction_event(slot));
    }

    assert!(wait_until_counter(
        handled.as_ref(),
        1,
        Duration::from_secs(3),
    ));
    assert_eq!(rejected.load(Ordering::Relaxed), 100);
    assert_eq!(handled.load(Ordering::Relaxed), 1);
    assert_eq!(host.dropped_event_count(), 0);
}

#[test]
fn sharded_transaction_dispatch_is_consistent() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let rejected = Arc::new(AtomicUsize::new(0));
    let handled = Arc::new(AtomicUsize::new(0));
    let threads = 8_usize;
    let iterations = 40_usize;
    let expected = threads.saturating_mul(iterations);
    let host = Arc::new(
        PluginHostBuilder::new()
            .with_event_queue_capacity(expected.saturating_mul(2))
            .with_transaction_dispatch_workers(4)
            .add_plugin(FilteringTransactionPlugin {
                accepted: Arc::clone(&accepted),
                rejected: Arc::clone(&rejected),
                handled: Arc::clone(&handled),
            })
            .build(),
    );

    let mut joins = Vec::with_capacity(threads);
    for worker_index in 0..threads {
        let worker_host = Arc::clone(&host);
        joins.push(thread::spawn(move || {
            for iteration in 0..iterations {
                let slot = ((worker_index * iterations) + iteration) as u64 * 2;
                worker_host.on_transaction(test_transaction_event(slot));
            }
        }));
    }
    for join in joins {
        assert!(join.join().is_ok());
    }

    assert!(wait_until_counter(
        handled.as_ref(),
        expected,
        Duration::from_secs(8),
    ));
    assert_eq!(accepted.load(Ordering::Relaxed), expected);
    assert_eq!(rejected.load(Ordering::Relaxed), 0);
    assert_eq!(host.transaction_dropped_event_count(), 0);
    assert_eq!(host.general_dropped_event_count(), 0);
}

#[test]
fn critical_transaction_lane_stays_isolated_from_background_pressure() {
    let critical_handled = Arc::new(AtomicUsize::new(0));
    let background_handled = Arc::new(AtomicUsize::new(0));
    let host = PluginHostBuilder::new()
        .with_event_queue_capacity(8)
        .with_transaction_dispatch_workers(4)
        .add_plugin(PriorityTransactionPlugin {
            critical_handled: Arc::clone(&critical_handled),
            background_handled: Arc::clone(&background_handled),
        })
        .build();

    for slot in (1..=119).step_by(2) {
        host.on_transaction(test_transaction_event(slot));
    }
    for (slot, signature_seed) in [(2_u64, 1_u8), (4, 2), (6, 3), (8, 4)] {
        host.on_transaction(test_transaction_event_with_signature(slot, signature_seed));
    }

    assert!(wait_until_counter(
        critical_handled.as_ref(),
        4,
        Duration::from_secs(5),
    ));
    assert_eq!(host.transaction_dropped_event_count(), 0);
    assert_eq!(host.general_dropped_event_count(), 0);
    assert!(host.background_transaction_dropped_event_count() > 0);
    assert!(background_handled.load(Ordering::Relaxed) < 60);
}

#[derive(Clone)]
struct LifecyclePlugin {
    startup_count: Arc<AtomicUsize>,
    shutdown_count: Arc<AtomicUsize>,
}

#[async_trait]
impl ObserverPlugin for LifecyclePlugin {
    fn name(&self) -> &'static str {
        "lifecycle-plugin"
    }

    async fn on_startup(&self, _ctx: PluginStartupContext) -> Result<(), PluginStartupError> {
        self.startup_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    async fn on_shutdown(&self, _ctx: PluginShutdownContext) {
        self.shutdown_count.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct FailingStartupPlugin {
    startup_attempted: Arc<AtomicBool>,
}

#[async_trait]
impl ObserverPlugin for FailingStartupPlugin {
    fn name(&self) -> &'static str {
        "failing-startup-plugin"
    }

    async fn on_startup(&self, _ctx: PluginStartupContext) -> Result<(), PluginStartupError> {
        self.startup_attempted.store(true, Ordering::Relaxed);
        Err(PluginStartupError::new("boom"))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn startup_and_shutdown_hooks_run_once() {
    let startup_count = Arc::new(AtomicUsize::new(0));
    let shutdown_count = Arc::new(AtomicUsize::new(0));
    let host = PluginHostBuilder::new()
        .add_plugin(LifecyclePlugin {
            startup_count: Arc::clone(&startup_count),
            shutdown_count: Arc::clone(&shutdown_count),
        })
        .build();

    assert!(host.startup().await.is_ok());
    assert!(host.startup().await.is_ok());
    host.shutdown().await;
    host.shutdown().await;

    assert_eq!(startup_count.load(Ordering::Relaxed), 1);
    assert_eq!(shutdown_count.load(Ordering::Relaxed), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn startup_failure_shuts_down_started_plugins() {
    let startup_count = Arc::new(AtomicUsize::new(0));
    let shutdown_count = Arc::new(AtomicUsize::new(0));
    let failed_startup = Arc::new(AtomicBool::new(false));
    let host = PluginHostBuilder::new()
        .add_plugin(LifecyclePlugin {
            startup_count: Arc::clone(&startup_count),
            shutdown_count: Arc::clone(&shutdown_count),
        })
        .add_plugin(FailingStartupPlugin {
            startup_attempted: Arc::clone(&failed_startup),
        })
        .build();

    let error = host
        .startup()
        .await
        .expect_err("second plugin should fail startup");
    assert_eq!(error.plugin, "failing-startup-plugin");
    assert!(failed_startup.load(Ordering::Relaxed));
    assert_eq!(startup_count.load(Ordering::Relaxed), 1);
    assert_eq!(shutdown_count.load(Ordering::Relaxed), 1);
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

fn test_transaction_event(slot: u64) -> TransactionEvent {
    test_transaction_event_with_signature(slot, 0)
}

fn test_transaction_event_with_signature(slot: u64, signature_seed: u8) -> TransactionEvent {
    let tx = solana_transaction::versioned::VersionedTransaction::from(
        Transaction::new_with_payer(&[], None),
    );
    TransactionEvent {
        slot,
        commitment_status: TxCommitmentStatus::Processed,
        confirmed_slot: None,
        finalized_slot: None,
        signature: (signature_seed != 0)
            .then(|| solana_signature::Signature::from([signature_seed; 64])),
        tx: Arc::new(tx),
        kind: TxKind::NonVote,
    }
}
