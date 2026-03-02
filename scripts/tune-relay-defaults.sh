#!/usr/bin/env bash
set -euo pipefail

if ! command -v rg >/dev/null 2>&1; then
  echo "ripgrep (rg) is required"
  exit 1
fi

BIN="${SOF_BIN:-target/debug/sof-observer}"
ENTRYPOINT="${SOF_GOSSIP_ENTRYPOINT:-85.195.118.195:8001}"
PORT_RANGE="${SOF_PORT_RANGE:-12000-12100}"
RUN_SECS="${SOF_TUNE_DURATION_SECS:-90}"
OUT_DIR="${SOF_TUNE_OUT_DIR:-/tmp/sof-relay-tune}"
RUN_VIA_CARGO="${SOF_RUN_VIA_CARGO:-0}"
RUN_RELEASE="${SOF_RUN_RELEASE:-1}"
RUN_ALL_FEATURES="${SOF_RUN_ALL_FEATURES:-1}"
RUN_EXAMPLE="${SOF_RUN_EXAMPLE:-non_vote_tx_logger}"

mkdir -p "${OUT_DIR}"
RESULTS_CSV="${OUT_DIR}/results.csv"
printf "profile,score,data,code,forwarded,send_attempts,rate_limited,repair_requests,latest_shred_age_ms,gossip_push_sent,gossip_req_sent,status,log\n" >"${RESULTS_CSV}"

parse_field() {
  local line="$1"
  local key="$2"
  sed -nE "s/.*\\b${key}=([^ ]+).*/\\1/p" <<<"${line}" | tr -d '"' | sed -E 's/i$//'
}

append_result() {
  local profile="$1"
  local ingest_line="$2"
  local gossip_line="$3"
  local status="$4"
  local logfile="$5"

  if [[ -z "${ingest_line}" ]]; then
    printf "%s,0,0,0,0,0,0,0,0,0,0,%s,%s\n" \
      "${profile}" "${status}" "${logfile}" >>"${RESULTS_CSV}"
    return
  fi

  local data code forwarded send_attempts rate_limited repair_requests latest_shred_age
  local gossip_push gossip_req score

  data="$(parse_field "${ingest_line}" "data")"
  code="$(parse_field "${ingest_line}" "code")"
  forwarded="$(parse_field "${ingest_line}" "udp_relay_forwarded_packets")"
  send_attempts="$(parse_field "${ingest_line}" "udp_relay_send_attempts")"
  rate_limited="$(parse_field "${ingest_line}" "udp_relay_rate_limited_packets")"
  repair_requests="$(parse_field "${ingest_line}" "repair_requests_sent")"
  latest_shred_age="$(parse_field "${ingest_line}" "latest_shred_age_ms")"
  gossip_push="$(parse_field "${gossip_line}" "packets_sent_push_messages_count")"
  gossip_req="$(parse_field "${gossip_line}" "packets_sent_gossip_requests_count")"

  data="${data:-0}"
  code="${code:-0}"
  forwarded="${forwarded:-0}"
  send_attempts="${send_attempts:-0}"
  rate_limited="${rate_limited:-0}"
  repair_requests="${repair_requests:-0}"
  latest_shred_age="${latest_shred_age:-0}"
  gossip_push="${gossip_push:-0}"
  gossip_req="${gossip_req:-0}"

  # Higher is better: prioritize stream coverage, then penalize control-plane/outbound pressure.
  score="$(
    awk \
      -v data="${data}" \
      -v code="${code}" \
      -v forwarded="${forwarded}" \
      -v sends="${send_attempts}" \
      -v limited="${rate_limited}" \
      -v repair="${repair_requests}" \
      -v age="${latest_shred_age}" \
      -v gpush="${gossip_push}" \
      -v greq="${gossip_req}" \
      'BEGIN {
         coverage = data + code;
         outbound = sends + limited + gpush + greq;
         score = (coverage / 1000.0) + (forwarded / 600.0) - (outbound / 12000.0) - (repair * 0.5) - (age / 200.0);
         printf "%.4f", score;
       }'
  )"

  printf "%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n" \
    "${profile}" "${score}" "${data}" "${code}" "${forwarded}" "${send_attempts}" \
    "${rate_limited}" "${repair_requests}" "${latest_shred_age}" "${gossip_push}" \
    "${gossip_req}" "${status}" "${logfile}" >>"${RESULTS_CSV}"
}

