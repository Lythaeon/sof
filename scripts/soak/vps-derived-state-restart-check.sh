#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/vps-common.sh"

SOF_DERIVED_STATE_RUN_SECS="${SOF_DERIVED_STATE_RUN_SECS:-45}"
SOF_DERIVED_STATE_STARTUP_TIMEOUT_SECS="${SOF_DERIVED_STATE_STARTUP_TIMEOUT_SECS:-90}"
SOF_DERIVED_STATE_SHUTDOWN_TIMEOUT_SECS="${SOF_DERIVED_STATE_SHUTDOWN_TIMEOUT_SECS:-30}"

ensure_remote_dirs

pidfile_path="${SOF_VPS_LOG_DIR}/derived_state_restart_check.pid"
workdir_path="${SOF_VPS_DEMO_STATE_DIR}/derived_state_slot_mirror"
checkpoint_path="${workdir_path}/.sof-example/slot-mirror-checkpoint.json"

read_checkpoint_sequence() {
  ssh_run "
    if [[ ! -f $(quote "$checkpoint_path") ]]; then
      exit 1
    fi
    grep -o '\"last_applied_sequence\":[[:space:]]*[0-9]*' $(quote "$checkpoint_path") | grep -o '[0-9]*' | head -n 1
  "
}

start_consumer() {
  local log_path="$1"
  ssh_script <<EOF
set -euo pipefail
pkill -f $(quote "$SOF_VPS_BASE_DIR/derived_state_slot_mirror") 2>/dev/null || true
rm -f $(quote "$pidfile_path")
mkdir -p $(quote "$workdir_path")
cd $(quote "$workdir_path")
nohup env \
  RUST_LOG=$(quote "$SOF_RUNTIME_LOG_LEVEL") \
  SOF_RUN_EXAMPLE=1 \
  SOF_GOSSIP_ENTRYPOINT=$(quote "$SOF_GOSSIP_ENTRYPOINT") \
  SOF_PORT_RANGE=$(quote "$SOF_PORT_RANGE") \
  SOF_SHRED_DEDUP_CAPACITY=$(quote "$SOF_SHRED_DEDUP_CAPACITY") \
  $(quote "$SOF_VPS_BASE_DIR/derived_state_slot_mirror") \
  >> $(quote "$log_path") 2>&1 &
echo \$! > $(quote "$pidfile_path")
EOF
}

stop_consumer() {
  local log_path="$1"
  ssh_run "
    pid=\$(cat $(quote "$pidfile_path"))
    kill -TERM \"\$pid\"
  "
  wait_for_remote_log_line "$log_path" "observer runtime shutdown signal received" "$SOF_DERIVED_STATE_SHUTDOWN_TIMEOUT_SECS"
  wait_for_remote_log_line "$log_path" "derived-state consumer shutdown completed" "$SOF_DERIVED_STATE_SHUTDOWN_TIMEOUT_SECS"
  wait_for_remote_pid_exit "$pidfile_path" "$SOF_DERIVED_STATE_SHUTDOWN_TIMEOUT_SECS"
}

printf 'Running derived-state restart check on %s\n' "$SOF_VPS_HOST"

first_log_path="${SOF_VPS_LOG_DIR}/derived_state_restart_check_first.log"
second_log_path="${SOF_VPS_LOG_DIR}/derived_state_restart_check_second.log"

ssh_script <<EOF
set -euo pipefail
rm -rf $(quote "$workdir_path")
rm -f $(quote "$first_log_path") $(quote "$second_log_path") $(quote "$pidfile_path")
mkdir -p $(quote "$workdir_path")
EOF

start_consumer "$first_log_path"
wait_for_remote_log_line "$first_log_path" "receiver bootstrap completed" "$SOF_DERIVED_STATE_STARTUP_TIMEOUT_SECS"
wait_for_remote_log_line "$first_log_path" "derived-state consumer startup completed" "$SOF_DERIVED_STATE_STARTUP_TIMEOUT_SECS"
sleep "$SOF_DERIVED_STATE_RUN_SECS"

first_sequence="$(read_checkpoint_sequence)"
[[ -n "$first_sequence" ]]
printf 'first checkpoint sequence: %s\n' "$first_sequence"

stop_consumer "$first_log_path"
printf 'first shutdown completed\n'

start_consumer "$second_log_path"
wait_for_remote_log_line "$second_log_path" "receiver bootstrap completed" "$SOF_DERIVED_STATE_STARTUP_TIMEOUT_SECS"
wait_for_remote_log_line "$second_log_path" "derived-state consumer startup completed" "$SOF_DERIVED_STATE_STARTUP_TIMEOUT_SECS"
sleep "$SOF_DERIVED_STATE_RUN_SECS"

second_sequence="$(read_checkpoint_sequence)"
[[ -n "$second_sequence" ]]
if (( second_sequence <= first_sequence )); then
  printf 'expected checkpoint sequence to advance across restart: first=%s second=%s\n' \
    "$first_sequence" "$second_sequence" >&2
  exit 1
fi

printf 'checkpoint advanced across restart: %s -> %s\n' "$first_sequence" "$second_sequence"

stop_consumer "$second_log_path"
printf 'derived-state restart check completed successfully\n'
