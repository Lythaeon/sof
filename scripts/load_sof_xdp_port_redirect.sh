#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <dev> <bpf-object> [attach-type]" >&2
  echo "example: $0 eth0 ./scripts/sof_xdp_port_redirect.bpf.o xdpgeneric" >&2
  exit 1
fi

DEV="$1"
OBJ="$2"
ATTACH_TYPE="${3:-xdpgeneric}"
MAP_PIN_DIR="/sys/fs/bpf"
PROG_PIN="/sys/fs/bpf/sof_xdp_port_redirect"

sudo -n mkdir -p /sys/fs/bpf
sudo -n rm -f "${PROG_PIN}"
sudo -n bpftool prog load "${OBJ}" "${PROG_PIN}" type xdp pinmaps "${MAP_PIN_DIR}"
sudo -n bpftool net attach "${ATTACH_TYPE}" pinned "${PROG_PIN}" dev "${DEV}" overwrite

echo "attached ${PROG_PIN} to ${DEV} via ${ATTACH_TYPE}"
echo "pinned xsks_map at ${MAP_PIN_DIR}/xsks_map"
