use super::*;
use agave_transaction_view::transaction_view::SanitizedTransactionView;
use async_trait::async_trait;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer as _;
use solana_transaction::Transaction;
use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use crate::event::TxKind;
use crate::framework::{
    PluginConfig, PluginContext, PluginSetupError, SerializedTransactionRange,
    TransactionBatchEvent, TransactionDispatchMode, TransactionEvent, TransactionEventRef,
    TransactionInterest, TransactionLogEvent, TransactionPrefilter, TransactionViewBatchEvent,
    TxCommitmentStatus,
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

#[test]
fn builder_tracks_transaction_log_interest() {
    struct TransactionLogPlugin;

    #[async_trait]
    impl ObserverPlugin for TransactionLogPlugin {
        fn config(&self) -> PluginConfig {
            PluginConfig {
                transaction_log: true,
                ..PluginConfig::new()
            }
        }

        fn accepts_transaction_log(&self, _event: &TransactionLogEvent) -> bool {
            true
        }
    }

    let host = PluginHostBuilder::new()
        .add_plugin(TransactionLogPlugin)
        .build();
    assert!(host.wants_transaction_log());
}

#[derive(Clone, Copy)]
struct InlineTransactionPlugin;

#[async_trait]
impl ObserverPlugin for InlineTransactionPlugin {
    fn name(&self) -> &'static str {
        "inline-transaction-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_inline_transaction()
    }
}

#[derive(Clone, Copy)]
struct InlineCriticalTransactionPlugin;

#[async_trait]
impl ObserverPlugin for InlineCriticalTransactionPlugin {
    fn name(&self) -> &'static str {
        "inline-critical-transaction-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_inline_transaction()
    }

    fn transaction_interest_ref(&self, _event: &TransactionEventRef<'_>) -> TransactionInterest {
        TransactionInterest::Critical
    }
}

#[derive(Clone, Copy)]
struct StandardCriticalTransactionPlugin;

#[async_trait]
impl ObserverPlugin for StandardCriticalTransactionPlugin {
    fn name(&self) -> &'static str {
        "standard-critical-transaction-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction()
    }

    fn transaction_interest_ref(&self, _event: &TransactionEventRef<'_>) -> TransactionInterest {
        TransactionInterest::Critical
    }
}

#[test]
fn builder_tracks_inline_transaction_preference() {
    let standard_host = PluginHostBuilder::new()
        .add_plugin(PriorityTransactionPlugin {
            critical_handled: Arc::new(AtomicUsize::new(0)),
            background_handled: Arc::new(AtomicUsize::new(0)),
        })
        .build();
    assert!(standard_host.wants_transaction());
    assert!(!standard_host.wants_inline_transaction_dispatch());

    let inline_host = PluginHostBuilder::new()
        .add_plugin(InlineTransactionPlugin)
        .build();
    assert!(inline_host.wants_transaction());
    assert!(inline_host.wants_inline_transaction_dispatch());

    let explicit_standard =
        PluginConfig::new().with_transaction_mode(TransactionDispatchMode::Standard);
    assert!(explicit_standard.transaction);
    assert_eq!(
        explicit_standard.transaction_dispatch_mode,
        TransactionDispatchMode::Standard
    );
}

