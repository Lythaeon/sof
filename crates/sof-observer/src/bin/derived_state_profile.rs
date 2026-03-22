//! Standalone derived-state dispatch profiler target.

use std::error::Error;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use sof::{
    event::{ForkSlotStatus, TxCommitmentStatus, TxKind},
    framework::{
        DerivedStateCheckpoint, DerivedStateConsumer, DerivedStateConsumerConfig,
        DerivedStateConsumerContext, DerivedStateConsumerFault, DerivedStateConsumerSetupError,
        DerivedStateFeedEnvelope, DerivedStateFeedEvent, DerivedStateHost, FeedWatermarks,
        SlotStatusChangedEvent, TransactionAppliedEvent,
    },
};
use solana_transaction::Transaction;
use solana_transaction::versioned::VersionedTransaction;

/// Default number of events per derived-state batch.
const DEFAULT_BATCH_SIZE: usize = 64;
/// Default number of measured iterations.
const DEFAULT_ITERATIONS: usize = 200_000;
/// Default number of warmup iterations.
const DEFAULT_WARMUP_ITERATIONS: usize = 10_000;
/// Default derived-state consumer fanout width.
const DEFAULT_CONSUMERS: usize = 4;

fn main() -> Result<(), Box<dyn Error>> {
    let options = Options::parse(std::env::args().skip(1))?;
    let watermarks = FeedWatermarks {
        canonical_tip_slot: Some(22_000),
        processed_slot: Some(22_000),
        confirmed_slot: Some(21_990),
        finalized_slot: Some(21_900),
    };
    let host = derived_state_host(options.consumers, options.scenario.consumer_mode());
    let template = build_events(options.scenario, options.batch_size);

    for _ in 0..options.warmup_iterations {
        host.on_events(watermarks, template.clone());
    }

    let started = Instant::now();
    for _ in 0..options.iterations {
        host.on_events(watermarks, template.clone());
    }
    let elapsed = started.elapsed();
    let elapsed_ns = elapsed.as_nanos();
    let total_events = options.iterations.saturating_mul(options.batch_size);
    let ns_per_iter_x1000 = scale_ratio_by_1000(
        elapsed_ns,
        u128::try_from(options.iterations).unwrap_or(u128::MAX),
    );
    let ns_per_event_x1000 = scale_ratio_by_1000(
        elapsed_ns,
        u128::try_from(total_events).unwrap_or(u128::MAX),
    );
    let elapsed_ms_x1000 = elapsed.as_micros();

    println!("scenario={}", options.scenario.as_str());
    println!("consumers={}", options.consumers);
    println!("batch_size={}", options.batch_size);
    println!("warmup_iterations={}", options.warmup_iterations);
    println!("iterations={}", options.iterations);
    println!("elapsed_ms={}", format_fixed_3(elapsed_ms_x1000));
    println!("ns_per_iteration={}", format_fixed_3(ns_per_iter_x1000));
    println!("ns_per_event={}", format_fixed_3(ns_per_event_x1000));
    println!(
        "last_sequence={}",
        host.last_emitted_sequence()
            .map_or(0_u64, |sequence| sequence.0)
    );
    Ok(())
}

#[derive(Clone, Copy, Debug)]
/// Parsed command-line options for the standalone profiling binary.
struct Options {
    /// Profile scenario to execute.
    scenario: Scenario,
    /// Number of registered derived-state consumers.
    consumers: usize,
    /// Events per emitted batch.
    batch_size: usize,
    /// Timed iteration count.
    iterations: usize,
    /// Untimed warmup iteration count.
    warmup_iterations: usize,
}

