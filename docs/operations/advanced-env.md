# Advanced Environment Controls (Expert Only)

These variables are intentionally undocumented in the quick-start path because they can easily reduce ingest quality, increase packet loss, or create unstable behavior.

Use these only when you are measuring changes and can roll back quickly.

- Source of truth: `crates/sof-observer/src/app/config/*`, `crates/sof-observer/src/app/runtime/*`, `crates/sof-observer/src/ingest/core.rs`
- Snapshot date: 2026-03-02

## Safe baseline

For most deployments, keep defaults and set only:

- `RUST_LOG`
- `SOF_GOSSIP_ENTRYPOINT` (gossip mode)
- `SOF_BIND` (plain UDP listener mode)

If outbound control-plane traffic is high without measurable shred ingest benefit,
prefer these Agave-aligned reductions before disabling relay/repair:

- set `SOF_GOSSIP_VALIDATORS` to a small, stable validator set
- keep `SOF_UDP_RELAY_REQUIRE_TURBINE_SOURCE_PORTS=true`
- lower `SOF_UDP_RELAY_MAX_SENDS_PER_SEC` incrementally while monitoring ingest coverage

To run a bounded live profile sweep and rank candidate settings on your host:

- `cargo make tune-relay-defaults`

## Runtime and dataset tuning

| Variable | Default | Why this is advanced |
|---|---:|---|
| `SOF_WORKER_THREADS` | host parallelism | Oversizing can add contention and context-switch overhead. |
| `SOF_DATASET_WORKERS` | `SOF_WORKER_THREADS` | Too high can cause queue churn without higher throughput. |
| `SOF_DATASET_MAX_TRACKED_SLOTS` | `2048` | Higher values increase memory and stale-state retention. |
| `SOF_FEC_MAX_TRACKED_SETS` | `8192` | Direct memory/CPU pressure control in recovery paths. |
| `SOF_DATASET_QUEUE_CAPACITY` | `8192` | Larger queue can hide backpressure and increase latency. |
| `SOF_DATASET_ATTEMPT_CACHE_CAPACITY` | `8192` | Bigger cache means more memory and longer stale entries. |
| `SOF_DATASET_ATTEMPT_SUCCESS_TTL_MS` | `30000` | Too high can suppress legitimate retries. |
| `SOF_DATASET_ATTEMPT_FAILURE_TTL_MS` | `3000` | Too low can cause retry storms. |
| `SOF_DATASET_TAIL_MIN_SHREDS` | `2` | Affects reconstruction completeness heuristics. |
| `SOF_COVERAGE_WINDOW_SLOTS` | `256` | Impacts coverage stats memory/cadence. |
| `SOF_FORK_WINDOW_SLOTS` | `2048` | Bounded in-memory slot graph used for local canonical/reorg tracking; lower values reduce memory but can shorten reorg context. |
| `SOF_TX_CONFIRMED_DEPTH_SLOTS` | `32` | Local slot-depth threshold used to tag tx events as `confirmed`; lower values are faster but less conservative. |
| `SOF_TX_FINALIZED_DEPTH_SLOTS` | `150` | Local slot-depth threshold used to tag tx events as `finalized`; must be >= confirmed depth for consistent semantics. |

## Verification and dedupe tuning

| Variable | Default | Why this is advanced |
|---|---:|---|
| `SOF_VERIFY_SHREDS` | `false` | Enabling adds cryptographic verification cost. |
| `SOF_VERIFY_STRICT` | `false` | Strict unknown-leader rejection can drop useful traffic. |
| `SOF_VERIFY_RECOVERED_SHREDS` | `false` | Extra checks on recovered data can increase CPU. |
| `SOF_VERIFY_SIGNATURE_CACHE` | `65536` | Cache size directly impacts memory and miss behavior. |
| `SOF_VERIFY_SLOT_WINDOW` | `4096` | Leader-window sizing trades memory for hit rate. |
| `SOF_VERIFY_UNKNOWN_RETRY_MS` | `2000` | Retry cadence affects false unknowns vs CPU/network cost. |
| `SOF_SHRED_DEDUP_CAPACITY` | `262144` | Too low increases duplicate processing; too high burns memory. |
| `SOF_SHRED_DEDUP_TTL_MS` | `250` | TTL controls duplicate acceptance during jitter bursts. |

