#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/vps-common.sh"

SOF_SOAK_CYCLES="${SOF_SOAK_CYCLES:-2}"
SOF_SOAK_RUN_SECS="${SOF_SOAK_RUN_SECS:-75}"
SOF_SOAK_STARTUP_TIMEOUT_SECS="${SOF_SOAK_STARTUP_TIMEOUT_SECS:-90}"
SOF_SOAK_SHUTDOWN_TIMEOUT_SECS="${SOF_SOAK_SHUTDOWN_TIMEOUT_SECS:-30}"

ensure_remote_dirs

printf 'Running observer restart soak on %s for %s cycles (%ss live window each)\n' \
  "$SOF_VPS_HOST" "$SOF_SOAK_CYCLES" "$SOF_SOAK_RUN_SECS"

for cycle in $(seq 1 "$SOF_SOAK_CYCLES"); do
  log_path="${SOF_VPS_LOG_DIR}/observer_runtime_cycle_${cycle}.log"
  pidfile_path="${SOF_VPS_LOG_DIR}/observer_runtime_cycle_${cycle}.pid"

  printf '\n== cycle %s ==\n' "$cycle"

  ssh_script <<EOF
set -euo pipefail
pkill -f $(quote "$SOF_VPS_BASE_DIR/observer_runtime") 2>/dev/null || true
rm -f $(quote "$log_path") $(quote "$pidfile_path")
cd $(quote "$SOF_VPS_BASE_DIR")
nohup env \
  RUST_LOG=$(quote "$SOF_RUNTIME_LOG_LEVEL") \
  SOF_TUNING_PRESET=vps \
  SOF_GOSSIP_ENTRYPOINT=$(quote "$SOF_GOSSIP_ENTRYPOINT") \
  SOF_PORT_RANGE=$(quote "$SOF_PORT_RANGE") \
  SOF_SHRED_DEDUP_CAPACITY=$(quote "$SOF_SHRED_DEDUP_CAPACITY") \
  $(quote "$SOF_VPS_BASE_DIR/observer_runtime") \
  >> $(quote "$log_path") 2>&1 &
echo \$! > $(quote "$pidfile_path")
EOF

  wait_for_remote_log_line "$log_path" "receiver bootstrap completed" "$SOF_SOAK_STARTUP_TIMEOUT_SECS"
  printf 'bootstrap completed\n'

  sleep "$SOF_SOAK_RUN_SECS"

  ingest_line="$(remote_last_matching_line "$log_path" "ingest telemetry")"
  receiver_line="$(remote_last_matching_line "$log_path" "gossip_receiver")"
  verify_line="$(remote_last_matching_line "$log_path" "gossip_socket_consume_verify_queue")"
  output_line="$(remote_last_matching_line "$log_path" "gossip_socket_consume_output_queue")"

  [[ -n "$ingest_line" ]]
  [[ -n "$receiver_line" ]]
  [[ -n "$verify_line" ]]
  [[ -n "$output_line" ]]

  assert_metric_equals "$ingest_line" "ingest_dropped_packets" "0"
  assert_metric_equals "$ingest_line" "dataset_queue_drops" "0"
  assert_metric_equals "$ingest_line" "packet_worker_dropped_packets" "0"
  assert_metric_equals "$ingest_line" "gossip_runtime_stall_age_ms" "0"
  assert_metric_equals "$receiver_line" "num_packets_dropped" "0"
  assert_metric_equals "$verify_line" "dropped_packets" "0"
  assert_metric_equals "$output_line" "dropped_packets" "0"

  printf 'ingest ok: latest_shred_age_ms=%s dedupe_entries=%s/%s\n' \
    "$(extract_metric "$ingest_line" "latest_shred_age_ms")" \
    "$(extract_metric "$ingest_line" "dedupe_entries")" \
    "$(extract_metric "$ingest_line" "dedupe_capacity")"
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
  printf 'shutdown completed\n'
done

printf '\nObserver restart soak completed successfully.\n'