impl Options {
    /// Parses command-line arguments into one `Options` value.
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Self {
            scenario: Scenario::SingleControl,
            consumers: DEFAULT_CONSUMERS,
            batch_size: DEFAULT_BATCH_SIZE,
            iterations: DEFAULT_ITERATIONS,
            warmup_iterations: DEFAULT_WARMUP_ITERATIONS,
        };

        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--scenario" => {
                    let value = args
                        .next()
                        .ok_or_else(|| String::from("missing value after --scenario"))?;
                    options.scenario = Scenario::parse(&value)?;
                }
                "--consumers" => {
                    let value = args
                        .next()
                        .ok_or_else(|| String::from("missing value after --consumers"))?;
                    options.consumers = parse_usize("--consumers", &value)?;
                }
                "--batch-size" => {
                    let value = args
                        .next()
                        .ok_or_else(|| String::from("missing value after --batch-size"))?;
                    options.batch_size = parse_usize("--batch-size", &value)?;
                }
                "--iterations" => {
                    let value = args
                        .next()
                        .ok_or_else(|| String::from("missing value after --iterations"))?;
                    options.iterations = parse_usize("--iterations", &value)?;
                }
                "--warmup-iterations" => {
                    let value = args
                        .next()
                        .ok_or_else(|| String::from("missing value after --warmup-iterations"))?;
                    options.warmup_iterations = parse_usize("--warmup-iterations", &value)?;
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                unknown => return Err(format!("unknown argument: {unknown}")),
            }
        }

        if matches!(options.scenario, Scenario::SingleControl) {
            options.consumers = 1;
        }

        Ok(options)
    }
}

#[derive(Clone, Copy, Debug)]
/// Supported derived-state dispatch profiling scenarios.
enum Scenario {
    /// One control-plane-only consumer.
    SingleControl,
    /// Four control-plane-only consumers.
    FourControl,
    /// Four full-feed consumers.
    FourFull,
    /// Four control-plane consumers receiving mixed control/tx batches.
    FourFiltered,
}

impl Scenario {
    /// Parses one scenario label from the CLI.
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "single-control" => Ok(Self::SingleControl),
            "four-control" => Ok(Self::FourControl),
            "four-full" => Ok(Self::FourFull),
            "four-filtered" => Ok(Self::FourFiltered),
            unknown => Err(format!("unknown scenario: {unknown}")),
        }
    }

    /// Returns the stable CLI label for this scenario.
    const fn as_str(self) -> &'static str {
        match self {
            Self::SingleControl => "single-control",
            Self::FourControl => "four-control",
            Self::FourFull => "four-full",
            Self::FourFiltered => "four-filtered",
        }
    }

    /// Returns the consumer subscription profile exercised by this scenario.
    const fn consumer_mode(self) -> BenchConsumerMode {
        match self {
            Self::SingleControl | Self::FourControl | Self::FourFiltered => {
                BenchConsumerMode::ControlPlaneOnly
            }
            Self::FourFull => BenchConsumerMode::FullFeed,
        }
    }
}

#[derive(Clone, Copy, Debug)]
/// Subscription shape used by the standalone profiling consumers.
enum BenchConsumerMode {
    /// Subscribes only to control-plane derived-state events.
    ControlPlaneOnly,
    /// Subscribes to the full derived-state feed.
    FullFeed,
}

/// Builds one initialized derived-state host with the requested fanout.
fn derived_state_host(consumer_count: usize, mode: BenchConsumerMode) -> DerivedStateHost {
    let mut builder = DerivedStateHost::builder();
    for _ in 0..consumer_count {
        builder = builder.add_consumer(NoopDerivedStateConsumer { mode });
    }
    let host = builder.build();
    host.initialize();
    host
}

/// Builds the batch template for one profiling scenario.
fn build_events(scenario: Scenario, count: usize) -> Vec<DerivedStateFeedEvent> {
    match scenario {
        Scenario::SingleControl | Scenario::FourControl | Scenario::FourFull => {
            control_plane_events(count)
        }
        Scenario::FourFiltered => mixed_events(count),
    }
}

/// Builds slot-status-only control-plane events.
fn control_plane_events(count: usize) -> Vec<DerivedStateFeedEvent> {
    (0..count)
        .map(|index| {
            DerivedStateFeedEvent::SlotStatusChanged(SlotStatusChangedEvent {
                slot: bench_slot(22_000, index),
                parent_slot: Some(bench_slot(21_999, index)),
                previous_status: Some(ForkSlotStatus::Processed),
                status: ForkSlotStatus::Confirmed,
            })
        })
        .collect()
}

