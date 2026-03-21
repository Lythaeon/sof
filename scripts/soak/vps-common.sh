#!/usr/bin/env bash
set -euo pipefail

SOF_SOAK_TRANSPORT="${SOF_SOAK_TRANSPORT:-ssh}"
SOF_SOAK_REPO_DIR="${SOF_SOAK_REPO_DIR:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)}"

# Backward-compatible aliases keep the older VPS-oriented names working, but
# the soak checks are intended to target any reachable public host profile.
SOF_SOAK_HOST="${SOF_SOAK_HOST:-${SOF_VPS_HOST:-91.99.102.201}}"
SOF_SOAK_USER="${SOF_SOAK_USER:-${SOF_VPS_USER:-sof}}"
SOF_SOAK_KEY="${SOF_SOAK_KEY:-${SOF_VPS_KEY:-$HOME/.ssh/sof-vps}}"
if [[ "$SOF_SOAK_TRANSPORT" == "local" ]]; then
  SOF_SOAK_BASE_DIR="${SOF_SOAK_BASE_DIR:-${SOF_VPS_BASE_DIR:-$SOF_SOAK_REPO_DIR}}"
else
  SOF_SOAK_BASE_DIR="${SOF_SOAK_BASE_DIR:-${SOF_VPS_BASE_DIR:-/home/sof/sof}}"
fi
SOF_SOAK_LOG_DIR="${SOF_SOAK_LOG_DIR:-${SOF_VPS_LOG_DIR:-$SOF_SOAK_BASE_DIR/logs/soak-validation}}"
SOF_SOAK_DEMO_STATE_DIR="${SOF_SOAK_DEMO_STATE_DIR:-${SOF_VPS_DEMO_STATE_DIR:-$SOF_SOAK_BASE_DIR/demo-state}}"
SOF_SOAK_LOCK_DIR="${SOF_SOAK_LOCK_DIR:-${SOF_VPS_LOCK_DIR:-$SOF_SOAK_BASE_DIR/.soak-lock}}"
SOF_SOAK_LOCK_TIMEOUT_SECS="${SOF_SOAK_LOCK_TIMEOUT_SECS:-60}"
SOF_GOSSIP_ENTRYPOINT="${SOF_GOSSIP_ENTRYPOINT:-64.130.50.23:8001}"
SOF_PORT_RANGE="${SOF_PORT_RANGE:-12000-12100}"
SOF_SHRED_DEDUP_CAPACITY="${SOF_SHRED_DEDUP_CAPACITY:-524288}"
SOF_RUNTIME_LOG_LEVEL="${SOF_RUNTIME_LOG_LEVEL:-info}"

SOF_SOAK_TARGET="${SOF_SOAK_USER}@${SOF_SOAK_HOST}"

ssh_opts=(
  -i "$SOF_SOAK_KEY"
  -o BatchMode=yes
  -o StrictHostKeyChecking=accept-new
)

scp_opts=(
  -i "$SOF_SOAK_KEY"
  -o BatchMode=yes
  -o StrictHostKeyChecking=accept-new
)

quote() {
  printf '%q' "$1"
}

transport_local() {
  [[ "$SOF_SOAK_TRANSPORT" == "local" ]]
}

ssh_run() {
  if transport_local; then
    bash -lc "$*"
  else
    ssh "${ssh_opts[@]}" "$SOF_SOAK_TARGET" "$@"
  fi
}

scp_to_remote() {
  if transport_local; then
    mkdir -p "$(dirname "$2")"
    cp "$1" "$2"
  else
    scp "${scp_opts[@]}" "$1" "$SOF_SOAK_TARGET:$2"
  fi
}

ssh_script() {
  if transport_local; then
    bash -s
  else
    ssh "${ssh_opts[@]}" "$SOF_SOAK_TARGET" 'bash -s'
  fi
}

ensure_remote_dirs() {
  ssh_run "mkdir -p $(quote "$SOF_SOAK_LOG_DIR") $(quote "$SOF_SOAK_DEMO_STATE_DIR")"
}

release_remote_soak_lock() {
  if [[ "${SOF_SOAK_LOCK_ACQUIRED:-0}" != "1" ]]; then
    return 0
  fi

  ssh_run "rm -rf $(quote "$SOF_SOAK_LOCK_DIR")" || true
  SOF_SOAK_LOCK_ACQUIRED=0
}

acquire_remote_soak_lock() {
  local elapsed=0

  while (( elapsed < SOF_SOAK_LOCK_TIMEOUT_SECS )); do
    if ssh_run "mkdir $(quote "$SOF_SOAK_LOCK_DIR")" 2>/dev/null; then
      SOF_SOAK_LOCK_ACQUIRED=1
      trap release_remote_soak_lock EXIT
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done

  printf 'timed out waiting for soak lock at %s\n' "$SOF_SOAK_LOCK_DIR" >&2
  return 1
}

allocate_remote_loopback_port() {
  ssh_run "python3 - <<'PY'
import socket

with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
    sock.bind(('127.0.0.1', 0))
    print(sock.getsockname()[1])
PY"
}

wait_for_remote_port_range_release() {
  local port_range="$1"
  local timeout_secs="$2"
  local range_start="${port_range%-*}"
  local range_end="${port_range#*-}"
  local elapsed=0

  while (( elapsed < timeout_secs )); do
    if ssh_run "python3 - <<'PY' $(quote "$range_start") $(quote "$range_end")
import socket
import sys

start = int(sys.argv[1])
end = int(sys.argv[2])

for port in range(start, end + 1):
    for sock_type in (socket.SOCK_STREAM, socket.SOCK_DGRAM):
        sock = socket.socket(socket.AF_INET, sock_type)
        try:
            sock.bind(('0.0.0.0', port))
        except OSError:
            sys.exit(1)
        finally:
            sock.close()

sys.exit(0)
PY"; then
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done

  return 1
}

build_and_sync_example() {
  local example_name="$1"
  local artifact_path="$SOF_SOAK_REPO_DIR/target/release/examples/${example_name}"
  local remote_path="${SOF_SOAK_BASE_DIR}/${example_name}"
  local staged_path="${remote_path}.staged.$$"

  (
    cd "$SOF_SOAK_REPO_DIR"
    cargo build --release -p sof --example "$example_name" --features gossip-bootstrap
  )
  scp_to_remote "$artifact_path" "$staged_path"
  ssh_run "
    chmod 755 $(quote "$staged_path")
    mv $(quote "$staged_path") $(quote "$remote_path")
  "
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

remote_pid_is_running() {
  local pidfile_path="$1"
  ssh_run "
    if [[ ! -f $(quote "$pidfile_path") ]]; then
      exit 1
    fi
    pid=\$(cat $(quote "$pidfile_path"))
    if [[ -z \"\$pid\" ]]; then
      exit 1
    fi
    state=\$(ps -o stat= -p \"\$pid\" 2>/dev/null | tr -d '[:space:]')
    if [[ -z \"\$state\" ]]; then
      exit 1
    fi
    if [[ \"\$state\" == Z* ]]; then
      exit 1
    fi
    exit 0
  "
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
