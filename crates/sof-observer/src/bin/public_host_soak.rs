//! Public-host soak harness for SOF runtime scenarios.
//!
//! This binary runs directly on the target host. It builds the required release
//! examples locally, launches them with the expected environment, and validates
//! restart, shutdown, crash recovery, and busy-poll behavior without any SSH or
//! transport indirection.

use std::{
    env,
    error::Error,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream, UdpSocket},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde_json::Value;

/// Dynamic error type used by the soak harness.
type DynError = Box<dyn Error + Send + Sync + 'static>;

/// Entry point for the public-host soak harness.
///
/// # Errors
///
/// Returns an error when argument parsing fails, the requested scenario fails,
/// or any child-process/runtime validation step does not pass.
fn main() -> Result<(), DynError> {
    let scenario = Scenario::from_args(env::args())?;
    let config = SoakConfig::from_env()?;
    let _lock = SoakLock::acquire(&config.lock_dir, config.lock_timeout)?;

    match scenario {
        Scenario::ObserverRestartLoop => run_observer_restart_loop(&config)?,
        Scenario::DerivedStateRestartCheck => run_derived_state_restart_check(&config)?,
        Scenario::DerivedStateCrashRecoveryCheck => {
            run_derived_state_crash_recovery_check(&config)?
        }
        Scenario::BusyPollCompare => run_busy_poll_compare(&config)?,
    }

    Ok(())
}

/// Supported public-host soak scenarios.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scenario {
    /// Validate observer runtime restart and shutdown behavior.
    ObserverRestartLoop,
    /// Validate derived-state checkpoint advancement across graceful restart.
    DerivedStateRestartCheck,
    /// Validate derived-state replay recovery after a forced crash.
    DerivedStateCrashRecoveryCheck,
    /// Compare baseline observer runtime behavior against busy-poll tuning.
    BusyPollCompare,
}

impl Scenario {
    /// Parses the requested scenario from CLI arguments.
    ///
    /// # Errors
    ///
    /// Returns an error when the first positional argument is missing or does
    /// not match a supported scenario name.
    fn from_args(mut args: env::Args) -> Result<Self, DynError> {
        let _program = args.next();
        let Some(raw) = args.next() else {
            return Err(io::Error::other(
                "missing scenario argument: expected one of observer-restart-loop, derived-state-restart-check, derived-state-crash-recovery-check, busy-poll-compare",
            )
            .into());
        };

        match raw.as_str() {
            "observer-restart-loop" => Ok(Self::ObserverRestartLoop),
            "derived-state-restart-check" => Ok(Self::DerivedStateRestartCheck),
            "derived-state-crash-recovery-check" => Ok(Self::DerivedStateCrashRecoveryCheck),
            "busy-poll-compare" => Ok(Self::BusyPollCompare),
            _ => Err(io::Error::other(format!(
                "unsupported scenario '{raw}': expected observer-restart-loop, derived-state-restart-check, derived-state-crash-recovery-check, or busy-poll-compare"
            ))
            .into()),
        }
    }
}

/// Local host configuration for the soak harness.
#[derive(Clone, Debug)]
struct SoakConfig {
    /// Repository root used for builds, logs, and demo-state storage.
    repo_dir: PathBuf,
    /// Soak log directory under the repository root.
    log_dir: PathBuf,
    /// Demo-state directory under the repository root.
    demo_state_dir: PathBuf,
    /// Lock directory used to serialize soak runs.
    lock_dir: PathBuf,
    /// Gossip entrypoint used by the public-host runtime scenarios.
    gossip_entrypoint: String,
    /// Shared UDP port range used by the public-host runtime scenarios.
    port_range: PortRange,
    /// Shred dedupe capacity used by the runtime scenarios.
    shred_dedup_capacity: u64,
    /// Runtime log level used by launched examples.
    runtime_log_level: String,
    /// Tuning preset used by observer runtime scenarios.
    tuning_preset: String,
    /// Seconds to wait for soak-lock acquisition.
    lock_timeout: Duration,
    /// Number of observer restart cycles to run.
    observer_cycles: u32,
    /// Live run window for observer restart validation.
    observer_run_window: Duration,
    /// Maximum time to wait for runtime startup.
    startup_timeout: Duration,
    /// Maximum time to wait for runtime shutdown.
    shutdown_timeout: Duration,
    /// Number of startup retry attempts for transient bootstrap failures.
    startup_retries: u32,
    /// Live run window before reading the first derived-state checkpoint.
    derived_state_run_window: Duration,
    /// Maximum time to wait for derived-state checkpoint advancement.
    derived_state_progress_timeout: Duration,
    /// Live run window before forcing a derived-state crash.
    derived_state_crash_window: Duration,
    /// Busy-poll comparison run window.
    busy_poll_compare_window: Duration,
    /// Busy-poll `SOF_UDP_BUSY_POLL_US` override.
    busy_poll_us: u32,
    /// Busy-poll `SOF_UDP_BUSY_POLL_BUDGET` override.
    busy_poll_budget: u32,
}