#[test]
fn transaction_classification_scopes_split_inline_and_deferred_plugins() {
    let host = PluginHostBuilder::new()
        .add_plugin(InlineCriticalTransactionPlugin)
        .add_plugin(StandardCriticalTransactionPlugin)
        .build();
    let event = test_transaction_event(2);
    let event_ref = TransactionEventRef {
        slot: event.slot,
        commitment_status: event.commitment_status,
        confirmed_slot: event.confirmed_slot,
        finalized_slot: event.finalized_slot,
        signature: event.signature,
        tx: event.tx.as_ref(),
        kind: event.kind,
    };

    assert!(host.wants_transaction_dispatch_in_scope(TransactionDispatchScope::All));
    assert!(host.wants_transaction_dispatch_in_scope(TransactionDispatchScope::InlineOnly));
    assert!(host.wants_transaction_dispatch_in_scope(TransactionDispatchScope::DeferredOnly));
    assert!(
        !host
            .classify_transaction_ref_in_scope(event_ref, TransactionDispatchScope::All)
            .is_empty()
    );
    assert!(
        !host
            .classify_transaction_ref_in_scope(event_ref, TransactionDispatchScope::InlineOnly)
            .is_empty()
    );
    assert!(
        !host
            .classify_transaction_ref_in_scope(event_ref, TransactionDispatchScope::DeferredOnly)
            .is_empty()
    );
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

    fn accepts_transaction_ref(&self, event: &TransactionEventRef<'_>) -> bool {
        let accept = event.slot.is_multiple_of(2);
        if accept {
            self.accepted.fetch_add(1, Ordering::Relaxed);
        } else {
            self.rejected.fetch_add(1, Ordering::Relaxed);
        }
        accept
    }

    fn transaction_interest_ref(&self, event: &TransactionEventRef<'_>) -> TransactionInterest {
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

    fn transaction_interest_ref(&self, event: &TransactionEventRef<'_>) -> TransactionInterest {
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

#[derive(Clone)]
struct PrefilterTransactionPlugin {
    filter: TransactionPrefilter,
}

#[async_trait]
impl ObserverPlugin for PrefilterTransactionPlugin {
    fn name(&self) -> &'static str {
        "prefilter-transaction-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_inline_transaction()
    }

    fn transaction_prefilter(&self) -> Option<&TransactionPrefilter> {
        Some(&self.filter)
    }
}

#[derive(Clone, Copy)]
struct ManualAccountMatchPlugin {
    required_a: Pubkey,
    required_b: Pubkey,
}

#[async_trait]
impl ObserverPlugin for ManualAccountMatchPlugin {
    fn name(&self) -> &'static str {
        "manual-account-match-plugin"
    }

    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_inline_transaction()
    }

    fn transaction_interest_ref(&self, event: &TransactionEventRef<'_>) -> TransactionInterest {
        let has_a = tx_mentions_account_key(event.tx, self.required_a);
        let has_b = tx_mentions_account_key(event.tx, self.required_b);
        if has_a && has_b {
            TransactionInterest::Critical
        } else {
            TransactionInterest::Ignore
        }
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

    async fn setup(&self, _ctx: PluginContext) -> Result<(), PluginSetupError> {
        self.startup_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    async fn shutdown(&self, _ctx: PluginContext) {
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

    async fn setup(&self, _ctx: PluginContext) -> Result<(), PluginSetupError> {
        self.startup_attempted.store(true, Ordering::Relaxed);
        Err(PluginSetupError::new("boom"))
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

#[test]
fn transaction_prefilter_matches_manual_account_classifier() {
    let required_a = Pubkey::new_unique();
    let required_b = Pubkey::new_unique();
    let tx = test_transaction_with_static_accounts([required_a, required_b, Pubkey::new_unique()]);
    let event = TransactionEventRef {
        slot: 7,
        commitment_status: TxCommitmentStatus::Processed,
        confirmed_slot: None,
        finalized_slot: None,
        signature: Some(Signature::from([7_u8; 64])),
        tx: &tx,
        kind: TxKind::NonVote,
    };

    let manual_host = PluginHostBuilder::new()
        .add_plugin(ManualAccountMatchPlugin {
            required_a,
            required_b,
        })
        .build();
    let prefilter_host = PluginHostBuilder::new()
        .add_plugin(PrefilterTransactionPlugin {
            filter: TransactionPrefilter::new(TransactionInterest::Critical)
                .with_account_required([required_a, required_b]),
        })
        .build();

    let manual = manual_host.classify_transaction_ref(event);
    let prefiltered = prefilter_host.classify_transaction_ref(event);

    assert_eq!(manual.is_empty(), prefiltered.is_empty());
}

#[test]
fn transaction_prefilter_view_classification_matches_decoded() {
    let required_a = Pubkey::new_unique();
    let required_b = Pubkey::new_unique();
    let tx = test_transaction_with_static_accounts([required_a, required_b, Pubkey::new_unique()]);
    let serialized = bincode::serialize(&tx).expect("serialize transaction");
    let view =
        SanitizedTransactionView::try_new_sanitized(serialized.as_slice(), true).expect("view");
    let signature = tx.signatures.first().copied();
    let event = TransactionEventRef {
        slot: 42,
        commitment_status: TxCommitmentStatus::Processed,
        confirmed_slot: None,
        finalized_slot: None,
        signature,
        tx: &tx,
        kind: TxKind::NonVote,
    };

    let host = PluginHostBuilder::new()
        .add_plugin(PrefilterTransactionPlugin {
            filter: TransactionPrefilter::new(TransactionInterest::Critical)
                .with_account_required([required_a, required_b]),
        })
        .build();

    let decoded =
        host.classify_transaction_ref_in_scope(event, TransactionDispatchScope::InlineOnly);
    let viewed = host.classify_transaction_view_in_scope(
        &view,
        TxCommitmentStatus::Processed,
        TransactionDispatchScope::InlineOnly,
    );

    assert!(!viewed.needs_full_classification);
    assert_eq!(decoded.is_empty(), viewed.dispatch.is_empty());
}

#[test]
fn transaction_view_prefilter_marks_manual_plugins_for_full_decode() {
    let required_a = Pubkey::new_unique();
    let required_b = Pubkey::new_unique();
    let tx = test_transaction_with_static_accounts([required_a, required_b, Pubkey::new_unique()]);
    let serialized = bincode::serialize(&tx).expect("serialize transaction");
    let view =
        SanitizedTransactionView::try_new_sanitized(serialized.as_slice(), true).expect("view");
    let signature = tx.signatures.first().copied();
    let event = TransactionEventRef {
        slot: 42,
        commitment_status: TxCommitmentStatus::Processed,
        confirmed_slot: None,
        finalized_slot: None,
        signature,
        tx: &tx,
        kind: TxKind::NonVote,
    };

    let host = PluginHostBuilder::new()
        .add_plugin(ManualAccountMatchPlugin {
            required_a,
            required_b,
        })
        .build();

    let decoded =
        host.classify_transaction_ref_in_scope(event, TransactionDispatchScope::InlineOnly);
    let viewed = host.classify_transaction_view_in_scope(
        &view,
        TxCommitmentStatus::Processed,
        TransactionDispatchScope::InlineOnly,
    );

    assert!(viewed.needs_full_classification);
    assert!(viewed.dispatch.is_empty());
    assert!(!decoded.is_empty());
}

#[test]
fn transaction_commitment_selector_filters_transaction_dispatch() {
    struct ConfirmedTransactionPlugin {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ObserverPlugin for ConfirmedTransactionPlugin {
        fn config(&self) -> PluginConfig {
            PluginConfig::new()
                .with_transaction()
                .at_commitment(TxCommitmentStatus::Confirmed)
        }

        async fn on_transaction(&self, _event: &TransactionEvent) {
            self.counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    let counter = Arc::new(AtomicUsize::new(0));
    let host = PluginHostBuilder::new()
        .add_plugin(ConfirmedTransactionPlugin {
            counter: Arc::clone(&counter),
        })
        .build();

    let mut processed = test_transaction_event(10);
    processed.commitment_status = TxCommitmentStatus::Processed;
    host.on_transaction(processed);

    let mut confirmed = test_transaction_event(11);
    confirmed.commitment_status = TxCommitmentStatus::Confirmed;
    host.on_transaction(confirmed);

    assert!(wait_until_counter(
        counter.as_ref(),
        1,
        Duration::from_secs(2)
    ));
}

#[test]
fn transaction_commitment_selector_only_matches_exact_commitment() {
    struct ConfirmedOnlyTransactionPlugin {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ObserverPlugin for ConfirmedOnlyTransactionPlugin {
        fn config(&self) -> PluginConfig {
            PluginConfig::new()
                .with_transaction()
                .only_at_commitment(TxCommitmentStatus::Confirmed)
        }

        async fn on_transaction(&self, _event: &TransactionEvent) {
            self.counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    let counter = Arc::new(AtomicUsize::new(0));
    let host = PluginHostBuilder::new()
        .add_plugin(ConfirmedOnlyTransactionPlugin {
            counter: Arc::clone(&counter),
        })
        .build();

    let mut finalized = test_transaction_event(12);
    finalized.commitment_status = TxCommitmentStatus::Finalized;
    host.on_transaction(finalized);

    let mut confirmed = test_transaction_event(13);
    confirmed.commitment_status = TxCommitmentStatus::Confirmed;
    host.on_transaction(confirmed);

    assert!(wait_until_counter(
        counter.as_ref(),
        1,
        Duration::from_secs(2)
    ));
}

#[test]
fn transaction_log_commitment_selector_filters_dispatch() {
    struct ConfirmedLogPlugin {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ObserverPlugin for ConfirmedLogPlugin {
        fn config(&self) -> PluginConfig {
            PluginConfig {
                transaction_log: true,
                ..PluginConfig::new().at_commitment(TxCommitmentStatus::Confirmed)
            }
        }

        async fn on_transaction_log(&self, _event: &TransactionLogEvent) {
            self.counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    let counter = Arc::new(AtomicUsize::new(0));
    let host = PluginHostBuilder::new()
        .add_plugin(ConfirmedLogPlugin {
            counter: Arc::clone(&counter),
        })
        .build();

    host.on_transaction_log(TransactionLogEvent {
        slot: 10,
        commitment_status: TxCommitmentStatus::Processed,
        signature: Signature::from([10_u8; 64]),
        err: None,
        logs: Arc::from(Vec::<String>::new()),
        matched_filter: None,
    });
    host.on_transaction_log(TransactionLogEvent {
        slot: 11,
        commitment_status: TxCommitmentStatus::Confirmed,
        signature: Signature::from([11_u8; 64]),
        err: None,
        logs: Arc::from(Vec::<String>::new()),
        matched_filter: None,
    });

    assert!(wait_until_counter(
        counter.as_ref(),
        1,
        Duration::from_secs(2)
    ));
}

#[test]
fn transaction_batch_and_view_commitment_selector_filter_dispatch() {
    struct ConfirmedBatchPlugin {
        batch_counter: Arc<AtomicUsize>,
        view_counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ObserverPlugin for ConfirmedBatchPlugin {
        fn config(&self) -> PluginConfig {
            PluginConfig::new()
                .at_commitment(TxCommitmentStatus::Confirmed)
                .with_transaction_batch()
                .with_transaction_view_batch()
        }

        async fn on_transaction_batch(&self, _event: &TransactionBatchEvent) {
            self.batch_counter.fetch_add(1, Ordering::Relaxed);
        }

        async fn on_transaction_view_batch(&self, _event: &TransactionViewBatchEvent) {
            self.view_counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    let batch_counter = Arc::new(AtomicUsize::new(0));
    let view_counter = Arc::new(AtomicUsize::new(0));
    let host = PluginHostBuilder::new()
        .add_plugin(ConfirmedBatchPlugin {
            batch_counter: Arc::clone(&batch_counter),
            view_counter: Arc::clone(&view_counter),
        })
        .build();

    let tx = test_transaction_with_static_accounts([Pubkey::new_unique(), Pubkey::new_unique()]);
    let payload = bincode::serialize(&tx).expect("serialize transaction");
    let payload_len = payload.len();

    host.on_transaction_batch(
        TransactionBatchEvent {
            slot: 20,
            start_index: 0,
            end_index: 0,
            last_in_slot: false,
            shreds: 1,
            payload_len,
            commitment_status: TxCommitmentStatus::Processed,
            confirmed_slot: None,
            finalized_slot: None,
            transactions: Arc::from(vec![tx.clone()].into_boxed_slice()),
        },
        Instant::now(),
    );
    host.on_transaction_view_batch(
        TransactionViewBatchEvent {
            slot: 20,
            start_index: 0,
            end_index: 0,
            last_in_slot: false,
            shreds: 1,
            payload_len,
            commitment_status: TxCommitmentStatus::Processed,
            confirmed_slot: None,
            finalized_slot: None,
            payload: Arc::from(payload.clone().into_boxed_slice()),
            transactions: Arc::from(
                vec![SerializedTransactionRange::new(0, payload_len as u32)].into_boxed_slice(),
            ),
        },
        Instant::now(),
    );

    host.on_transaction_batch(
        TransactionBatchEvent {
            slot: 21,
            start_index: 0,
            end_index: 0,
            last_in_slot: false,
            shreds: 1,
            payload_len,
            commitment_status: TxCommitmentStatus::Confirmed,
            confirmed_slot: Some(21),
            finalized_slot: None,
            transactions: Arc::from(vec![tx].into_boxed_slice()),
        },
        Instant::now(),
    );
    host.on_transaction_view_batch(
        TransactionViewBatchEvent {
            slot: 21,
            start_index: 0,
            end_index: 0,
            last_in_slot: false,
            shreds: 1,
            payload_len,
            commitment_status: TxCommitmentStatus::Confirmed,
            confirmed_slot: Some(21),
            finalized_slot: None,
            payload: Arc::from(payload.into_boxed_slice()),
            transactions: Arc::from(
                vec![SerializedTransactionRange::new(0, payload_len as u32)].into_boxed_slice(),
            ),
        },
        Instant::now(),
    );

    assert!(wait_until_counter(
        batch_counter.as_ref(),
        1,
        Duration::from_secs(2),
    ));
    assert!(wait_until_counter(
        view_counter.as_ref(),
        1,
        Duration::from_secs(2),
    ));
}

#[test]
#[ignore = "profiling fixture for transaction prefilter A/B"]
fn transaction_prefilter_profile_fixture() {
    let plugin_count = std::env::var("SOF_TRANSACTION_PREFILTER_PROFILE_PLUGINS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(64);
    let iterations = std::env::var("SOF_TRANSACTION_PREFILTER_PROFILE_ITERS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(200_000);
    let required_a = Pubkey::new_unique();
    let required_b = Pubkey::new_unique();
    let tx = test_transaction_with_static_accounts([required_a, required_b, Pubkey::new_unique()]);
    let event = TransactionEventRef {
        slot: 42,
        commitment_status: TxCommitmentStatus::Processed,
        confirmed_slot: None,
        finalized_slot: None,
        signature: Some(Signature::from([42_u8; 64])),
        tx: &tx,
        kind: TxKind::NonVote,
    };

    let manual_host = PluginHostBuilder::new()
        .add_plugins((0..plugin_count).map(|_| ManualAccountMatchPlugin {
            required_a,
            required_b,
        }))
        .build();
    let prefilter_host = PluginHostBuilder::new()
        .add_plugins((0..plugin_count).map(|_| {
            PrefilterTransactionPlugin {
                filter: TransactionPrefilter::new(TransactionInterest::Critical)
                    .with_account_required([required_a, required_b]),
            }
        }))
        .build();

    let manual_started_at = Instant::now();
    let mut manual_hits = 0_usize;
    for _ in 0..iterations {
        manual_hits = manual_hits.saturating_add(usize::from(
            !manual_host.classify_transaction_ref(event).is_empty(),
        ));
    }
    let manual_elapsed = manual_started_at.elapsed();

    let prefilter_started_at = Instant::now();
    let mut prefilter_hits = 0_usize;
    for _ in 0..iterations {
        prefilter_hits = prefilter_hits.saturating_add(usize::from(
            !prefilter_host.classify_transaction_ref(event).is_empty(),
        ));
    }
    let prefilter_elapsed = prefilter_started_at.elapsed();

    assert_eq!(manual_hits, prefilter_hits);
    println!(
        "transaction_prefilter_profile_fixture plugins={} iterations={} manual_us={} prefilter_us={}",
        plugin_count,
        iterations,
        manual_elapsed.as_micros(),
        prefilter_elapsed.as_micros()
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

fn test_transaction_with_static_accounts<const N: usize>(
    accounts: [Pubkey; N],
) -> solana_transaction::versioned::VersionedTransaction {
    let payer = Keypair::new();
    let instruction = Instruction {
        program_id: Pubkey::new_unique(),
        accounts: accounts
            .into_iter()
            .map(|pubkey| AccountMeta::new_readonly(pubkey, false))
            .collect(),
        data: Vec::new(),
    };
    solana_transaction::versioned::VersionedTransaction::from(Transaction::new_signed_with_payer(
        &[instruction],
        Some(&payer.pubkey()),
        &[&payer],
        solana_hash::Hash::new_unique(),
    ))
}

fn tx_mentions_account_key(
    tx: &solana_transaction::versioned::VersionedTransaction,
    key: Pubkey,
) -> bool {
    tx.message.static_account_keys().contains(&key)
        || tx
            .message
            .address_table_lookups()
            .is_some_and(|lookups| lookups.iter().any(|lookup| lookup.account_key == key))
}
