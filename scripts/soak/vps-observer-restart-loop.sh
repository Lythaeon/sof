#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/vps-common.sh"

SOF_SOAK_CYCLES="${SOF_SOAK_CYCLES:-2}"
SOF_SOAK_RUN_SECS="${SOF_SOAK_RUN_SECS:-75}"
SOF_SOAK_STARTUP_TIMEOUT_SECS="${SOF_SOAK_STARTUP_TIMEOUT_SECS:-90}"
SOF_SOAK_SHUTDOWN_TIMEOUT_SECS="${SOF_SOAK_SHUTDOWN_TIMEOUT_SECS:-30}"
SOF_SOAK_STARTUP_RETRIES="${SOF_SOAK_STARTUP_RETRIES:-3}"

acquire_remote_soak_lock
ensure_remote_dirs
build_and_sync_example "observer_runtime"
SOF_SOAK_OBSERVABILITY_BIND="${SOF_SOAK_OBSERVABILITY_BIND:-127.0.0.1:$(allocate_remote_loopback_port)}"

scrape_prometheus_metric() {
  local metric_name="$1"
  ssh_run "
    curl -fsS http://$(quote "$SOF_SOAK_OBSERVABILITY_BIND")/metrics \
      | awk '/^${metric_name} / { print \$2; exit }'
  "
}

printf 'Running observer restart soak on %s for %s cycles (%ss live window each)\n' \
  "$SOF_SOAK_HOST" "$SOF_SOAK_CYCLES" "$SOF_SOAK_RUN_SECS"

start_observer() {
  local log_path="$1"
  local pidfile_path="$2"

  ssh_script <<EOF
set -euo pipefail
pkill -f $(quote "$SOF_SOAK_BASE_DIR/observer_runtime") 2>/dev/null || true
rm -f $(quote "$log_path") $(quote "$pidfile_path")
EOF

  wait_for_remote_port_range_release "$SOF_PORT_RANGE" "$SOF_SOAK_SHUTDOWN_TIMEOUT_SECS"

  ssh_script <<EOF
set -euo pipefail
rm -f $(quote "$pidfile_path")
cd $(quote "$SOF_SOAK_BASE_DIR")
nohup env \
  RUST_LOG=$(quote "$SOF_RUNTIME_LOG_LEVEL") \
  SOF_TUNING_PRESET=vps \
  SOF_GOSSIP_ENTRYPOINT=$(quote "$SOF_GOSSIP_ENTRYPOINT") \
  SOF_PORT_RANGE=$(quote "$SOF_PORT_RANGE") \
  SOF_SHRED_DEDUP_CAPACITY=$(quote "$SOF_SHRED_DEDUP_CAPACITY") \
  SOF_OBSERVABILITY_BIND=$(quote "$SOF_SOAK_OBSERVABILITY_BIND") \
  $(quote "$SOF_SOAK_BASE_DIR/observer_runtime") \
  >> $(quote "$log_path") 2>&1 &
echo \$! > $(quote "$pidfile_path")
EOF
}

wait_for_observer_startup() {
  local log_path="$1"
  local pidfile_path="$2"
  local attempt=1

  while (( attempt <= SOF_SOAK_STARTUP_RETRIES )); do
    start_observer "$log_path" "$pidfile_path"

    local elapsed=0
    while (( elapsed < SOF_SOAK_STARTUP_TIMEOUT_SECS )); do
      if ssh_run "
        test -f $(quote "$log_path") \
          && grep -qF 'receiver bootstrap completed' $(quote "$log_path")
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

    if (( attempt == SOF_SOAK_STARTUP_RETRIES )); then
      printf 'observer failed to start after %s attempts; last log: %s\n' \
        "$SOF_SOAK_STARTUP_RETRIES" "$log_path" >&2
      return 1
    fi

    printf 'startup attempt %s/%s failed; retrying\n' \
      "$attempt" "$SOF_SOAK_STARTUP_RETRIES"
    wait_for_remote_pid_exit "$pidfile_path" "$SOF_SOAK_SHUTDOWN_TIMEOUT_SECS" || true
    wait_for_remote_port_range_release "$SOF_PORT_RANGE" "$SOF_SOAK_SHUTDOWN_TIMEOUT_SECS" || true
    attempt=$((attempt + 1))
  done
}