impl SoakConfig {
    /// Builds soak configuration from environment variables and repository
    /// defaults.
    ///
    /// # Errors
    ///
    /// Returns an error when an environment variable cannot be parsed or the
    /// repository root cannot be resolved.
    fn from_env() -> Result<Self, DynError> {
        let repo_dir = resolve_repo_dir()?;
        let log_dir = repo_dir.join("logs").join("soak-validation");
        let demo_state_dir = repo_dir.join("demo-state");
        let lock_dir = repo_dir.join(".soak-lock");

        Ok(Self {
            repo_dir,
            log_dir,
            demo_state_dir,
            lock_dir,
            gossip_entrypoint: env_or_default("SOF_GOSSIP_ENTRYPOINT", "64.130.50.23:8001"),
            port_range: PortRange::parse(&env_or_default("SOF_PORT_RANGE", "12000-12100"))?,
            shred_dedup_capacity: parse_env_or_default("SOF_SHRED_DEDUP_CAPACITY", 524_288_u64)?,
            runtime_log_level: env_or_default("SOF_RUNTIME_LOG_LEVEL", "info"),
            tuning_preset: env_or_default("SOF_TUNING_PRESET", "vps"),
            lock_timeout: Duration::from_secs(parse_env_or_default(
                "SOF_SOAK_LOCK_TIMEOUT_SECS",
                60_u64,
            )?),
            observer_cycles: parse_env_or_default("SOF_SOAK_CYCLES", 2_u32)?,
            observer_run_window: Duration::from_secs(parse_env_or_default(
                "SOF_SOAK_RUN_SECS",
                75_u64,
            )?),
            startup_timeout: Duration::from_secs(parse_env_or_default(
                "SOF_SOAK_STARTUP_TIMEOUT_SECS",
                90_u64,
            )?),
            shutdown_timeout: Duration::from_secs(parse_env_or_default(
                "SOF_SOAK_SHUTDOWN_TIMEOUT_SECS",
                30_u64,
            )?),
            startup_retries: parse_env_or_default("SOF_SOAK_STARTUP_RETRIES", 3_u32)?,
            derived_state_run_window: Duration::from_secs(parse_env_or_default(
                "SOF_DERIVED_STATE_RUN_SECS",
                45_u64,
            )?),
            derived_state_progress_timeout: Duration::from_secs(parse_env_or_default(
                "SOF_DERIVED_STATE_PROGRESS_TIMEOUT_SECS",
                60_u64,
            )?),
            derived_state_crash_window: Duration::from_secs(parse_env_or_default(
                "SOF_DERIVED_STATE_CRASH_RUN_SECS",
                20_u64,
            )?),
            busy_poll_compare_window: Duration::from_secs(parse_env_or_default(
                "SOF_COMPARE_RUN_SECS",
                300_u64,
            )?),
            busy_poll_us: parse_env_or_default("SOF_BUSY_POLL_US", 50_u32)?,
            busy_poll_budget: parse_env_or_default("SOF_BUSY_POLL_BUDGET", 64_u32)?,
        })
    }
}

/// Shared UDP port range used by the public-host scenarios.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PortRange {
    /// Inclusive range start.
    start: u16,
    /// Inclusive range end.
    end: u16,
}

impl PortRange {
    /// Parses a `start-end` port range.
    ///
    /// # Errors
    ///
    /// Returns an error when the format is invalid or the range is reversed.
    fn parse(raw: &str) -> Result<Self, DynError> {
        let Some((start, end)) = raw.split_once('-') else {
            return Err(io::Error::other(format!(
                "invalid port range '{raw}': expected start-end"
            ))
            .into());
        };
        let start = start.parse::<u16>()?;
        let end = end.parse::<u16>()?;
        if start > end {
            return Err(io::Error::other(format!(
                "invalid port range '{raw}': start must be <= end"
            ))
            .into());
        }
        Ok(Self { start, end })
    }

    /// Formats the range back into the runtime environment representation.
    #[must_use]
    fn as_env_value(self) -> String {
        format!("{}-{}", self.start, self.end)
    }
}

/// Filesystem-based soak lock that serializes host-local runs.
#[derive(Debug)]
struct SoakLock {
    /// Lock directory path.
    path: PathBuf,
}

