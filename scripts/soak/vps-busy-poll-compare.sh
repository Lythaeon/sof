#!/usr/bin/env bash
set -euo pipefail

SOF_VPS_HOST="${SOF_VPS_HOST:-91.99.102.201}"
SOF_VPS_USER="${SOF_VPS_USER:-sof}"
SOF_VPS_KEY="${SOF_VPS_KEY:-$HOME/.ssh/sof-vps}"
SOF_VPS_BASE_DIR="${SOF_VPS_BASE_DIR:-/home/sof/sof}"
SOF_GOSSIP_ENTRYPOINT="${SOF_GOSSIP_ENTRYPOINT:-64.130.50.23:8001}"
SOF_PORT_RANGE="${SOF_PORT_RANGE:-12000-12100}"
SOF_SHRED_DEDUP_CAPACITY="${SOF_SHRED_DEDUP_CAPACITY:-524288}"
SOF_TUNING_PRESET="${SOF_TUNING_PRESET:-vps}"
SOF_BUSY_POLL_US="${SOF_BUSY_POLL_US:-50}"
SOF_BUSY_POLL_BUDGET="${SOF_BUSY_POLL_BUDGET:-64}"
SOF_COMPARE_RUN_SECS="${SOF_COMPARE_RUN_SECS:-300}"
REMOTE_BINARY="${SOF_VPS_BASE_DIR}/observer_runtime_busy_poll_eval"
REMOTE_LOG_DIR="${SOF_VPS_BASE_DIR}/logs/busy-poll-validation"

ssh_base=(
  ssh
  -i "$SOF_VPS_KEY"
  -o BatchMode=yes
  -o StrictHostKeyChecking=accept-new
  "${SOF_VPS_USER}@${SOF_VPS_HOST}"
)
scp_base=(
  scp
  -i "$SOF_VPS_KEY"
  -o BatchMode=yes
  -o StrictHostKeyChecking=accept-new
)

ssh_run() {
  "${ssh_base[@]}" "$@"
}

extract_metric() {
  local line="$1"
  local key="$2"
  printf '%s\n' "$line" \
    | sed -E 's/\x1B\[[0-9;]*[[:alpha:]]//g' \
    | tr ' ' '\n' \
    | sed -n "s/^${key}=//p" \
    | tail -n1 \
    | sed -E 's/i$//'
}

remote_last_matching_line() {
  local path="$1"
  local needle="$2"
  ssh_run "grep -F '$needle' '$path' | tail -n 1 || true"
}

run_profile() {
  local profile_name="$1"
  local extra_env="$2"
  local log_path="${REMOTE_LOG_DIR}/${profile_name}.log"

  ssh_run "mkdir -p '$REMOTE_LOG_DIR' && rm -f '$log_path' && pgrep -f '^$REMOTE_BINARY$' | xargs -r kill || true"
  ssh_run "
    cd '$SOF_VPS_BASE_DIR' &&
    timeout -s TERM '${SOF_COMPARE_RUN_SECS}s' env \
      RUST_LOG=info \
      SOF_TUNING_PRESET='$SOF_TUNING_PRESET' \
      SOF_GOSSIP_ENTRYPOINT='$SOF_GOSSIP_ENTRYPOINT' \
      SOF_PORT_RANGE='$SOF_PORT_RANGE' \
      SOF_SHRED_DEDUP_CAPACITY='$SOF_SHRED_DEDUP_CAPACITY' \
      ${extra_env} \
      '$REMOTE_BINARY' > '$log_path' 2>&1
    status=\$?
    if [[ \$status -ne 0 && \$status -ne 124 ]]; then
      exit \$status
    fi
  "

  local ingest_line receiver_line verify_line output_line
  ingest_line="$(remote_last_matching_line "$log_path" "ingest telemetry")"
  receiver_line="$(remote_last_matching_line "$log_path" "gossip_receiver")"
  verify_line="$(remote_last_matching_line "$log_path" "gossip_socket_consume_verify_queue")"
  output_line="$(remote_last_matching_line "$log_path" "gossip_socket_consume_output_queue")"
  local direct_busy_poll="no"
  local gossip_busy_poll="no"
  if ssh_run "grep -Fq 'configured UDP busy-poll socket options' '$log_path'"; then
    direct_busy_poll="yes"
  fi
  if ssh_run "grep -Fq 'configured Linux UDP busy-poll socket options for observer-facing gossip sockets' '$log_path'"; then
    gossip_busy_poll="yes"
  fi

  printf '%s\n' "== ${profile_name} =="
  printf 'log=%s\n' "$log_path"
  printf 'direct_busy_poll=%s gossip_busy_poll=%s\n' "$direct_busy_poll" "$gossip_busy_poll"
  printf 'latest_shred_age_ms=%s latest_dataset_age_ms=%s gossip_runtime_stall_age_ms=%s\n' \
    "$(extract_metric "$ingest_line" latest_shred_age_ms)" \
    "$(extract_metric "$ingest_line" latest_dataset_age_ms)" \
    "$(extract_metric "$ingest_line" gossip_runtime_stall_age_ms)"
  printf 'ingest_dropped_packets=%s dataset_queue_drops=%s packet_worker_dropped_packets=%s\n' \
    "$(extract_metric "$ingest_line" ingest_dropped_packets)" \
    "$(extract_metric "$ingest_line" dataset_queue_drops)" \
    "$(extract_metric "$ingest_line" packet_worker_dropped_packets)"
  printf 'dedupe_entries=%s/%s dedupe_capacity_evictions=%s\n' \
    "$(extract_metric "$ingest_line" dedupe_entries)" \
    "$(extract_metric "$ingest_line" dedupe_capacity)" \
    "$(extract_metric "$ingest_line" dedupe_capacity_evictions)"
  printf 'receiver_channel_len=%s receiver_dropped_packets=%s\n' \
    "$(extract_metric "$receiver_line" channel_len)" \
    "$(extract_metric "$receiver_line" num_packets_dropped)"
  printf 'verify_queue_current=%s verify_queue_max=%s verify_dropped_packets=%s\n' \
    "$(extract_metric "$verify_line" current_len)" \
    "$(extract_metric "$verify_line" max_len)" \
    "$(extract_metric "$verify_line" dropped_packets)"
  printf 'output_queue_current=%s output_queue_max=%s output_dropped_packets=%s\n' \
    "$(extract_metric "$output_line" current_len)" \
    "$(extract_metric "$output_line" max_len)" \
    "$(extract_metric "$output_line" dropped_packets)"
}

echo "Building observer_runtime release example locally..."
cargo build --release -p sof --example observer_runtime --features gossip-bootstrap

echo "Uploading binary to VPS..."
ssh_run "mkdir -p '$SOF_VPS_BASE_DIR' '$REMOTE_LOG_DIR'"
"${scp_base[@]}" target/release/examples/observer_runtime "${SOF_VPS_USER}@${SOF_VPS_HOST}:${REMOTE_BINARY}"
ssh_run "chmod 755 '$REMOTE_BINARY'"

run_profile "baseline" ""
run_profile "busy_poll" "SOF_UDP_BUSY_POLL_US='$SOF_BUSY_POLL_US' SOF_UDP_BUSY_POLL_BUDGET='$SOF_BUSY_POLL_BUDGET' SOF_UDP_PREFER_BUSY_POLL=true"
