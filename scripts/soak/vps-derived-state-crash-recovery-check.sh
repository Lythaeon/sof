#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/vps-common.sh"

SOF_DERIVED_STATE_CRASH_RUN_SECS="${SOF_DERIVED_STATE_CRASH_RUN_SECS:-20}"
SOF_DERIVED_STATE_CRASH_STARTUP_TIMEOUT_SECS="${SOF_DERIVED_STATE_CRASH_STARTUP_TIMEOUT_SECS:-90}"
SOF_DERIVED_STATE_CRASH_SHUTDOWN_TIMEOUT_SECS="${SOF_DERIVED_STATE_CRASH_SHUTDOWN_TIMEOUT_SECS:-30}"
SOF_DERIVED_STATE_CRASH_PROGRESS_TIMEOUT_SECS="${SOF_DERIVED_STATE_CRASH_PROGRESS_TIMEOUT_SECS:-60}"
SOF_DERIVED_STATE_CRASH_STARTUP_RETRIES="${SOF_DERIVED_STATE_CRASH_STARTUP_RETRIES:-3}"

acquire_remote_soak_lock
ensure_remote_dirs
build_and_sync_example "derived_state_slot_mirror"
SOF_DERIVED_STATE_OBSERVABILITY_BIND="${SOF_DERIVED_STATE_OBSERVABILITY_BIND:-127.0.0.1:$(allocate_remote_loopback_port)}"

pidfile_path="${SOF_SOAK_LOG_DIR}/derived_state_crash_recovery.pid"
workdir_path="${SOF_SOAK_DEMO_STATE_DIR}/derived_state_slot_mirror"
checkpoint_path="${workdir_path}/.sof-example/slot-mirror-checkpoint.json"
first_log_path="${SOF_SOAK_LOG_DIR}/derived_state_crash_recovery_first.log"
second_log_path="${SOF_SOAK_LOG_DIR}/derived_state_crash_recovery_second.log"

read_checkpoint_sequence() {
  ssh_run "
    if [[ ! -f $(quote "$checkpoint_path") ]]; then
      exit 1
    fi
    grep -o '\"last_applied_sequence\":[[:space:]]*[0-9]*' $(quote "$checkpoint_path") | grep -o '[0-9]*' | head -n 1
  "
}

wait_for_checkpoint_advance() {
  local baseline_sequence="$1"
  local timeout_secs="$2"
  local elapsed=0

  while (( elapsed < timeout_secs )); do
    local observed_sequence
    observed_sequence="$(read_checkpoint_sequence 2>/dev/null || true)"
    if [[ -n "$observed_sequence" ]] && (( observed_sequence > baseline_sequence )); then
      printf '%s\n' "$observed_sequence"
      return 0
    fi
    sleep 2
    elapsed=$((elapsed + 2))
  done

  return 1
}

start_consumer() {
  local log_path="$1"
  ssh_script <<EOF
set -euo pipefail
pkill -f $(quote "$SOF_SOAK_BASE_DIR/derived_state_slot_mirror") 2>/dev/null || true
rm -f $(quote "$pidfile_path")
EOF

  wait_for_remote_port_range_release "$SOF_PORT_RANGE" "$SOF_DERIVED_STATE_CRASH_SHUTDOWN_TIMEOUT_SECS"

  ssh_script <<EOF
set -euo pipefail
rm -f $(quote "$pidfile_path")
mkdir -p $(quote "$workdir_path")
cd $(quote "$workdir_path")
nohup env \
  RUST_LOG=$(quote "$SOF_RUNTIME_LOG_LEVEL") \
  SOF_RUN_EXAMPLE=1 \
  SOF_GOSSIP_ENTRYPOINT=$(quote "$SOF_GOSSIP_ENTRYPOINT") \
  SOF_PORT_RANGE=$(quote "$SOF_PORT_RANGE") \
  SOF_SHRED_DEDUP_CAPACITY=$(quote "$SOF_SHRED_DEDUP_CAPACITY") \
  SOF_OBSERVABILITY_BIND=$(quote "$SOF_DERIVED_STATE_OBSERVABILITY_BIND") \
  $(quote "$SOF_SOAK_BASE_DIR/derived_state_slot_mirror") \
  >> $(quote "$log_path") 2>&1 &
echo \$! > $(quote "$pidfile_path")
EOF
}

wait_for_consumer_startup() {
  local log_path="$1"
  local attempt=1

  while (( attempt <= SOF_DERIVED_STATE_CRASH_STARTUP_RETRIES )); do
    ssh_run "rm -f $(quote "$log_path")"
    start_consumer "$log_path"

    local elapsed=0
    while (( elapsed < SOF_DERIVED_STATE_CRASH_STARTUP_TIMEOUT_SECS )); do
      if ssh_run "
        test -f $(quote "$log_path") \
          && grep -qF 'receiver bootstrap completed' $(quote "$log_path") \
          && grep -qF 'derived-state consumer startup completed' $(quote "$log_path") \
          && grep -qF 'derived-state consumers enabled' $(quote "$log_path")
      "; then
        return 0
      fi

      if ssh_run "
        test -f $(quote "$log_path") \
          && {
            grep -qF 'receiver runtime bootstrap failed' $(quote "$log_path") \
            || grep -qF 'Address already in use' $(quote "$log_path");
          }
      "; then
        break
      fi

      if ! remote_pid_is_running "$pidfile_path"; then
        break
      fi

      sleep 1
      elapsed=$((elapsed + 1))
    done

    if (( attempt == SOF_DERIVED_STATE_CRASH_STARTUP_RETRIES )); then
      printf 'derived-state crash recovery startup failed after %s attempts; last log: %s\n' \
        "$SOF_DERIVED_STATE_CRASH_STARTUP_RETRIES" "$log_path" >&2
      return 1
    fi

    printf 'startup attempt %s/%s failed; retrying\n' \
      "$attempt" "$SOF_DERIVED_STATE_CRASH_STARTUP_RETRIES"
    wait_for_remote_pid_exit "$pidfile_path" "$SOF_DERIVED_STATE_CRASH_SHUTDOWN_TIMEOUT_SECS" || true
    wait_for_remote_port_range_release "$SOF_PORT_RANGE" "$SOF_DERIVED_STATE_CRASH_SHUTDOWN_TIMEOUT_SECS" || true
    attempt=$((attempt + 1))
  done
}