impl SoakLock {
    /// Acquires the soak lock by creating a directory and retrying until a
    /// timeout expires.
    ///
    /// # Errors
    ///
    /// Returns an error when the lock cannot be acquired before the timeout.
    fn acquire(path: &Path, timeout: Duration) -> Result<Self, DynError> {
        let started_at = Instant::now();
        loop {
            match fs::create_dir(path) {
                Ok(()) => {
                    return Ok(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if started_at.elapsed() >= timeout {
                        return Err(io::Error::other(format!(
                            "timed out waiting for soak lock at {}",
                            path.display()
                        ))
                        .into());
                    }
                    thread::sleep(Duration::from_secs(1));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for SoakLock {
    /// Releases the lock directory best-effort when the harness exits.
    fn drop(&mut self) {
        drop(fs::remove_dir_all(&self.path));
    }
}

/// Launch result for a started example process.
#[derive(Debug)]
struct StartedProcess {
    /// Child process handle.
    child: Child,
    /// Log file path used for stdout/stderr redirection.
    log_path: PathBuf,
}

/// Snapshot of busy-poll profile results.
#[derive(Debug)]
struct BusyPollProfileSummary {
    /// Profile name.
    name: &'static str,
    /// Whether direct ingest sockets confirmed busy-poll configuration.
    direct_busy_poll: bool,
    /// Whether observer-facing gossip sockets confirmed busy-poll configuration.
    gossip_busy_poll: bool,
    /// Latest shred age in milliseconds.
    latest_shred_age_ms: String,
    /// Latest dataset age in milliseconds.
    latest_dataset_age_ms: String,
    /// Gossip runtime stall age in milliseconds.
    gossip_runtime_stall_age_ms: String,
    /// Ingest dropped-packet counter.
    ingest_dropped_packets: String,
    /// Dataset queue drop counter.
    dataset_queue_drops: String,
    /// Packet worker dropped-packet counter.
    packet_worker_dropped_packets: String,
    /// Dedupe entries gauge.
    dedupe_entries: String,
    /// Dedupe capacity gauge.
    dedupe_capacity: String,
    /// Dedupe capacity-eviction counter.
    dedupe_capacity_evictions: String,
    /// Receiver queue length.
    receiver_channel_len: String,
    /// Receiver dropped-packet counter.
    receiver_dropped_packets: String,
    /// Verify queue current length.
    verify_queue_current: String,
    /// Verify queue max length.
    verify_queue_max: String,
    /// Verify queue dropped-packet counter.
    verify_dropped_packets: String,
    /// Output queue current length.
    output_queue_current: String,
    /// Output queue max length.
    output_queue_max: String,
    /// Output queue dropped-packet counter.
    output_dropped_packets: String,
    /// Profile log path.
    log_path: PathBuf,
}

/// Runs the observer restart-loop scenario.
///
/// # Errors
///
/// Returns an error when build, launch, metric validation, or shutdown fails.
fn run_observer_restart_loop(config: &SoakConfig) -> Result<(), DynError> {
    ensure_host_dirs(config)?;
    build_example(config, "observer_runtime")?;
    println!(
        "Running observer restart soak on {} for {} cycles ({}s live window each)",
        config.gossip_entrypoint,
        config.observer_cycles,
        config.observer_run_window.as_secs()
    );

    let binary_path = example_binary_path(config, "observer_runtime");
    for cycle in 1..=config.observer_cycles {
        let log_path = config
            .log_dir
            .join(format!("observer_runtime_cycle_{cycle}.log"));
        println!();
        println!("== cycle {cycle} ==");
        let observability_port = allocate_loopback_port()?;
        let observability_bind = format!("127.0.0.1:{observability_port}");
        let mut process =
            start_observer_with_retries(config, &binary_path, &log_path, &observability_bind)?;
        println!("bootstrap completed");

        thread::sleep(config.observer_run_window);

        let receiver_line = require_last_matching_line(&log_path, "gossip_receiver")?;
        let verify_line =
            require_last_matching_line(&log_path, "gossip_socket_consume_verify_queue")?;
        let output_line =
            require_last_matching_line(&log_path, "gossip_socket_consume_output_queue")?;
        let runtime_ready = scrape_prometheus_metric(&observability_bind, "sof_runtime_ready")?;
        let ingest_dropped_packets =
            scrape_prometheus_metric(&observability_bind, "sof_ingest_dropped_packets_total")?;
        let dataset_queue_drops =
            scrape_prometheus_metric(&observability_bind, "sof_dataset_queue_dropped_jobs_total")?;
        let packet_worker_dropped_packets = scrape_prometheus_metric(
            &observability_bind,
            "sof_packet_worker_dropped_packets_total",
        )?;
        let latest_shred_age_ms =
            scrape_prometheus_metric(&observability_bind, "sof_latest_shred_age_ms")?;
        let dedupe_entries =
            scrape_prometheus_metric(&observability_bind, "sof_shred_dedupe_entries")?;
        let dedupe_high_watermark =
            scrape_prometheus_metric(&observability_bind, "sof_shred_dedupe_max_entries")?;
        let gossip_runtime_stall_age_ms =
            scrape_prometheus_metric(&observability_bind, "sof_gossip_runtime_stall_age_ms")?;

        require_metric_value(&runtime_ready, "sof_runtime_ready", "1")?;
        require_metric_value(
            &ingest_dropped_packets,
            "sof_ingest_dropped_packets_total",
            "0",
        )?;
        require_metric_value(
            &dataset_queue_drops,
            "sof_dataset_queue_dropped_jobs_total",
            "0",
        )?;
        require_metric_value(
            &packet_worker_dropped_packets,
            "sof_packet_worker_dropped_packets_total",
            "0",
        )?;
        require_metric_value(
            &gossip_runtime_stall_age_ms,
            "sof_gossip_runtime_stall_age_ms",
            "0",
        )?;
        require_line_metric_value(&receiver_line, "num_packets_dropped", "0")?;
        require_line_metric_value(&verify_line, "dropped_packets", "0")?;
        require_line_metric_value(&output_line, "dropped_packets", "0")?;

        println!(
            "ingest ok: latest_shred_age_ms={} dedupe_entries={} dedupe_high_watermark={}",
            latest_shred_age_ms, dedupe_entries, dedupe_high_watermark
        );
        println!(
            "gossip queues ok: receiver_len={} verify_max_len={} output_max_len={}",
            extract_line_metric(&receiver_line, "channel_len")?,
            extract_line_metric(&verify_line, "max_len")?,
            extract_line_metric(&output_line, "max_len")?
        );

        stop_observer_gracefully(config, &mut process.child, &process.log_path)?;
        println!("shutdown completed");
    }

    println!();
    println!("Observer restart soak completed successfully.");
    Ok(())
}

/// Runs the derived-state graceful restart scenario.
///
/// # Errors
///
/// Returns an error when build, launch, checkpoint validation, or shutdown
/// fails.
fn run_derived_state_restart_check(config: &SoakConfig) -> Result<(), DynError> {
    ensure_host_dirs(config)?;
    build_example(config, "derived_state_slot_mirror")?;

    let binary_path = example_binary_path(config, "derived_state_slot_mirror");
    let work_dir = config.demo_state_dir.join("derived_state_slot_mirror");
    let checkpoint_path = work_dir
        .join(".sof-example")
        .join("slot-mirror-checkpoint.json");
    let first_log_path = config.log_dir.join("derived_state_restart_check_first.log");
    let second_log_path = config
        .log_dir
        .join("derived_state_restart_check_second.log");

    reset_dir(&work_dir)?;
    remove_if_exists(&first_log_path)?;
    remove_if_exists(&second_log_path)?;

    println!("Running derived-state restart check on local host");

    let mut first =
        start_derived_state_with_retries(config, &binary_path, &work_dir, &first_log_path, None)?;
    thread::sleep(config.derived_state_run_window);

    let first_sequence = read_checkpoint_sequence(&checkpoint_path)?;
    println!("first checkpoint sequence: {first_sequence}");

    stop_derived_state_gracefully(config, &mut first.child, &first.log_path)?;
    println!("first shutdown completed");

    let mut second =
        start_derived_state_with_retries(config, &binary_path, &work_dir, &second_log_path, None)?;
    let second_sequence = wait_for_checkpoint_advance(
        &checkpoint_path,
        first_sequence,
        config.derived_state_progress_timeout,
    )?;

    println!(
        "checkpoint advanced across restart: {} -> {}",
        first_sequence, second_sequence
    );

    stop_derived_state_gracefully(config, &mut second.child, &second.log_path)?;
    println!("derived-state restart check completed successfully");
    Ok(())
}

/// Runs the derived-state crash-recovery scenario.
///
/// # Errors
///
/// Returns an error when build, launch, checkpoint validation, metric scraping,
/// or shutdown fails.
fn run_derived_state_crash_recovery_check(config: &SoakConfig) -> Result<(), DynError> {
    ensure_host_dirs(config)?;
    build_example(config, "derived_state_slot_mirror")?;

    let binary_path = example_binary_path(config, "derived_state_slot_mirror");
    let work_dir = config.demo_state_dir.join("derived_state_slot_mirror");
    let checkpoint_path = work_dir
        .join(".sof-example")
        .join("slot-mirror-checkpoint.json");
    let first_log_path = config
        .log_dir
        .join("derived_state_crash_recovery_first.log");
    let second_log_path = config
        .log_dir
        .join("derived_state_crash_recovery_second.log");
    let observability_port = allocate_loopback_port()?;
    let observability_bind = format!("127.0.0.1:{observability_port}");

    reset_dir(&work_dir)?;
    remove_if_exists(&first_log_path)?;
    remove_if_exists(&second_log_path)?;

    println!("Running derived-state crash recovery check on local host");

    let mut first = start_derived_state_with_retries(
        config,
        &binary_path,
        &work_dir,
        &first_log_path,
        Some(&observability_bind),
    )?;
    wait_for_log_line(
        &first.log_path,
        "derived-state consumers enabled",
        config.startup_timeout,
    )?;
    thread::sleep(config.derived_state_crash_window);

    let first_sequence = read_checkpoint_sequence(&checkpoint_path)?;
    println!("first checkpoint sequence before crash: {first_sequence}");

    kill_child(&mut first.child, Signal::SIGKILL)?;
    wait_for_child_exit(&mut first.child, config.shutdown_timeout)?;
    wait_for_port_range_release(config.port_range, config.shutdown_timeout)?;
    println!("forced crash completed");

    let mut second = start_derived_state_with_retries(
        config,
        &binary_path,
        &work_dir,
        &second_log_path,
        Some(&observability_bind),
    )?;
    wait_for_log_line(
        &second.log_path,
        "derived-state consumers enabled",
        config.startup_timeout,
    )?;

    let second_sequence = wait_for_checkpoint_advance(
        &checkpoint_path,
        first_sequence,
        config.derived_state_progress_timeout,
    )?;
    let healthy_consumers = wait_for_prometheus_metric(
        &observability_bind,
        "sof_derived_state_healthy_consumers",
        config.startup_timeout,
    )?;
    let fault_total = wait_for_prometheus_metric(
        &observability_bind,
        "sof_derived_state_fault_total",
        config.startup_timeout,
    )?;
    let replay_append_failures = wait_for_prometheus_metric(
        &observability_bind,
        "sof_derived_state_replay_append_failures_total",
        config.startup_timeout,
    )?;
    let replay_load_failures = wait_for_prometheus_metric(
        &observability_bind,
        "sof_derived_state_replay_load_failures_total",
        config.startup_timeout,
    )?;
    let runtime_ready = wait_for_prometheus_metric(
        &observability_bind,
        "sof_runtime_ready",
        config.startup_timeout,
    )?;
    let ingest_dropped_packets = wait_for_prometheus_metric(
        &observability_bind,
        "sof_ingest_dropped_packets_total",
        config.startup_timeout,
    )?;
    let dataset_queue_drops = wait_for_prometheus_metric(
        &observability_bind,
        "sof_dataset_queue_dropped_jobs_total",
        config.startup_timeout,
    )?;

    require_metric_value(
        &healthy_consumers,
        "sof_derived_state_healthy_consumers",
        "1",
    )?;
    require_metric_value(&fault_total, "sof_derived_state_fault_total", "0")?;
    require_metric_value(
        &replay_append_failures,
        "sof_derived_state_replay_append_failures_total",
        "0",
    )?;
    require_metric_value(
        &replay_load_failures,
        "sof_derived_state_replay_load_failures_total",
        "0",
    )?;
    require_metric_value(&runtime_ready, "sof_runtime_ready", "1")?;
    require_metric_value(
        &ingest_dropped_packets,
        "sof_ingest_dropped_packets_total",
        "0",
    )?;
    require_metric_value(
        &dataset_queue_drops,
        "sof_dataset_queue_dropped_jobs_total",
        "0",
    )?;

    println!(
        "checkpoint advanced after recovery: {} -> {}",
        first_sequence, second_sequence
    );
    println!(
        "metrics ok: healthy_consumers={} fault_total={} replay_append_failures={} replay_load_failures={} runtime_ready={}",
        healthy_consumers, fault_total, replay_append_failures, replay_load_failures, runtime_ready
    );
    println!(
        "queue health ok: ingest_dropped_packets={} dataset_queue_drops={}",
        ingest_dropped_packets, dataset_queue_drops
    );

    stop_derived_state_gracefully(config, &mut second.child, &second.log_path)?;
    println!("derived-state crash recovery check completed successfully");
    Ok(())
}

/// Runs the busy-poll comparison scenario.
///
/// # Errors
///
/// Returns an error when build, launch, log parsing, or shutdown fails.
fn run_busy_poll_compare(config: &SoakConfig) -> Result<(), DynError> {
    ensure_host_dirs(config)?;
    build_example(config, "observer_runtime")?;
    let binary_path = example_binary_path(config, "observer_runtime");
    let baseline_log_path = config.log_dir.join("busy_poll_baseline.log");
    let busy_poll_log_path = config.log_dir.join("busy_poll_busy_poll.log");

    let baseline =
        run_busy_poll_profile(config, &binary_path, &baseline_log_path, "baseline", &[])?;
    let busy_poll = run_busy_poll_profile(
        config,
        &binary_path,
        &busy_poll_log_path,
        "busy_poll",
        &[
            (
                String::from("SOF_UDP_BUSY_POLL_US"),
                config.busy_poll_us.to_string(),
            ),
            (
                String::from("SOF_UDP_BUSY_POLL_BUDGET"),
                config.busy_poll_budget.to_string(),
            ),
            (
                String::from("SOF_UDP_PREFER_BUSY_POLL"),
                String::from("true"),
            ),
        ],
    )?;

    print_busy_poll_summary(&baseline);
    print_busy_poll_summary(&busy_poll);
    Ok(())
}

/// Prints a formatted busy-poll profile summary.
fn print_busy_poll_summary(summary: &BusyPollProfileSummary) {
    println!("== {} ==", summary.name);
    println!("log={}", summary.log_path.display());
    println!(
        "direct_busy_poll={} gossip_busy_poll={}",
        if summary.direct_busy_poll {
            "yes"
        } else {
            "no"
        },
        if summary.gossip_busy_poll {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        "latest_shred_age_ms={} latest_dataset_age_ms={} gossip_runtime_stall_age_ms={}",
        summary.latest_shred_age_ms,
        summary.latest_dataset_age_ms,
        summary.gossip_runtime_stall_age_ms
    );
    println!(
        "ingest_dropped_packets={} dataset_queue_drops={} packet_worker_dropped_packets={}",
        summary.ingest_dropped_packets,
        summary.dataset_queue_drops,
        summary.packet_worker_dropped_packets
    );
    println!(
        "dedupe_entries={}/{} dedupe_capacity_evictions={}",
        summary.dedupe_entries, summary.dedupe_capacity, summary.dedupe_capacity_evictions
    );
    println!(
        "receiver_channel_len={} receiver_dropped_packets={}",
        summary.receiver_channel_len, summary.receiver_dropped_packets
    );
    println!(
        "verify_queue_current={} verify_queue_max={} verify_dropped_packets={}",
        summary.verify_queue_current, summary.verify_queue_max, summary.verify_dropped_packets
    );
    println!(
        "output_queue_current={} output_queue_max={} output_dropped_packets={}",
        summary.output_queue_current, summary.output_queue_max, summary.output_dropped_packets
    );
}

/// Runs a single busy-poll profile and returns the parsed summary.
///
/// # Errors
///
/// Returns an error when launch, startup, log parsing, or shutdown fails.
fn run_busy_poll_profile(
    config: &SoakConfig,
    binary_path: &Path,
    log_path: &Path,
    name: &'static str,
    extra_env: &[(String, String)],
) -> Result<BusyPollProfileSummary, DynError> {
    remove_if_exists(log_path)?;
    let observability_port = allocate_loopback_port()?;
    let observability_bind = format!("127.0.0.1:{observability_port}");
    let mut process = start_observer_profile_with_retries(
        config,
        binary_path,
        log_path,
        &observability_bind,
        extra_env,
    )?;

    thread::sleep(config.busy_poll_compare_window);
    stop_observer_gracefully(config, &mut process.child, &process.log_path)?;

    let ingest_line = require_last_matching_line(log_path, "ingest telemetry")?;
    let receiver_line = require_last_matching_line(log_path, "gossip_receiver")?;
    let verify_line = require_last_matching_line(log_path, "gossip_socket_consume_verify_queue")?;
    let output_line = require_last_matching_line(log_path, "gossip_socket_consume_output_queue")?;

    Ok(BusyPollProfileSummary {
        name,
        direct_busy_poll: file_contains(log_path, "configured UDP busy-poll socket options")?,
        gossip_busy_poll: file_contains(
            log_path,
            "configured Linux UDP busy-poll socket options for observer-facing gossip sockets",
        )?,
        latest_shred_age_ms: extract_line_metric(&ingest_line, "latest_shred_age_ms")?,
        latest_dataset_age_ms: extract_line_metric(&ingest_line, "latest_dataset_age_ms")?,
        gossip_runtime_stall_age_ms: extract_line_metric(
            &ingest_line,
            "gossip_runtime_stall_age_ms",
        )?,
        ingest_dropped_packets: extract_line_metric(&ingest_line, "ingest_dropped_packets")?,
        dataset_queue_drops: extract_line_metric(&ingest_line, "dataset_queue_drops")?,
        packet_worker_dropped_packets: extract_line_metric(
            &ingest_line,
            "packet_worker_dropped_packets",
        )?,
        dedupe_entries: extract_line_metric(&ingest_line, "dedupe_entries")?,
        dedupe_capacity: extract_line_metric(&ingest_line, "dedupe_capacity")?,
        dedupe_capacity_evictions: extract_line_metric(&ingest_line, "dedupe_capacity_evictions")?,
        receiver_channel_len: extract_line_metric(&receiver_line, "channel_len")?,
        receiver_dropped_packets: extract_line_metric(&receiver_line, "num_packets_dropped")?,
        verify_queue_current: extract_line_metric(&verify_line, "current_len")?,
        verify_queue_max: extract_line_metric(&verify_line, "max_len")?,
        verify_dropped_packets: extract_line_metric(&verify_line, "dropped_packets")?,
        output_queue_current: extract_line_metric(&output_line, "current_len")?,
        output_queue_max: extract_line_metric(&output_line, "max_len")?,
        output_dropped_packets: extract_line_metric(&output_line, "dropped_packets")?,
        log_path: log_path.to_path_buf(),
    })
}

/// Starts an observer runtime with scenario retries.
///
/// # Errors
///
/// Returns an error when all startup attempts fail.
fn start_observer_with_retries(
    config: &SoakConfig,
    binary_path: &Path,
    log_path: &Path,
    observability_bind: &str,
) -> Result<StartedProcess, DynError> {
    start_observer_profile_with_retries(config, binary_path, log_path, observability_bind, &[])
}

/// Starts an observer runtime with retries and extra environment variables.
///
/// # Errors
///
/// Returns an error when all startup attempts fail.
fn start_observer_profile_with_retries(
    config: &SoakConfig,
    binary_path: &Path,
    log_path: &Path,
    observability_bind: &str,
    extra_env: &[(String, String)],
) -> Result<StartedProcess, DynError> {
    for attempt in 1..=config.startup_retries {
        cleanup_stale_processes("observer_runtime")?;
        wait_for_port_range_release(config.port_range, config.shutdown_timeout)?;
        remove_if_exists(log_path)?;

        let mut child = spawn_example_process(
            binary_path,
            config.repo_dir.as_path(),
            log_path,
            &observer_environment(config, observability_bind, extra_env),
        )?;

        let outcome = wait_for_startup_outcome(
            &mut child,
            log_path,
            config.startup_timeout,
            &["receiver bootstrap completed"],
            &[
                "receiver runtime bootstrap failed",
                "Address already in use",
            ],
        )?;
        if outcome {
            return Ok(StartedProcess {
                child,
                log_path: log_path.to_path_buf(),
            });
        }

        if attempt == config.startup_retries {
            return Err(io::Error::other(format!(
                "observer failed to start after {} attempts; last log: {}",
                config.startup_retries,
                log_path.display()
            ))
            .into());
        }

        println!(
            "startup attempt {}/{} failed; retrying",
            attempt, config.startup_retries
        );
        kill_child_if_running(&mut child, Signal::SIGTERM)?;
        wait_for_child_exit(&mut child, config.shutdown_timeout)?;
        wait_for_port_range_release(config.port_range, config.shutdown_timeout)?;
    }

    Err(io::Error::other("observer startup loop terminated unexpectedly").into())
}

/// Starts a derived-state example with retries.
///
/// # Errors
///
/// Returns an error when all startup attempts fail.
fn start_derived_state_with_retries(
    config: &SoakConfig,
    binary_path: &Path,
    work_dir: &Path,
    log_path: &Path,
    observability_bind: Option<&str>,
) -> Result<StartedProcess, DynError> {
    for attempt in 1..=config.startup_retries {
        cleanup_stale_processes("derived_state_slot_mirror")?;
        wait_for_port_range_release(config.port_range, config.shutdown_timeout)?;
        remove_if_exists(log_path)?;

        let mut environment = derived_state_environment(config, observability_bind);
        environment.push((String::from("SOF_RUN_EXAMPLE"), String::from("1")));
        let mut child = spawn_example_process(binary_path, work_dir, log_path, &environment)?;

        let outcome = wait_for_startup_outcome(
            &mut child,
            log_path,
            config.startup_timeout,
            &[
                "receiver bootstrap completed",
                "derived-state consumer startup completed",
            ],
            &[
                "receiver runtime bootstrap failed",
                "Address already in use",
            ],
        )?;
        if outcome {
            return Ok(StartedProcess {
                child,
                log_path: log_path.to_path_buf(),
            });
        }

        if attempt == config.startup_retries {
            return Err(io::Error::other(format!(
                "derived-state consumer failed to start after {} attempts; last log: {}",
                config.startup_retries,
                log_path.display()
            ))
            .into());
        }

        println!(
            "startup attempt {}/{} failed; retrying",
            attempt, config.startup_retries
        );
        kill_child_if_running(&mut child, Signal::SIGTERM)?;
        wait_for_child_exit(&mut child, config.shutdown_timeout)?;
        wait_for_port_range_release(config.port_range, config.shutdown_timeout)?;
    }

    Err(io::Error::other("derived-state startup loop terminated unexpectedly").into())
}

/// Waits for startup success or failure patterns in the log while monitoring
/// the child process.
///
/// # Errors
///
/// Returns an error when the timeout expires or the child exits unexpectedly
/// before a success/failure condition is observed.
fn wait_for_startup_outcome(
    child: &mut Child,
    log_path: &Path,
    timeout: Duration,
    success_patterns: &[&str],
    failure_patterns: &[&str],
) -> Result<bool, DynError> {
    let started_at = Instant::now();
    loop {
        if started_at.elapsed() >= timeout {
            return Err(io::Error::other(format!(
                "startup timed out after {}s for log {}",
                timeout.as_secs(),
                log_path.display()
            ))
            .into());
        }

        if file_contains_all(log_path, success_patterns)? {
            return Ok(true);
        }
        if file_contains_any(log_path, failure_patterns)? {
            return Ok(false);
        }
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "child exited during startup with status {status}"
            ))
            .into());
        }

        thread::sleep(Duration::from_secs(1));
    }
}

/// Stops an observer runtime gracefully and validates shutdown completion.
///
/// # Errors
///
/// Returns an error when the process does not stop cleanly.
fn stop_observer_gracefully(
    config: &SoakConfig,
    child: &mut Child,
    log_path: &Path,
) -> Result<(), DynError> {
    kill_child(child, Signal::SIGTERM)?;
    wait_for_log_line(
        log_path,
        "observer runtime shutdown signal received",
        config.shutdown_timeout,
    )?;
    wait_for_child_exit(child, config.shutdown_timeout)?;
    wait_for_port_range_release(config.port_range, config.shutdown_timeout)?;
    Ok(())
}

/// Stops a derived-state example gracefully and validates shutdown completion.
///
/// # Errors
///
/// Returns an error when the process does not stop cleanly.
fn stop_derived_state_gracefully(
    config: &SoakConfig,
    child: &mut Child,
    log_path: &Path,
) -> Result<(), DynError> {
    kill_child(child, Signal::SIGTERM)?;
    wait_for_log_line(
        log_path,
        "observer runtime shutdown signal received",
        config.shutdown_timeout,
    )?;
    wait_for_log_line(
        log_path,
        "derived-state consumer shutdown completed",
        config.shutdown_timeout,
    )?;
    wait_for_child_exit(child, config.shutdown_timeout)?;
    wait_for_port_range_release(config.port_range, config.shutdown_timeout)?;
    Ok(())
}

/// Builds a release example with `gossip-bootstrap` enabled.
///
/// # Errors
///
/// Returns an error when `cargo build` fails.
fn build_example(config: &SoakConfig, example_name: &str) -> Result<(), DynError> {
    let status = Command::new("cargo")
        .current_dir(&config.repo_dir)
        .args([
            "build",
            "--release",
            "-p",
            "sof",
            "--example",
            example_name,
            "--features",
            "gossip-bootstrap",
        ])
        .status()?;
    require_success(status, &format!("cargo build example {example_name}"))?;
    Ok(())
}

/// Returns the path of a release example binary.
#[must_use]
fn example_binary_path(config: &SoakConfig, example_name: &str) -> PathBuf {
    config
        .repo_dir
        .join("target")
        .join("release")
        .join("examples")
        .join(example_name)
}

/// Spawns an example process with stdout/stderr redirected to a log file.
///
/// # Errors
///
/// Returns an error when the log file cannot be opened or the process cannot be
/// spawned.
fn spawn_example_process(
    binary_path: &Path,
    current_dir: &Path,
    log_path: &Path,
    environment: &[(String, String)],
) -> Result<Child, DynError> {
    let log_file = open_log_file(log_path)?;
    let stderr_file = log_file.try_clone()?;
    let mut command = Command::new(binary_path);
    command.current_dir(current_dir);
    for (key, value) in environment {
        command.env(key, value);
    }
    command.stdout(Stdio::from(log_file));
    command.stderr(Stdio::from(stderr_file));
    Ok(command.spawn()?)
}

/// Opens a log file for append, creating parent directories when necessary.
///
/// # Errors
///
/// Returns an error when the parent directory or file cannot be created.
fn open_log_file(path: &Path) -> Result<File, DynError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    Ok(file)
}

/// Ensures the standard host-local directories exist.
///
/// # Errors
///
/// Returns an error when the log or demo-state directories cannot be created.
fn ensure_host_dirs(config: &SoakConfig) -> Result<(), DynError> {
    fs::create_dir_all(&config.log_dir)?;
    fs::create_dir_all(&config.demo_state_dir)?;
    Ok(())
}

/// Resets a directory to an empty state.
///
/// # Errors
///
/// Returns an error when removal or re-creation fails.
fn reset_dir(path: &Path) -> Result<(), DynError> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    fs::create_dir_all(path)?;
    Ok(())
}

/// Removes a file if it exists.
///
/// # Errors
///
/// Returns an error when the file exists but cannot be removed.
fn remove_if_exists(path: &Path) -> Result<(), DynError> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Best-effort cleanup for stale example processes from previous interrupted
/// runs.
///
/// # Errors
///
/// Returns an error when `pkill` fails unexpectedly.
fn cleanup_stale_processes(binary_name: &str) -> Result<(), DynError> {
    let status = Command::new("pkill").args(["-x", binary_name]).status()?;
    if status.success() || status.code() == Some(1) {
        return Ok(());
    }
    Err(io::Error::other(format!("pkill failed for {binary_name}: {status}")).into())
}

/// Allocates an ephemeral loopback TCP port for the observability endpoint.
///
/// # Errors
///
/// Returns an error when the listener cannot be created.
fn allocate_loopback_port() -> Result<u16, DynError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

/// Waits until the entire runtime port range can be rebound locally.
///
/// # Errors
///
/// Returns an error when the port range does not become available before the
/// timeout.
fn wait_for_port_range_release(range: PortRange, timeout: Duration) -> Result<(), DynError> {
    let started_at = Instant::now();
    loop {
        if can_bind_port_range(range) {
            return Ok(());
        }
        if started_at.elapsed() >= timeout {
            return Err(io::Error::other(format!(
                "port range {} did not become available within {}s",
                range.as_env_value(),
                timeout.as_secs()
            ))
            .into());
        }
        thread::sleep(Duration::from_secs(1));
    }
}

/// Returns whether both TCP and UDP binds succeed across the full port range.
///
fn can_bind_port_range(range: PortRange) -> bool {
    for port in range.start..=range.end {
        match TcpListener::bind(("0.0.0.0", port)) {
            Ok(listener) => drop(listener),
            Err(_error) => return false,
        }
        match UdpSocket::bind(("0.0.0.0", port)) {
            Ok(socket) => drop(socket),
            Err(_error) => return false,
        }
    }
    true
}

/// Reads the last matching line from a log file.
///
/// # Errors
///
/// Returns an error when the file cannot be read or the pattern is absent.
fn require_last_matching_line(path: &Path, needle: &str) -> Result<String, DynError> {
    let contents = fs::read_to_string(path)?;
    let Some(line) = contents.lines().rev().find(|line| line.contains(needle)) else {
        return Err(io::Error::other(format!(
            "missing log line containing '{needle}' in {}",
            path.display()
        ))
        .into());
    };
    Ok(line.to_owned())
}

/// Waits for a log line to appear within a timeout.
///
/// # Errors
///
/// Returns an error when the line does not appear before the timeout or the log
/// file cannot be read.
fn wait_for_log_line(path: &Path, needle: &str, timeout: Duration) -> Result<(), DynError> {
    let started_at = Instant::now();
    loop {
        if file_contains(path, needle)? {
            return Ok(());
        }
        if started_at.elapsed() >= timeout {
            return Err(io::Error::other(format!(
                "missing log line '{needle}' in {} after {}s",
                path.display(),
                timeout.as_secs()
            ))
            .into());
        }
        thread::sleep(Duration::from_secs(1));
    }
}

/// Returns whether a file contains the given substring.
///
/// # Errors
///
/// Returns an error when the file exists but cannot be read.
fn file_contains(path: &Path, needle: &str) -> Result<bool, DynError> {
    if !path.exists() {
        return Ok(false);
    }
    let contents = fs::read_to_string(path)?;
    Ok(contents.contains(needle))
}

/// Returns whether a file contains all required substrings.
///
/// # Errors
///
/// Returns an error when the file exists but cannot be read.
fn file_contains_all(path: &Path, needles: &[&str]) -> Result<bool, DynError> {
    if !path.exists() {
        return Ok(false);
    }
    let contents = fs::read_to_string(path)?;
    Ok(needles.iter().all(|needle| contents.contains(needle)))
}

/// Returns whether a file contains any of the provided substrings.
///
/// # Errors
///
/// Returns an error when the file exists but cannot be read.
fn file_contains_any(path: &Path, needles: &[&str]) -> Result<bool, DynError> {
    if !path.exists() {
        return Ok(false);
    }
    let contents = fs::read_to_string(path)?;
    Ok(needles.iter().any(|needle| contents.contains(needle)))
}

/// Parses a `key=value` token from a structured log line.
///
/// # Errors
///
/// Returns an error when the metric token is absent.
fn extract_line_metric(line: &str, key: &str) -> Result<String, DynError> {
    for token in strip_ansi(line).split_whitespace() {
        if let Some(value) = token.strip_prefix(&format!("{key}=")) {
            return Ok(String::from(value.trim_end_matches('i')));
        }
    }
    Err(io::Error::other(format!("missing metric '{key}' in line: {line}")).into())
}

/// Verifies a structured log metric equals the expected value.
///
/// # Errors
///
/// Returns an error when the metric is absent or does not match.
fn require_line_metric_value(line: &str, key: &str, expected: &str) -> Result<(), DynError> {
    let actual = extract_line_metric(line, key)?;
    require_metric_value(&actual, key, expected)
}

/// Verifies a plain metric value equals the expected string.
///
/// # Errors
///
/// Returns an error when the value does not match.
fn require_metric_value(actual: &str, metric_name: &str, expected: &str) -> Result<(), DynError> {
    if actual == expected {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "unexpected {metric_name}: expected {expected}, got {actual}"
    ))
    .into())
}