for cycle in $(seq 1 "$SOF_SOAK_CYCLES"); do
  log_path="${SOF_SOAK_LOG_DIR}/observer_runtime_cycle_${cycle}.log"
  pidfile_path="${SOF_SOAK_LOG_DIR}/observer_runtime_cycle_${cycle}.pid"

  printf '\n== cycle %s ==\n' "$cycle"

  wait_for_observer_startup "$log_path" "$pidfile_path"
  printf 'bootstrap completed\n'

  sleep "$SOF_SOAK_RUN_SECS"

  receiver_line="$(remote_last_matching_line "$log_path" "gossip_receiver")"
  verify_line="$(remote_last_matching_line "$log_path" "gossip_socket_consume_verify_queue")"
  output_line="$(remote_last_matching_line "$log_path" "gossip_socket_consume_output_queue")"
  runtime_ready="$(scrape_prometheus_metric "sof_runtime_ready")"
  ingest_dropped_packets="$(scrape_prometheus_metric "sof_ingest_dropped_packets_total")"
  dataset_queue_drops="$(scrape_prometheus_metric "sof_dataset_queue_dropped_jobs_total")"
  packet_worker_dropped_packets="$(scrape_prometheus_metric "sof_packet_worker_dropped_packets_total")"
  latest_shred_age_ms="$(scrape_prometheus_metric "sof_latest_shred_age_ms")"
  dedupe_entries="$(scrape_prometheus_metric "sof_shred_dedupe_entries")"
  dedupe_high_watermark="$(scrape_prometheus_metric "sof_shred_dedupe_max_entries")"
  gossip_runtime_stall_age_ms="$(scrape_prometheus_metric "sof_gossip_runtime_stall_age_ms")"

  [[ -n "$receiver_line" ]]
  [[ -n "$verify_line" ]]
  [[ -n "$output_line" ]]
  [[ "$runtime_ready" == "1" ]]
  [[ "$ingest_dropped_packets" == "0" ]]
  [[ "$dataset_queue_drops" == "0" ]]
  [[ "$packet_worker_dropped_packets" == "0" ]]
  [[ "$gossip_runtime_stall_age_ms" == "0" ]]

  assert_metric_equals "$receiver_line" "num_packets_dropped" "0"
  assert_metric_equals "$verify_line" "dropped_packets" "0"
  assert_metric_equals "$output_line" "dropped_packets" "0"

  printf 'ingest ok: latest_shred_age_ms=%s dedupe_entries=%s dedupe_high_watermark=%s\n' \
    "$latest_shred_age_ms" \
    "$dedupe_entries" \
    "$dedupe_high_watermark"
  printf 'gossip queues ok: receiver_len=%s verify_max_len=%s output_max_len=%s\n' \
    "$(extract_metric "$receiver_line" "channel_len")" \
    "$(extract_metric "$verify_line" "max_len")" \
    "$(extract_metric "$output_line" "max_len")"

  ssh_run "
    pid=\$(cat $(quote "$pidfile_path"))
    kill -TERM \"\$pid\"
  "
  wait_for_remote_log_line "$log_path" "observer runtime shutdown signal received" "$SOF_SOAK_SHUTDOWN_TIMEOUT_SECS"
  wait_for_remote_pid_exit "$pidfile_path" "$SOF_SOAK_SHUTDOWN_TIMEOUT_SECS"
  wait_for_remote_port_range_release "$SOF_PORT_RANGE" "$SOF_SOAK_SHUTDOWN_TIMEOUT_SECS"
  printf 'shutdown completed\n'
done

printf '\nObserver restart soak completed successfully.\n'