## Relay cache tuning

| Variable | Default | Why this is advanced |
|---|---:|---|
| `SOF_RELAY_CACHE_WINDOW_MS` | `10000` | Larger windows increase memory and stale replay surface; smaller windows reduce peer cache hit rate. |
| `SOF_RELAY_CACHE_MAX_SHREDS` | `16384` | Hard memory/serve bound; too low hurts cache utility, too high risks memory pressure under burst traffic. |
| `SOF_UDP_RELAY_ENABLED` | `true` | Disabling turns off gossip-discovered TVU UDP forwarding and reduces SOF's relay contribution to the wider network. |
| `SOF_UDP_RELAY_REFRESH_MS` | `2000` | Faster refresh reacts quickly but increases control-plane churn; slower refresh can hold stale peers longer. |
| `SOF_UDP_RELAY_PEER_CANDIDATES` | `64` | Larger scans add gossip iteration cost; too small can miss healthy relay targets in large clusters. |
| `SOF_UDP_RELAY_FANOUT` | `8` (hard cap: `200`) | Per-packet relay target cap; values are hard-capped at `200` to avoid retransmit fanout inflation. |
| `SOF_UDP_RELAY_MAX_SENDS_PER_SEC` | `1200` | Global UDP relay send budget; too low under-relays, too high can overload local NIC/kernel buffers. |
| `SOF_UDP_RELAY_MAX_PEERS_PER_IP` | `2` | Caps selected relay destinations per IP to reduce Sybil amplification risk from many identities behind one host. |
| `SOF_UDP_RELAY_REQUIRE_TURBINE_SOURCE_PORTS` | `true` | Forward only turbine-port sourced shreds (`8899`/`8900`) by default; helps prevent repair/relay re-flood amplification. |
| `SOF_UDP_RELAY_SEND_ERROR_BACKOFF_MS` | `1000` | Backoff duration after repeated relay send failures to avoid outbound retry storms under network pressure. |
| `SOF_UDP_RELAY_SEND_ERROR_BACKOFF_THRESHOLD` | `64` | Number of consecutive relay send errors before backoff engages. |

## Gossip bootstrap and network controls

(Feature-gated paths may require `--features gossip-bootstrap`.)

| Variable | Default | Why this is advanced |
|---|---:|---|
| `SOF_PORT_RANGE` | `12000-12100` | Wrong range breaks inbound TVU/gossip reachability. |
| `SOF_GOSSIP_PORT` | unset | Can collide with existing bindings or NAT mapping. |
| `SOF_TVU_SOCKETS` | available parallelism, clamped `1..=16` (fallback `1`) | Improper socket fanout can hurt packet locality. |
| `SOF_SHRED_VERSION` | unset | Bad override can reject valid cluster traffic. |
| `SOF_GOSSIP_VALIDATORS` | unset | Optional comma-separated validator identity pubkeys passed to Agave gossip `gossip_validators`; constrains push/pull targets to this set and can materially lower control-plane traffic/CPU at the cost of narrower gossip mesh. |
| `SOF_GOSSIP_ENTRYPOINT_PROBE` | `false` | Adds startup probing behavior that can delay launch. |
| `SOF_GOSSIP_BOOTSTRAP_STABILIZE_MAX_WAIT_MS` | `20000` | Startup stabilization timing affects readiness behavior. |

## Gossip runtime switch controls