/// Reads the derived-state checkpoint sequence from the persisted JSON file.
///
/// # Errors
///
/// Returns an error when the file cannot be read or the JSON shape is invalid.
fn read_checkpoint_sequence(checkpoint_path: &Path) -> Result<u64, DynError> {
    let contents = fs::read_to_string(checkpoint_path)?;
    let value: Value = serde_json::from_str(&contents)?;
    let Some(sequence) = value.get("last_applied_sequence").and_then(Value::as_u64) else {
        return Err(io::Error::other(format!(
            "missing last_applied_sequence in {}",
            checkpoint_path.display()
        ))
        .into());
    };
    Ok(sequence)
}

/// Waits until the checkpoint sequence exceeds the provided baseline.
///
/// # Errors
///
/// Returns an error when the checkpoint does not advance before the timeout.
fn wait_for_checkpoint_advance(
    checkpoint_path: &Path,
    baseline_sequence: u64,
    timeout: Duration,
) -> Result<u64, DynError> {
    let started_at = Instant::now();
    loop {
        if checkpoint_path.exists() {
            let observed = read_checkpoint_sequence(checkpoint_path)?;
            if observed > baseline_sequence {
                return Ok(observed);
            }
        }
        if started_at.elapsed() >= timeout {
            return Err(io::Error::other(format!(
                "checkpoint did not advance beyond {} within {}s",
                baseline_sequence,
                timeout.as_secs()
            ))
            .into());
        }
        thread::sleep(Duration::from_secs(2));
    }
}

