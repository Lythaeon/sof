#!/usr/bin/env bash
set -euo pipefail

SOF_VPS_HOST="${SOF_VPS_HOST:-91.99.102.201}"
SOF_VPS_USER="${SOF_VPS_USER:-sof}"
SOF_VPS_KEY="${SOF_VPS_KEY:-$HOME/.ssh/sof-vps}"
SOF_VPS_BASE_DIR="${SOF_VPS_BASE_DIR:-/home/sof/sof}"
SOF_VPS_LOG_DIR="${SOF_VPS_LOG_DIR:-$SOF_VPS_BASE_DIR/logs/soak-validation}"
SOF_VPS_DEMO_STATE_DIR="${SOF_VPS_DEMO_STATE_DIR:-$SOF_VPS_BASE_DIR/demo-state}"
SOF_GOSSIP_ENTRYPOINT="${SOF_GOSSIP_ENTRYPOINT:-64.130.50.23:8001}"
SOF_PORT_RANGE="${SOF_PORT_RANGE:-12000-12100}"
SOF_SHRED_DEDUP_CAPACITY="${SOF_SHRED_DEDUP_CAPACITY:-524288}"
SOF_RUNTIME_LOG_LEVEL="${SOF_RUNTIME_LOG_LEVEL:-info}"

SOF_VPS_TARGET="${SOF_VPS_USER}@${SOF_VPS_HOST}"

ssh_opts=(
  -i "$SOF_VPS_KEY"
  -o BatchMode=yes
  -o StrictHostKeyChecking=accept-new
)

quote() {
  printf '%q' "$1"
}

ssh_run() {
  ssh "${ssh_opts[@]}" "$SOF_VPS_TARGET" "$@"
}

ssh_script() {
  ssh "${ssh_opts[@]}" "$SOF_VPS_TARGET" 'bash -s'
}

ensure_remote_dirs() {
  ssh_run "mkdir -p $(quote "$SOF_VPS_LOG_DIR") $(quote "$SOF_VPS_DEMO_STATE_DIR")"
}

wait_for_remote_log_line() {
  local log_path="$1"
  local pattern="$2"
  local timeout_secs="$3"
  local elapsed=0

  while (( elapsed < timeout_secs )); do
    if ssh_run "test -f $(quote "$log_path") && grep -qF $(quote "$pattern") $(quote "$log_path")"; then
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done

  return 1
}

wait_for_remote_pid_exit() {
  local pidfile_path="$1"
  local timeout_secs="$2"
  local elapsed=0

  while (( elapsed < timeout_secs )); do
    if ssh_run "
      if [[ ! -f $(quote "$pidfile_path") ]]; then
        exit 0
      fi
      pid=\$(cat $(quote "$pidfile_path"))
      if [[ -z \"\$pid\" ]]; then
        exit 0
      fi
      state=\$(ps -o stat= -p \"\$pid\" 2>/dev/null | tr -d '[:space:]')
      if [[ -z \"\$state\" ]]; then
        exit 0
      fi
      if [[ \"\$state\" == Z* ]]; then
        exit 0
      fi
      exit 1
    "; then
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done

  return 1
}

remote_last_matching_line() {
  local log_path="$1"
  local pattern="$2"
  ssh_run "grep -F $(quote "$pattern") $(quote "$log_path") | tail -n 1"
}

extract_metric() {
  local line="$1"
  local key="$2"
  local clean_line
  local value

  clean_line="$(sed -E $'s/\x1B\\[[0-9;]*[[:alpha:]]//g' <<<"$line")"
  value="$(sed -n "s/.*${key}=\\([^ ]*\\).*/\\1/p" <<<"$clean_line")"
  printf '%s\n' "${value%i}"
}

assert_metric_equals() {
  local line="$1"
  local key="$2"
  local expected="$3"
  local value

  value="$(extract_metric "$line" "$key")"
  if [[ -z "$value" ]]; then
    printf 'missing metric %s in line:\n%s\n' "$key" "$line" >&2
    return 1
  fi
  if [[ "$value" != "$expected" ]]; then
    printf 'unexpected %s: expected %s, got %s\nline: %s\n' "$key" "$expected" "$value" "$line" >&2
    return 1
  fi
}