| Variable | Default | Why this is advanced |
|---|---:|---|
| `SOF_GOSSIP_RUNTIME_SWITCH_ENABLED` | `false` | Defaults off to avoid runtime churn; enable only when actively tuning failover behavior. |
| `SOF_GOSSIP_RUNTIME_SWITCH_STALL_MS` | `2000` | Too low causes switch flapping; too high delays recovery. |
| `SOF_GOSSIP_RUNTIME_SWITCH_DATASET_STALL_MS` | `12000` | Affects stalled detection for dataset output. |
| `SOF_GOSSIP_RUNTIME_SWITCH_COOLDOWN_MS` | `15000` | Cooldown tuning directly impacts churn risk. |
| `SOF_GOSSIP_RUNTIME_SWITCH_WARMUP_MS` | `10000` | Too short can pick unstable candidate runtimes. |
| `SOF_GOSSIP_RUNTIME_SWITCH_OVERLAP_MS` | `1250` | Overlap impacts double-ingest window and contention. |
| `SOF_GOSSIP_RUNTIME_SWITCH_PORT_RANGE` | unset | Incorrect overlap range can fail bind during switch. |
| `SOF_GOSSIP_RUNTIME_SWITCH_SUSTAIN_MS` | `1500` | Sustained-traffic threshold controls switch confidence. |
| `SOF_GOSSIP_RUNTIME_SWITCH_NO_TRAFFIC_GRACE_MS` | `120000` | Grace period influences sensitivity to transient outages. |
| `SOF_GOSSIP_RUNTIME_SWITCH_STABILIZE_MS` | `1000` | Low values increase false positive readiness. |
| `SOF_GOSSIP_RUNTIME_SWITCH_STABILIZE_MIN_PACKETS` | `8` | Packet threshold influences switch acceptance quality. |
| `SOF_GOSSIP_RUNTIME_SWITCH_STABILIZE_MAX_WAIT_MS` | `8000` | Max wait impacts startup/switch latency. |
| `SOF_GOSSIP_RUNTIME_SWITCH_PEER_CANDIDATES` | `64` | Too low misses good peers; too high adds overhead. |

## Repair pipeline controls

Repairs are stream-first by default: SOF keeps repair request budgets at `0` while fresh shred flow is healthy, and only opens repair request budgets once tip/dataset stall thresholds are observed.