/// Waits until a Prometheus metric is present and returns its value.
///
/// # Errors
///
/// Returns an error when the metric does not appear before the timeout.
fn wait_for_prometheus_metric(
    observability_bind: &str,
    metric_name: &str,
    timeout: Duration,
) -> Result<String, DynError> {
    let started_at = Instant::now();
    loop {
        match scrape_prometheus_metric(observability_bind, metric_name) {
            Ok(value) => return Ok(value),
            Err(_error) => {
                if started_at.elapsed() >= timeout {
                    return Err(io::Error::other(format!(
                        "metric {metric_name} did not appear within {}s",
                        timeout.as_secs()
                    ))
                    .into());
                }
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// Scrapes a Prometheus metric from the local observability endpoint.
///
/// # Errors
///
/// Returns an error when the endpoint cannot be reached or the metric is absent.
fn scrape_prometheus_metric(
    observability_bind: &str,
    metric_name: &str,
) -> Result<String, DynError> {
    let body = http_get(observability_bind, "/metrics")?;
    for line in body.lines() {
        if line.starts_with(&format!("{metric_name} "))
            || line.starts_with(&format!("{metric_name}{{"))
        {
            let Some(value) = line.split_whitespace().last() else {
                return Err(
                    io::Error::other(format!("missing value for metric {metric_name}")).into(),
                );
            };
            return Ok(String::from(value));
        }
    }
    Err(io::Error::other(format!("metric {metric_name} not found")).into())
}

/// Performs a minimal HTTP GET against a local loopback endpoint.
///
/// # Errors
///
/// Returns an error when the socket cannot connect, the request cannot be sent,
/// or the response cannot be parsed.
fn http_get(bind_addr: &str, path: &str) -> Result<String, DynError> {
    let mut stream = TcpStream::connect(bind_addr)?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {bind_addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;
    stream.shutdown(Shutdown::Write)?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let Some((header, body)) = response.split_once("\r\n\r\n") else {
        return Err(io::Error::other("invalid HTTP response from observability endpoint").into());
    };
    if !header.starts_with("HTTP/1.1 200") && !header.starts_with("HTTP/1.0 200") {
        return Err(io::Error::other(format!(
            "unexpected HTTP status from observability endpoint: {header}"
        ))
        .into());
    }
    Ok(String::from(body))
}

/// Waits for a child process to exit before the timeout expires.
///
/// # Errors
///
/// Returns an error when the child does not exit in time or waiting fails.
fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> Result<ExitStatus, DynError> {
    let started_at = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started_at.elapsed() >= timeout {
            return Err(io::Error::other(format!(
                "child did not exit within {}s",
                timeout.as_secs()
            ))
            .into());
        }
        thread::sleep(Duration::from_secs(1));
    }
}

/// Sends a signal to a running child process.
///
/// # Errors
///
/// Returns an error when signal delivery fails.
fn kill_child(child: &mut Child, signal: Signal) -> Result<(), DynError> {
    let raw_pid = i32::try_from(child.id())?;
    kill(Pid::from_raw(raw_pid), signal)?;
    Ok(())
}

/// Sends a signal to a child only when it is still running.
///
/// # Errors
///
/// Returns an error when status probing or signal delivery fails.
fn kill_child_if_running(child: &mut Child, signal: Signal) -> Result<(), DynError> {
    if child.try_wait()?.is_none() {
        kill_child(child, signal)?;
    }
    Ok(())
}

/// Returns runtime environment variables for observer scenarios.
#[must_use]
fn observer_environment(
    config: &SoakConfig,
    observability_bind: &str,
    extra_env: &[(String, String)],
) -> Vec<(String, String)> {
    let mut environment = vec![
        (String::from("RUST_LOG"), config.runtime_log_level.clone()),
        (
            String::from("SOF_TUNING_PRESET"),
            config.tuning_preset.clone(),
        ),
        (
            String::from("SOF_GOSSIP_ENTRYPOINT"),
            config.gossip_entrypoint.clone(),
        ),
        (
            String::from("SOF_PORT_RANGE"),
            config.port_range.as_env_value(),
        ),
        (
            String::from("SOF_SHRED_DEDUP_CAPACITY"),
            config.shred_dedup_capacity.to_string(),
        ),
        (
            String::from("SOF_OBSERVABILITY_BIND"),
            String::from(observability_bind),
        ),
    ];
    for (key, value) in extra_env {
        environment.push((key.clone(), value.clone()));
    }
    environment
}

/// Returns runtime environment variables for derived-state scenarios.
#[must_use]
fn derived_state_environment(
    config: &SoakConfig,
    observability_bind: Option<&str>,
) -> Vec<(String, String)> {
    let mut environment = vec![
        (String::from("RUST_LOG"), config.runtime_log_level.clone()),
        (
            String::from("SOF_GOSSIP_ENTRYPOINT"),
            config.gossip_entrypoint.clone(),
        ),
        (
            String::from("SOF_PORT_RANGE"),
            config.port_range.as_env_value(),
        ),
        (
            String::from("SOF_SHRED_DEDUP_CAPACITY"),
            config.shred_dedup_capacity.to_string(),
        ),
    ];
    if let Some(bind) = observability_bind {
        environment.push((String::from("SOF_OBSERVABILITY_BIND"), String::from(bind)));
    }
    environment
}

/// Resolves the repository root from the binary crate location.
///
/// # Errors
///
/// Returns an error when the manifest directory cannot be converted to a parent
/// repository path.
fn resolve_repo_dir() -> Result<PathBuf, DynError> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let Some(repo_dir) = manifest_dir.parent().and_then(Path::parent) else {
        return Err(io::Error::other(format!(
            "failed to resolve repo root from {}",
            manifest_dir.display()
        ))
        .into());
    };
    Ok(repo_dir.to_path_buf())
}

/// Reads an environment variable or falls back to a static default.
#[must_use]
fn env_or_default(key: &str, default_value: &str) -> String {
    env::var(key).unwrap_or_else(|_error| String::from(default_value))
}

/// Parses an environment variable or falls back to a default typed value.
///
/// # Errors
///
/// Returns an error when the environment variable exists but cannot be parsed.
fn parse_env_or_default<T>(key: &str, default_value: T) -> Result<T, DynError>
where
    T: std::str::FromStr,
    T::Err: Error + Send + Sync + 'static,
{
    match env::var(key) {
        Ok(raw) => Ok(raw.parse::<T>()?),
        Err(_error) => Ok(default_value),
    }
}

/// Requires a child-process status to be successful.
///
/// # Errors
///
/// Returns an error when the status is non-zero.
fn require_success(status: ExitStatus, context: &str) -> Result<(), DynError> {
    if status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!("{context} failed with status {status}")).into())
}

/// Removes ANSI escape sequences from a structured log line.
#[must_use]
fn strip_ansi(line: &str) -> String {
    let mut stripped = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if matches!(chars.peek(), Some('[')) {
                let _consumed_bracket = chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        stripped.push(ch);
    }
    stripped
}