/// Builds mixed control-plane and tx-applied events for filtered fanout profiling.
fn mixed_events(count: usize) -> Vec<DerivedStateFeedEvent> {
    let tx = Arc::new(VersionedTransaction::from(Transaction::new_with_payer(
        &[],
        None,
    )));
    (0..count)
        .map(|index| {
            if index % 2 == 0 {
                DerivedStateFeedEvent::SlotStatusChanged(SlotStatusChangedEvent {
                    slot: bench_slot(22_000, index),
                    parent_slot: Some(bench_slot(21_999, index)),
                    previous_status: Some(ForkSlotStatus::Processed),
                    status: ForkSlotStatus::Confirmed,
                })
            } else {
                DerivedStateFeedEvent::TransactionApplied(TransactionAppliedEvent {
                    slot: bench_slot(22_000, index),
                    tx_index: u32::try_from(index).unwrap_or(u32::MAX),
                    signature: None,
                    kind: TxKind::NonVote,
                    transaction: Arc::clone(&tx),
                    commitment_status: TxCommitmentStatus::Processed,
                })
            }
        })
        .collect()
}

/// Converts an event index into a benchmark slot.
fn bench_slot(base: u64, index: usize) -> u64 {
    base.saturating_add(u64::try_from(index).unwrap_or(u64::MAX))
}

#[derive(Clone, Copy, Debug)]
/// No-op consumer used to isolate host dispatch cost.
struct NoopDerivedStateConsumer {
    /// Subscription mode used by this no-op consumer.
    mode: BenchConsumerMode,
}

impl DerivedStateConsumer for NoopDerivedStateConsumer {
    fn name(&self) -> &'static str {
        "noop-derived-state-consumer"
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn extension_version(&self) -> &'static str {
        "profile"
    }

    fn load_checkpoint(
        &mut self,
    ) -> Result<Option<DerivedStateCheckpoint>, DerivedStateConsumerFault> {
        Ok(None)
    }

    fn config(&self) -> DerivedStateConsumerConfig {
        match self.mode {
            BenchConsumerMode::ControlPlaneOnly => {
                DerivedStateConsumerConfig::new().with_control_plane_observed()
            }
            BenchConsumerMode::FullFeed => DerivedStateConsumerConfig::new()
                .with_transaction_applied()
                .with_account_touch_observed()
                .with_control_plane_observed(),
        }
    }

    fn setup(
        &mut self,
        _ctx: DerivedStateConsumerContext,
    ) -> Result<(), DerivedStateConsumerSetupError> {
        Ok(())
    }

    fn apply(
        &mut self,
        envelope: &DerivedStateFeedEnvelope,
    ) -> Result<(), DerivedStateConsumerFault> {
        black_box(envelope.sequence);
        Ok(())
    }

    fn flush_checkpoint(
        &mut self,
        checkpoint: DerivedStateCheckpoint,
    ) -> Result<(), DerivedStateConsumerFault> {
        black_box(checkpoint.last_applied_sequence);
        Ok(())
    }
}

/// Parses one positive integer CLI value.
fn parse_usize(flag: &str, value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|error| format!("invalid value for {flag}: {value} ({error})"))
}

/// Prints standalone profiler usage.
fn print_help() {
    println!("derived_state_profile");
    println!("  --scenario single-control|four-control|four-full|four-filtered");
    println!("  --consumers <n>");
    println!("  --batch-size <n>");
    println!("  --iterations <n>");
    println!("  --warmup-iterations <n>");
}

/// Formats one fixed-point number encoded as value times one thousand.
fn format_fixed_3(value_x1000: u128) -> String {
    let whole = value_x1000 / 1000;
    let frac = value_x1000 % 1000;
    format!("{whole}.{frac:03}")
}

/// Scales one ratio by one thousand using checked integer arithmetic.
fn scale_ratio_by_1000(numerator: u128, denominator: u128) -> u128 {
    numerator
        .checked_mul(1000)
        .and_then(|value| value.checked_div(denominator.max(1)))
        .unwrap_or(u128::MAX)
}