run_profile() {
  local profile="$1"
  shift
  local logfile="${OUT_DIR}/${profile}.log"
  local rc=0

  echo "== ${profile} (${RUN_SECS}s) =="
  if [[ "${RUN_VIA_CARGO}" == "1" ]]; then
    local -a cargo_cmd=(cargo run -q -p sof --example "${RUN_EXAMPLE}")
    if [[ "${RUN_RELEASE}" == "1" ]]; then
      cargo_cmd=(cargo run --release -q -p sof --example "${RUN_EXAMPLE}")
    fi
    if [[ "${RUN_ALL_FEATURES}" == "1" ]]; then
      cargo_cmd+=(--all-features)
    fi
    timeout "${RUN_SECS}s" env \
      RUST_LOG=info \
      SOF_GOSSIP_ENTRYPOINT="${ENTRYPOINT}" \
      SOF_PORT_RANGE="${PORT_RANGE}" \
      "$@" \
      "${cargo_cmd[@]}" >"${logfile}" 2>&1 || rc=$?
  else
    timeout "${RUN_SECS}s" env \
      RUST_LOG=info \
      SOF_GOSSIP_ENTRYPOINT="${ENTRYPOINT}" \
      SOF_PORT_RANGE="${PORT_RANGE}" \
      "$@" \
      "${BIN}" >"${logfile}" 2>&1 || rc=$?
  fi

  local status="ok"
  if [[ "${rc}" -ne 0 && "${rc}" -ne 124 ]]; then
    status="failed(rc=${rc})"
  fi
  local ingest_line gossip_line
  ingest_line="$(rg "ingest telemetry" "${logfile}" | tail -n 1 || true)"
  gossip_line="$(rg "cluster_info_stats5" "${logfile}" | tail -n 1 || true)"
  append_result "${profile}" "${ingest_line}" "${gossip_line}" "${status}" "${logfile}"
}

# Baseline + candidate defaults.
run_profile "baseline-current" \
  SOF_UDP_RELAY_REFRESH_MS=1500 \
  SOF_UDP_RELAY_PEER_CANDIDATES=96 \
  SOF_UDP_RELAY_FANOUT=16 \
  SOF_UDP_RELAY_MAX_SENDS_PER_SEC=2000 \
  SOF_UDP_RELAY_REQUIRE_TURBINE_SOURCE_PORTS=true

run_profile "candidate-balanced" \
  SOF_UDP_RELAY_REFRESH_MS=2000 \
  SOF_UDP_RELAY_PEER_CANDIDATES=64 \
  SOF_UDP_RELAY_FANOUT=8 \
  SOF_UDP_RELAY_MAX_SENDS_PER_SEC=1200 \
  SOF_UDP_RELAY_REQUIRE_TURBINE_SOURCE_PORTS=true

run_profile "candidate-lean" \
  SOF_UDP_RELAY_REFRESH_MS=2500 \
  SOF_UDP_RELAY_PEER_CANDIDATES=48 \
  SOF_UDP_RELAY_FANOUT=6 \
  SOF_UDP_RELAY_MAX_SENDS_PER_SEC=900 \
  SOF_UDP_RELAY_REQUIRE_TURBINE_SOURCE_PORTS=true

run_profile "candidate-forward-heavy" \
  SOF_UDP_RELAY_REFRESH_MS=2000 \
  SOF_UDP_RELAY_PEER_CANDIDATES=64 \
  SOF_UDP_RELAY_FANOUT=8 \
  SOF_UDP_RELAY_MAX_SENDS_PER_SEC=1200 \
  SOF_UDP_RELAY_REQUIRE_TURBINE_SOURCE_PORTS=false

echo
echo "== ranked results =="
(
  echo "profile score data code forwarded send_attempts rate_limited repair latest_shred_age_ms gossip_push gossip_req status log"
  tail -n +2 "${RESULTS_CSV}" | sort -t, -k2,2gr | awk -F',' '{printf "%-24s %-8s %-8s %-8s %-9s %-13s %-12s %-7s %-20s %-11s %-10s %-14s %s\n", $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13}'
) | tee "${OUT_DIR}/summary.txt"

echo
echo "results_csv=${RESULTS_CSV}"
echo "summary=${OUT_DIR}/summary.txt"
