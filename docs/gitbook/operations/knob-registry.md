# Knob Registry

Use this page as the reference registry for SOF runtime knobs.

Scope:

- all `SOF_*` environment knobs implemented in `crates/sof-observer`
- the bundled gossip backend knobs exposed through `crates/sof-solana-gossip`
- feature-gated knobs are marked where relevant

This registry is intentionally different from the tuning guide:

- use this page when you need to look up one knob quickly
- use [Tuning and Environment Controls](tuning-and-env.md) when you need advice on what to change
  and in what order

Snapshot date:

- `2026-03-14`

## How To Read The Registry

- `Default` is the runtime default when the knob is unset
- `Scope` tells you whether the knob is always available, feature-gated, or implementation-specific
- `Meaning` is intentionally short; deeper tradeoffs still live in the tuning guide

## Jump To

- [Mode Selectors](#mode-selectors)
- [Ingest And UDP Path](#ingest-and-udp-path)
- [Runtime Pipeline And Local State](#runtime-pipeline-and-local-state)
- [Logging And Diagnostics](#logging-and-diagnostics)
- [Verification And Dedupe](#verification-and-dedupe)
- [Relay And Traffic Shaping](#relay-and-traffic-shaping)
- [Gossip Bootstrap Core](#gossip-bootstrap-core)
- [Gossip Runtime Switching](#gossip-runtime-switching)
- [Repair](#repair)
- [Derived State](#derived-state)
- [Typed API Mirrors](#typed-api-mirrors)

## Mode Selectors

| Knob | Default | Scope | Meaning |
| --- | --- | --- | --- |
| `SOF_BIND` | `0.0.0.0:8001` | all builds | Direct UDP bind address for the packaged runtime. |
| `SOF_GOSSIP_ENTRYPOINT` | mainnet bootstrap set with `gossip-bootstrap`, otherwise unset | `gossip-bootstrap` affects behavior | Comma-separated gossip entrypoints used for bootstrap discovery. |
| `SOF_GOSSIP_BOOTSTRAP_ONLY` | `false` | `gossip-bootstrap` | Bootstrap gossip control-plane state, then detach it and keep direct receivers active. |
| `SOF_LIVE_SHREDS_ENABLED` | `true` | all builds | Enables live shred processing paths required by parts of the control plane. |
| `SOF_INGEST_QUEUE_MODE` | `bounded` | all builds | Selects ingest queue implementation: `bounded`, `unbounded`, or `lockfree`. |
| `SOF_INGEST_QUEUE_CAPACITY` | `16384` | all builds | Capacity for bounded or lock-free ingest queue modes. |

## Ingest And UDP Path

| Knob | Default | Scope | Meaning |
| --- | --- | --- | --- |
| `SOF_UDP_RCVBUF` | `67108864` | all builds | UDP socket receive buffer size in bytes. |
| `SOF_UDP_BATCH_SIZE` | `64` | all builds | Maximum packet batch size per UDP receive pass. |
| `SOF_UDP_BATCH_MAX_WAIT_MS` | `1` | all builds | Coalesce wait before flushing a partially filled UDP batch. |
| `SOF_UDP_IDLE_WAIT_MS` | `100` | all builds | Idle wait interval when traffic is sparse. |
| `SOF_UDP_RECEIVER_CORE` | unset | all builds | Fixed CPU core index for UDP receiver pinning. |
| `SOF_UDP_RECEIVER_PIN_BY_PORT` | `false` | all builds | Derive receiver pinning from local port number instead of a fixed core. |
| `SOF_UDP_DROP_ON_CHANNEL_FULL` | `true` | built-in UDP path only | Drop new UDP batches instead of blocking when the ingest queue is full. |
| `SOF_UDP_TRACK_RXQ_OVFL` | `false` | Linux-only | Enables `SO_RXQ_OVFL` kernel-drop telemetry when supported. |

## Runtime Pipeline And Local State

| Knob | Default | Scope | Meaning |
| --- | --- | --- | --- |
| `SOF_WORKER_THREADS` | host parallelism | all builds | Base worker parallelism used by the runtime. |
| `SOF_DATASET_WORKERS` | `SOF_WORKER_THREADS` | all builds | Number of dataset reconstruction workers. |
| `SOF_PACKET_WORKERS` | `SOF_WORKER_THREADS` | all builds | Number of packet verify/FEC/reassembly workers. |
| `SOF_PACKET_WORKER_QUEUE_CAPACITY` | `256` | all builds | Queue depth per packet worker. |
| `SOF_DATASET_MAX_TRACKED_SLOTS` | `2048` | all builds | Maximum tracked dataset slots kept in memory. |
| `SOF_DATASET_RETAINED_SLOT_LAG` | `256` | all builds | Retention lag for dataset tracking state after slots advance. |
| `SOF_FEC_MAX_TRACKED_SETS` | `8192` | all builds | Maximum tracked FEC sets. |
| `SOF_FEC_RETAINED_SLOT_LAG` | `128` | all builds | Retention lag for FEC state after slots advance. |
| `SOF_DATASET_QUEUE_CAPACITY` | `8192` | all builds | Queue depth for reconstructed dataset work. |
| `SOF_DATASET_ATTEMPT_CACHE_CAPACITY` | `8192` | all builds | Cache size for dataset reconstruction attempts. |
| `SOF_DATASET_ATTEMPT_SUCCESS_TTL_MS` | `30000` | all builds | TTL for successful dataset attempts. |
| `SOF_DATASET_ATTEMPT_FAILURE_TTL_MS` | `3000` | all builds | TTL for failed dataset attempts. |
| `SOF_DATASET_TAIL_MIN_SHREDS` | `2` | all builds | Minimum tail shreds heuristic used during reconstruction. |
| `SOF_COVERAGE_WINDOW_SLOTS` | `256` | all builds | Slot window for coverage metrics and reporting. |
| `SOF_FORK_WINDOW_SLOTS` | `2048` | all builds | In-memory fork graph window for canonical tracking and reorgs. |
| `SOF_TX_CONFIRMED_DEPTH_SLOTS` | `32` | all builds | Local slot-depth threshold for `confirmed` tx tagging. |
| `SOF_TX_FINALIZED_DEPTH_SLOTS` | `150` | all builds | Local slot-depth threshold for `finalized` tx tagging. |
| `SOF_RUNTIME_EXTENSION_QUEUE_DEPTH_WARN` | `4096` | all builds | Warning threshold for aggregate runtime-extension queue depth. |
| `SOF_RUNTIME_EXTENSION_DISPATCH_LAG_WARN_US` | `50000` | all builds | Warning threshold for runtime-extension dispatch lag. |
| `SOF_RUNTIME_EXTENSION_DROP_WARN_DELTA` | `100` | all builds | Warning threshold for dropped runtime-extension events per telemetry interval. |

## Logging And Diagnostics

| Knob | Default | Scope | Meaning |
| --- | --- | --- | --- |
| `SOF_LOG_STARTUP_STEPS` | `true` | all builds | Emit startup-step logs during runtime bootstrap. |
| `SOF_LOG_ALL_TXS` | `false` | all builds | Log every observed transaction. |
| `SOF_LOG_NON_VOTE_TXS` | `false` | all builds | Log observed non-vote transactions. |
| `SOF_SKIP_VOTE_ONLY_TX_DETAIL_PATH` | `false` | all builds | Skip detailed processing path for vote-only tx detail emission. |
| `SOF_LOG_DATASET_RECONSTRUCTION` | `false` | all builds | Emit dataset reconstruction logs. |
| `SOF_LOG_REPAIR_PEER_TRAFFIC` | `false` | `gossip-bootstrap` | Emit repair peer traffic logs. |
| `SOF_LOG_REPAIR_PEER_TRAFFIC_EVERY` | `256` | `gossip-bootstrap` | Sampling interval for repair peer traffic logs. |

## Verification And Dedupe

| Knob | Default | Scope | Meaning |
| --- | --- | --- | --- |
| `SOF_VERIFY_SHREDS` | `false` | all builds | Enable shred verification. |
| `SOF_VERIFY_STRICT` | `false` | all builds | Reject unknown leaders more aggressively during verification. |
| `SOF_VERIFY_RECOVERED_SHREDS` | `false` | all builds | Verify recovered shreds after FEC recovery. |
| `SOF_VERIFY_SIGNATURE_CACHE` | `65536` | all builds | Signature verification cache entries. |
| `SOF_VERIFY_SLOT_WINDOW` | `4096` | all builds | Slot-leader verification window. |
| `SOF_VERIFY_UNKNOWN_RETRY_MS` | `2000` | all builds | Retry interval for unknown-leader verification state. |
| `SOF_SHRED_DEDUP_CAPACITY` | `262144` | all builds | Capacity of the semantic shred dedupe cache. |
| `SOF_SHRED_DEDUP_TTL_MS` | `250` | all builds | Base eviction TTL for dedupe entries. |

## Relay And Traffic Shaping

| Knob | Default | Scope | Meaning |
| --- | --- | --- | --- |
| `SOF_RELAY_CACHE_WINDOW_MS` | `10000` | `gossip-bootstrap` | Time window for retained relay-cache shreds. |
| `SOF_RELAY_CACHE_MAX_SHREDS` | `16384` | `gossip-bootstrap` | Maximum retained relay-cache shreds. |
| `SOF_UDP_RELAY_ENABLED` | `true` | `gossip-bootstrap` | Enable bounded UDP relay forwarding. |
| `SOF_UDP_RELAY_REFRESH_MS` | `2000` | `gossip-bootstrap` | Relay peer refresh cadence. |
| `SOF_UDP_RELAY_PEER_CANDIDATES` | `64` | `gossip-bootstrap` | Number of peer candidates scanned for relay selection. |
| `SOF_UDP_RELAY_FANOUT` | `8` | `gossip-bootstrap` | Maximum relay fanout per packet. |
| `SOF_UDP_RELAY_MAX_SENDS_PER_SEC` | `1200` | `gossip-bootstrap` | Global relay send budget per second. |
| `SOF_UDP_RELAY_MAX_PEERS_PER_IP` | `2` | `gossip-bootstrap` | Cap selected relay peers per IP address. |
| `SOF_UDP_RELAY_REQUIRE_TURBINE_SOURCE_PORTS` | `true` | `gossip-bootstrap` | Forward only turbine-source-port traffic by default. |
| `SOF_UDP_RELAY_SEND_ERROR_BACKOFF_MS` | `1000` | `gossip-bootstrap` | Backoff duration after repeated relay send errors. |
| `SOF_UDP_RELAY_SEND_ERROR_BACKOFF_THRESHOLD` | `64` | `gossip-bootstrap` | Consecutive send-error threshold before backoff. |

## Gossip Bootstrap Core

| Knob | Default | Scope | Meaning |
| --- | --- | --- | --- |
| `SOF_PORT_RANGE` | `12000-12100` | `gossip-bootstrap` | Bind range for gossip and TVU sockets. |
| `SOF_GOSSIP_PORT` | unset | `gossip-bootstrap` | Fixed gossip port override. |
| `SOF_TVU_SOCKETS` | available parallelism, clamped `1..=16` | `gossip-bootstrap` | TVU socket fanout count. |
| `SOF_SHRED_VERSION` | unset | `gossip-bootstrap` | Explicit shred-version override. |
| `SOF_GOSSIP_ENTRYPOINT_PROBE` | `false` | `gossip-bootstrap` | Probe entrypoints during startup. |
| `SOF_GOSSIP_VALIDATORS` | unset | `gossip-bootstrap` | Comma-separated validator identity set used to constrain gossip. |
| `SOF_GOSSIP_BOOTSTRAP_STABILIZE_MAX_WAIT_MS` | `20000` | `gossip-bootstrap` | Max wait for bootstrap stabilization. |
| `SOF_GOSSIP_BOOTSTRAP_STABILIZE_MIN_PEERS` | `128` | `gossip-bootstrap` | Minimum peer count target for bootstrap stabilization. |
| `SOF_GOSSIP_RECEIVER_CHANNEL_CAPACITY` | `8192` | bundled gossip backend | Receiver to gossip-request queue capacity. |
| `SOF_GOSSIP_SOCKET_CONSUME_CHANNEL_CAPACITY` | `8192` | bundled gossip backend | Receiver to listen-worker queue capacity. |
| `SOF_GOSSIP_RESPONSE_CHANNEL_CAPACITY` | `8192` | bundled gossip backend | Outbound gossip response queue capacity. |
| `SOF_GOSSIP_CHANNEL_CONSUME_CAPACITY` | `1024` | bundled gossip backend | Max drained packet batches per gossip worker pass. |
| `SOF_GOSSIP_CONSUME_THREADS` | host parallelism, clamped to `8` | bundled gossip backend | Gossip socket-consume worker count. |
| `SOF_GOSSIP_LISTEN_THREADS` | host parallelism, clamped to `8` | bundled gossip backend | Gossip listen worker count. |
| `SOF_GOSSIP_RUN_THREADS` | host parallelism, clamped to `8` | bundled gossip backend | Gossip run/push-pull Rayon worker count. |
| `SOF_GOSSIP_SOCKET_CONSUME_PARALLEL_PACKET_THRESHOLD` | `1024` | bundled gossip backend | Serial vs parallel threshold for socket-consume verification. |
| `SOF_GOSSIP_LISTEN_PARALLEL_BATCH_THRESHOLD` | `32` | bundled gossip backend | Serial vs parallel threshold for listen-loop batch filtering. |
| `SOF_GOSSIP_LISTEN_PARALLEL_MESSAGE_THRESHOLD` | `256` | bundled gossip backend | Serial vs parallel threshold for listen-loop total message filtering. |
| `SOF_GOSSIP_STATS_INTERVAL_SECS` | `10` | bundled gossip backend | Gossip metrics reporting interval. |
| `SOF_GOSSIP_SAMPLE_LOGS_ENABLED` | `true` | bundled gossip backend | Enable sampled gossip and ping datapoint logging. |

## Gossip Runtime Switching

| Knob | Default | Scope | Meaning |
| --- | --- | --- | --- |
| `SOF_GOSSIP_RUNTIME_SWITCH_ENABLED` | `false` | `gossip-bootstrap` | Enable runtime switching between gossip receiver candidates. |
| `SOF_GOSSIP_RUNTIME_SWITCH_STALL_MS` | `2000` | `gossip-bootstrap` | Traffic stall threshold that can trigger switching. |
| `SOF_GOSSIP_RUNTIME_SWITCH_DATASET_STALL_MS` | `12000` | `gossip-bootstrap` | Dataset-output stall threshold for switching. |
| `SOF_GOSSIP_RUNTIME_SWITCH_COOLDOWN_MS` | `15000` | `gossip-bootstrap` | Cooldown after one switch before another is allowed. |
| `SOF_GOSSIP_RUNTIME_SWITCH_WARMUP_MS` | `10000` | `gossip-bootstrap` | Warmup period for a candidate runtime before evaluation. |
| `SOF_GOSSIP_RUNTIME_SWITCH_OVERLAP_MS` | `1250` | `gossip-bootstrap` | Overlap window when swapping runtimes. |
| `SOF_GOSSIP_RUNTIME_SWITCH_PORT_RANGE` | unset | `gossip-bootstrap` | Separate port range for overlap/runtime-switch bring-up. |
| `SOF_GOSSIP_RUNTIME_SWITCH_SUSTAIN_MS` | `1500` | `gossip-bootstrap` | Sustained healthy-traffic threshold before accepting a candidate. |
| `SOF_GOSSIP_RUNTIME_SWITCH_NO_TRAFFIC_GRACE_MS` | `120000` | `gossip-bootstrap` | Grace period before no-traffic conditions are treated as stalled. |
| `SOF_GOSSIP_RUNTIME_SWITCH_STABILIZE_MS` | `1000` | `gossip-bootstrap` | Stabilization window before a runtime is considered ready. |
| `SOF_GOSSIP_RUNTIME_SWITCH_STABILIZE_MIN_PACKETS` | `8` | `gossip-bootstrap` | Minimum packet count during stabilization. |
| `SOF_GOSSIP_RUNTIME_SWITCH_STABILIZE_MAX_WAIT_MS` | `8000` | `gossip-bootstrap` | Maximum time to wait for stabilization. |
| `SOF_GOSSIP_RUNTIME_SWITCH_PEER_CANDIDATES` | `64` | `gossip-bootstrap` | Candidate peer set size used during switch evaluation. |

## Repair

| Knob | Default | Scope | Meaning |
| --- | --- | --- | --- |
| `SOF_REPAIR_ENABLED` | `true` | `gossip-bootstrap` | Enable bounded repair requests and serving. |
| `SOF_REPAIR_TICK_MS` | `200` | `gossip-bootstrap` | Repair scheduler tick cadence. |
| `SOF_REPAIR_SLOT_WINDOW` | `512` | `gossip-bootstrap` | Slot window considered by repair. |
| `SOF_REPAIR_SETTLE_MS` | `250` | `gossip-bootstrap` | Settle delay before repair escalates. |
| `SOF_REPAIR_COOLDOWN_MS` | `150` | `gossip-bootstrap` | Cooldown before retrying repeated repair requests. |
| `SOF_REPAIR_MIN_SLOT_LAG` | `4` | `gossip-bootstrap` | Minimum slot lag before normal repair requests are allowed. |
| `SOF_REPAIR_MIN_SLOT_LAG_STALLED` | `2` | `gossip-bootstrap` | Minimum slot lag before stalled-mode repair requests are allowed. |
| `SOF_REPAIR_TIP_STALL_MS` | `1500` | `gossip-bootstrap` | Tip-stall threshold used to escalate repair. |
| `SOF_REPAIR_STALL_SUSTAIN_MS` | `1500` | `gossip-bootstrap` | Time a stall must persist before stalled-mode budgets apply. |
| `SOF_REPAIR_TIP_PROBE_AHEAD_SLOTS` | `16` | `gossip-bootstrap` | Forward probe distance ahead of the current tip. |
| `SOF_REPAIR_MAX_REQUESTS_PER_TICK` | `4` | `gossip-bootstrap` | Normal-mode repair request budget per tick. |
| `SOF_REPAIR_MAX_REQUESTS_PER_TICK_STALLED` | derived | `gossip-bootstrap` | Stalled-mode repair request budget per tick. |
| `SOF_REPAIR_MAX_HIGHEST_PER_TICK` | `4` | `gossip-bootstrap` | Highest-window request budget per tick. |
| `SOF_REPAIR_MAX_HIGHEST_PER_TICK_STALLED` | derived | `gossip-bootstrap` | Stalled-mode highest-window request budget per tick. |
| `SOF_REPAIR_MAX_FORWARD_PROBE_PER_TICK` | `2` | `gossip-bootstrap` | Normal-mode forward probe budget per tick. |
| `SOF_REPAIR_MAX_FORWARD_PROBE_PER_TICK_STALLED` | derived | `gossip-bootstrap` | Stalled-mode forward probe budget per tick. |
| `SOF_REPAIR_PER_SLOT_CAP` | `16` | `gossip-bootstrap` | Per-slot cap for outstanding repair requests. |
| `SOF_REPAIR_PER_SLOT_CAP_STALLED` | derived | `gossip-bootstrap` | Stalled-mode per-slot repair cap. |
| `SOF_REPAIR_BACKFILL_SETS` | `8` | `gossip-bootstrap` | Number of backfill sets considered per pass. |
| `SOF_REPAIR_DATASET_STALL_MS` | `2000` | `gossip-bootstrap` | Dataset-stall threshold used to escalate repair. |
| `SOF_REPAIR_OUTSTANDING_TIMEOUT_MS` | `150` | `gossip-bootstrap` | Timeout for deduping outstanding repair requests. |
| `SOF_REPAIR_SOURCE_HINT_FLUSH_MS` | `200` | `gossip-bootstrap` | Flush interval for repair source hints. |
| `SOF_REPAIR_SOURCE_HINT_BATCH_SIZE` | `512` | `gossip-bootstrap` | Source-hint batch size. |
| `SOF_REPAIR_SOURCE_HINT_CAPACITY` | `16384` | `gossip-bootstrap` | Source-hint buffer capacity. |
| `SOF_REPAIR_PEER_CACHE_TTL_MS` | `10000` | `gossip-bootstrap` | Repair peer cache TTL. |
| `SOF_REPAIR_PEER_CACHE_CAPACITY` | `128` | `gossip-bootstrap` | Repair peer cache capacity. |
| `SOF_REPAIR_ACTIVE_PEERS` | `256` | `gossip-bootstrap` | Active repair peer working set size. |
| `SOF_REPAIR_PEER_SAMPLE_SIZE` | `10` | `gossip-bootstrap` | Stake-weighted repair peer sample size. |
| `SOF_REPAIR_SERVE_MAX_BYTES_PER_SEC` | `4000000` | `gossip-bootstrap` | Max repair response bytes per second. |
| `SOF_REPAIR_SERVE_UNSTAKED_MAX_BYTES_PER_SEC` | `1000000` | `gossip-bootstrap` | Max unstaked repair response bytes per second. |
| `SOF_REPAIR_SERVE_MAX_REQUESTS_PER_PEER_PER_SEC` | `256` | `gossip-bootstrap` | Max served repair requests per peer per second. |
| `SOF_REPAIR_COMMAND_QUEUE` | `16384` | `gossip-bootstrap` | Repair command queue depth. |
| `SOF_REPAIR_RESULT_QUEUE` | `16384` | `gossip-bootstrap` | Repair result queue depth. |
| `SOF_REPAIR_PEER_REFRESH_MS` | `1000` | `gossip-bootstrap` | Repair peer refresh cadence. |

## Derived State

| Knob | Default | Scope | Meaning |
| --- | --- | --- | --- |
| `SOF_DERIVED_STATE_CHECKPOINT_INTERVAL_MS` | `30000` | all builds using derived-state host | Checkpoint-barrier cadence for derived-state consumers. |
| `SOF_DERIVED_STATE_RECOVERY_INTERVAL_MS` | `5000` | all builds using derived-state host | Recovery-attempt cadence for derived-state consumers. |
| `SOF_DERIVED_STATE_REPLAY_BACKEND` | `memory` | all builds using derived-state host | Replay retention backend: memory or disk-backed mode. |
| `SOF_DERIVED_STATE_REPLAY_DIR` | `.sof-derived-state-replay` | all builds using derived-state host | Directory used by disk-backed derived-state replay. |
| `SOF_DERIVED_STATE_REPLAY_DURABILITY` | `flush` | all builds using derived-state host | Disk replay durability policy. |
| `SOF_DERIVED_STATE_REPLAY_MAX_ENVELOPES` | `8192` | all builds using derived-state host | Max retained envelopes per session. |
| `SOF_DERIVED_STATE_REPLAY_MAX_SESSIONS` | `4` | all builds using derived-state host | Max retained sessions for the replay backend. |

## Typed API Mirrors

The registry above covers environment knobs, but several common controls also have typed API entry
points:

- `sof::runtime::RuntimeSetup`
- `sof_gossip_tuning::GossipTuningProfile`
- `sof::runtime::DerivedStateRuntimeConfig`

Prefer the typed API when:

- you embed SOF in a long-lived service
- you want reviewable, versioned configuration
- you do not want environment strings scattered across your application