| Variable | Default | Why this is advanced |
|---|---:|---|
| `SOF_REPAIR_ENABLED` | `true` | Disabling can stall recovery under packet loss. |
| `SOF_REPAIR_TICK_MS` | `200` | Tick cadence controls repair burst/latency tradeoff. |
| `SOF_REPAIR_SLOT_WINDOW` | `512` | Window size alters memory and backfill scope. |
| `SOF_REPAIR_SETTLE_MS` | `250` | Too aggressive requests can duplicate in-flight turbine traffic. |
| `SOF_REPAIR_COOLDOWN_MS` | `150` | Low cooldown can flood peers with repeats. |
| `SOF_REPAIR_MIN_SLOT_LAG` | `4` | Too low can over-repair at slot tip. |
| `SOF_REPAIR_MIN_SLOT_LAG_STALLED` | `2` | Keeps stalled-mode repairs off the very tip to preserve stream-first behavior. |
| `SOF_REPAIR_TIP_STALL_MS` | `1500` | Changes tip-stall sensitivity; higher values reduce false stall escalation. |
| `SOF_REPAIR_STALL_SUSTAIN_MS` | `1500` | Stall must persist this long before switching to elevated stalled-mode budgets. |
| `SOF_REPAIR_TIP_PROBE_AHEAD_SLOTS` | `16` | Larger values can spend budget probing too far ahead. |
| `SOF_REPAIR_MAX_REQUESTS_PER_TICK` | `4` (hard cap: `24`) | Default-low repair pacing so stream ingestion stays primary. |
| `SOF_REPAIR_MAX_REQUESTS_PER_TICK_STALLED` | derived (hard cap: `24`) | Stalled-mode request budget stays capped to prevent repair amplification bursts. |
| `SOF_REPAIR_MAX_HIGHEST_PER_TICK` | `4` | Affects highest-window probing pressure. |
| `SOF_REPAIR_MAX_HIGHEST_PER_TICK_STALLED` | derived | Aggressive stalled probing can thrash peers. |
| `SOF_REPAIR_MAX_FORWARD_PROBE_PER_TICK` | `2` | Forward probes can steal budget from direct missing requests. |
| `SOF_REPAIR_MAX_FORWARD_PROBE_PER_TICK_STALLED` | derived | Stalled-mode burst risk. |
| `SOF_REPAIR_PER_SLOT_CAP` | `16` | Prevents per-slot repair concentration; bad values skew fairness. |
| `SOF_REPAIR_PER_SLOT_CAP_STALLED` | derived | Stalled per-slot cap can produce hot-slot overload. |
| `SOF_REPAIR_BACKFILL_SETS` | `8` | Backfill aggressiveness affects catch-up cost. |
| `SOF_REPAIR_DATASET_STALL_MS` | `2000` | Dataset-stall threshold drives repair escalation timing. |
| `SOF_REPAIR_OUTSTANDING_TIMEOUT_MS` | `150` | In-flight dedupe timeout affects duplicate resend behavior. |
| `SOF_REPAIR_SOURCE_HINT_FLUSH_MS` | `200` | Hint flush cadence impacts request locality quality. |
| `SOF_REPAIR_SOURCE_HINT_BATCH_SIZE` | `512` | Batch sizing impacts CPU/cache behavior under load. |
| `SOF_REPAIR_SOURCE_HINT_CAPACITY` | `16384` | Larger buffers increase memory and stale hints. |
| `SOF_REPAIR_PEER_CACHE_TTL_MS` | `10000` | Peer cache freshness affects request quality. |
| `SOF_REPAIR_PEER_CACHE_CAPACITY` | `128` | Too small causes churn, too large grows stale state. |
| `SOF_REPAIR_ACTIVE_PEERS` | `256` | Active peer fanout changes repair network pressure. |
| `SOF_REPAIR_PEER_SAMPLE_SIZE` | `10` (hard cap: `10`) | Stake-weighted sample set stays bounded to the Agave-aligned profile. |
| `SOF_REPAIR_SERVE_MAX_BYTES_PER_SEC` | `4000000` (hard cap: `12000000`) | Conservative default response-byte budget with hard cap preserved for anti-amplification posture. |
| `SOF_REPAIR_SERVE_UNSTAKED_MAX_BYTES_PER_SEC` | `1000000` (hard cap: `2000000`) | Conservative unstaked sub-budget with hard cap preserved. |
| `SOF_REPAIR_SERVE_MAX_REQUESTS_PER_PEER_PER_SEC` | `256` (hard cap: `256`) | Per-source request budget is hard-capped to bound abusive request floods. |
| `SOF_REPAIR_COMMAND_QUEUE` | `16384` | Queue depth can hide overload and inflate latency. |
| `SOF_REPAIR_RESULT_QUEUE` | `16384` | Same risk as command queue for downstream processing. |
| `SOF_REPAIR_PEER_REFRESH_MS` | `1000` | Refresh cadence trades freshness for background overhead. |

## Ingest hot-path controls

| Variable | Default | Why this is advanced |
|---|---:|---|
| `SOF_UDP_RCVBUF` | `67108864` | Kernel buffer sizing can interact badly with host limits. |
| `SOF_UDP_BATCH_SIZE` | `64` | Larger batches reduce syscalls but increase per-batch latency. |
| `SOF_UDP_BATCH_MAX_WAIT_MS` | `1` | Higher values can add avoidable tail latency. |
| `SOF_UDP_IDLE_WAIT_MS` | `100` | Impacts poll cadence when traffic is sparse. |
| `SOF_UDP_RECEIVER_CORE` | unset | Wrong pinning can hurt NUMA/locality and starve other work. |
| `SOF_UDP_RECEIVER_PIN_BY_PORT` | `false` | Deterministic pinning may conflict with scheduler strategy. |
| `SOF_UDP_TRACK_RXQ_OVFL` | `false` | Linux-only telemetry path adds socket option behavior. |

## Non-advanced operational selectors

These are normal mode selectors and are expected to be set intentionally:

- `SOF_BIND` (default `0.0.0.0:8001`)
- `SOF_GOSSIP_ENTRYPOINT` (default mainnet bootstrap set when built with `gossip-bootstrap`; otherwise unset)