stop_consumer_gracefully() {
  local log_path="$1"
  ssh_run "
    pid=\$(cat $(quote "$pidfile_path"))
    kill -TERM \"\$pid\"
  "
  wait_for_remote_log_line "$log_path" "observer runtime shutdown signal received" "$SOF_DERIVED_STATE_CRASH_SHUTDOWN_TIMEOUT_SECS"
  wait_for_remote_log_line "$log_path" "derived-state consumer shutdown completed" "$SOF_DERIVED_STATE_CRASH_SHUTDOWN_TIMEOUT_SECS"
  wait_for_remote_pid_exit "$pidfile_path" "$SOF_DERIVED_STATE_CRASH_SHUTDOWN_TIMEOUT_SECS"
  wait_for_remote_port_range_release "$SOF_PORT_RANGE" "$SOF_DERIVED_STATE_CRASH_SHUTDOWN_TIMEOUT_SECS"
}

crash_consumer() {
  ssh_run "
    pid=\$(cat $(quote "$pidfile_path"))
    kill -KILL \"\$pid\"
  "
  wait_for_remote_pid_exit "$pidfile_path" "$SOF_DERIVED_STATE_CRASH_SHUTDOWN_TIMEOUT_SECS"
  wait_for_remote_port_range_release "$SOF_PORT_RANGE" "$SOF_DERIVED_STATE_CRASH_SHUTDOWN_TIMEOUT_SECS"
}

scrape_prometheus_metric() {
  local metric_name="$1"
  ssh_run "
    curl -fsS http://$(quote "$SOF_DERIVED_STATE_OBSERVABILITY_BIND")/metrics \
      | awk '/^${metric_name}(\\{| )/ { print \$NF; exit }'
  "
}

wait_for_prometheus_metric() {
  local metric_name="$1"
  local timeout_secs="$2"
  local elapsed=0

  while (( elapsed < timeout_secs )); do
    local metric_value
    metric_value="$(scrape_prometheus_metric "$metric_name" 2>/dev/null || true)"
    if [[ -n "$metric_value" ]]; then
      printf '%s\n' "$metric_value"
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done

  return 1
}

printf 'Running derived-state crash recovery check on %s\n' "$SOF_SOAK_HOST"

ssh_script <<EOF
set -euo pipefail
rm -rf $(quote "$workdir_path")
rm -f $(quote "$first_log_path") $(quote "$second_log_path") $(quote "$pidfile_path")
mkdir -p $(quote "$workdir_path")
EOF

wait_for_consumer_startup "$first_log_path"
sleep "$SOF_DERIVED_STATE_CRASH_RUN_SECS"

first_sequence="$(read_checkpoint_sequence)"
[[ -n "$first_sequence" ]]
printf 'first checkpoint sequence before crash: %s\n' "$first_sequence"

crash_consumer
printf 'forced crash completed\n'

wait_for_consumer_startup "$second_log_path"
second_sequence="$(wait_for_checkpoint_advance "$first_sequence" "$SOF_DERIVED_STATE_CRASH_PROGRESS_TIMEOUT_SECS" || true)"
if [[ -z "$second_sequence" ]]; then
  printf 'checkpoint did not advance after crash recovery within %ss (baseline=%s)\n' \
    "$SOF_DERIVED_STATE_CRASH_PROGRESS_TIMEOUT_SECS" "$first_sequence" >&2
  exit 1
fi

healthy_consumers="$(wait_for_prometheus_metric "sof_derived_state_healthy_consumers" 30)"
fault_total="$(wait_for_prometheus_metric "sof_derived_state_fault_total" 30)"
replay_append_failures="$(wait_for_prometheus_metric "sof_derived_state_replay_append_failures_total" 30)"
replay_load_failures="$(wait_for_prometheus_metric "sof_derived_state_replay_load_failures_total" 30)"
runtime_ready="$(wait_for_prometheus_metric "sof_runtime_ready" 30)"
ingest_dropped_packets="$(wait_for_prometheus_metric "sof_ingest_dropped_packets_total" 30)"
dataset_queue_drops="$(wait_for_prometheus_metric "sof_dataset_queue_dropped_jobs_total" 30)"

[[ "$healthy_consumers" == "1" ]]
[[ "$fault_total" == "0" ]]
[[ "$replay_append_failures" == "0" ]]
[[ "$replay_load_failures" == "0" ]]
[[ "$runtime_ready" == "1" ]]
[[ "$ingest_dropped_packets" == "0" ]]
[[ "$dataset_queue_drops" == "0" ]]

printf 'checkpoint advanced after recovery: %s -> %s\n' "$first_sequence" "$second_sequence"
printf 'metrics ok: healthy_consumers=%s fault_total=%s replay_append_failures=%s replay_load_failures=%s runtime_ready=%s\n' \
  "$healthy_consumers" "$fault_total" "$replay_append_failures" "$replay_load_failures" "$runtime_ready"
printf 'queue health ok: ingest_dropped_packets=%s dataset_queue_drops=%s\n' \
  "$ingest_dropped_packets" "$dataset_queue_drops"

stop_consumer_gracefully "$second_log_path"
printf 'derived-state crash recovery check completed successfully\n'
