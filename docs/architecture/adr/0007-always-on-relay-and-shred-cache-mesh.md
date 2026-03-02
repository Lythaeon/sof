# ADR-0007: Always-On Relay and Shred Cache Mesh for `sof-observer`

- Status: Implemented
- Date: 2026-03-01
- Decision makers: `sof-observer` maintainers
- Related: `docs/architecture/runtime-bootstrap-modes.md`, `docs/operations/shred-ingestion-home-router-and-proxy.md`

## Context

`sof-observer` adoption increases total shred consumers. If many nodes independently ingest from validators/entrypoints, aggregate demand can become a liability:

1. validator-facing bandwidth and socket pressure increases,
2. rate limiting affects all consumer classes similarly, and
3. observer nodes do not consistently contribute capacity back to the network.

Current relay support is stream fanout oriented and optional. It does not provide a standardized short-lived shred cache with bounded pull/serve behavior between peers. This limits SOF's ability to act as cooperative infrastructure at scale.

We want every SOF node to be a net contributor by default: receive, cache, relay, and serve recent shreds to other nodes with strict bounds.

## Reference Signals from Agave

To avoid designing a mesh that fights the protocol, this ADR aligns with upstream Solana/Agave patterns observed in `anza-xyz/agave`:

1. Dynamic validator networking is port-range based, not fixed-port based:
   - `VALIDATOR_PORT_RANGE = (8000, 10000)`
   - minimum required width: `25`
2. Turbine retransmit is explicitly bounded:
   - `DATA_PLANE_FANOUT = 200`
   - `MAX_NUM_TURBINE_HOPS = 4`
3. Turbine dedups duplicate TVU destinations and limits duplicate destinations per IP in production clusters.
4. Repair protocol is bounded by request type and response budget:
   - `Orphan`, `HighestShred`, `Shred` request families
   - `MAX_ORPHAN_REPAIR_RESPONSES = 11`
   - ancestor response payload bounded (`MAX_ANCESTOR_RESPONSES = 30`)
5. Repair serving includes strict abuse controls:
   - signed request verification with recipient check and timestamp window (`10 min`)
   - ping/pong cache with capacity and rate-limiting
   - load shedding and stake/whitelist prioritization
   - outbound repair bandwidth budget (`12_000_000` bytes/s), with heavier cost while leader
6. Repair requesting is also bounded and stake-aware:
   - defer initial repairs to allow propagation (`250 ms`)
   - per-request timeout (`150 ms`)
   - weighted peer sampling (stake-weighted subset, sample size `10`)
   - outstanding-request nonce tracking with expiry.

These are reference behaviors, not copy-paste constants for SOF. The key point is preserving the same boundedness and anti-amplification posture.

## Decision

Adopt an always-on relay and cache capability in `sof-observer`, with a bounded recent-shred ring buffer and explicit peer request/response primitives.

Core decision points:

1. Relay and recent-shred caching are default runtime behavior for all nodes, not a separate "observer-only" mode.
2. Each node keeps a short bounded ring buffer of recent shreds (target default: 10 seconds).
3. Nodes can serve recent shreds from cache to peers via a bounded protocol.
4. Relay forwarding includes loop prevention and per-peer rate limits.
5. Validator-facing ingress is treated as scarce; peer mesh traffic should absorb most marginal growth.
6. Discovery is public; serving is public-but-bounded. Unbounded serving is explicitly out of scope.
7. Gossip is used to discover SOF peers in the wider cluster graph, not only bootstrap entrypoints.

### Operating Profile (Implemented Default)

1. Default-low repair rate: stream ingestion remains primary; repair is fallback.
2. Strict caps identical to Agave where directly mapped (`fanout=200`, `repair_settle_ms=250`, `repair_timeout_ms=150`, `repair_peer_sample_size=10`, `repair_serve_max_bytes_per_sec=12_000_000`).
3. No identity churn during runtime operation: one gossip identity is kept per process lifetime.
4. No retransmit fanout increase: relay fanout is hard-capped at turbine fanout (`200`).
5. Net contribution only within limits: nodes serve repairs/shreds opportunistically but always behind hard budgets and load-shed paths.

This does not remove existing ingest source choices (`SOF_BIND`, gossip bootstrap, relay upstreams). It removes the architectural assumption that relay/cache participation is optional.

### Relay Tier Definition (SOF Terminology)

The Solana ecosystem does not currently define a formal "relay tier" role. For SOF architecture and operations, we define:

1. Relay tier: the set of SOF nodes that actively receive, cache, and re-serve recent shreds under strict bounded policies.
2. Relay client: a SOF node process participating in that tier by default.

This terminology exists to make the contribution model explicit: SOF is not intended to be a passive shred consumer. It is an active, bounded network participant that strengthens aggregate data availability.

## Goals

1. Make SOF nodes additive to network health under growth.
2. Reduce marginal load on validators as SOF node count increases.
3. Improve shred availability and catch-up latency through local peer cache hits.
4. Preserve bounded memory, bounded queues, and bounded fanout in hot paths.
5. Keep behavior safe by default (loop control, limits, observability).

## Non-Goals (initial)

1. Replace Solana-native repair protocol with a new global protocol.
2. Guarantee full historical shred serving beyond short recent windows.
3. Provide unbounded public relay endpoints.
4. Lock transport to one implementation before operational validation.

## Technical Shape

### 1. Recent Shred Ring Buffer

Introduce a dedicated relay cache ring buffer separate from parse-time dedupe:

1. Time-windowed ring (default target 10 seconds) with strict cap.
2. Keyed by shred identity: `(slot, index, variant, version, fec_set_index)`.
3. Stores raw shred bytes and minimal metadata for relay decisions.
4. Eviction is time-first and cap-enforced.

Default sizing note:
- Raw shred payload is roughly 1203-1228 bytes each; memory overhead is primarily index and allocation metadata, not only payload bytes.
- Final default caps must be chosen from measured traffic percentiles, not static assumptions.

### 2. Peer Relay Protocol

Define bounded protocol primitives:

1. Stream: forward newly seen shreds to subscribed peers.
2. Pull: peer requests recent ranges/missing slices.
3. Response: serve from ring buffer only within configured limits.

Protocol requirements:

1. Max request span and max response items/bytes.
2. Request timeout and cancellation behavior.
3. Explicit backpressure/drop behavior under overload.

Public network policy:

1. Any node may discover SOF relay-capable peers through gossip.
2. Any node may attempt to connect, but service remains bounded by hard per-peer and global limits.
3. No unbounded mode is provided for public operation.

Implemented interoperability details:

1. SOF now accepts Agave-compatible signed UDP repair requests (`WindowIndex`, `HighestWindowIndex`) on the repair ingress path.
2. Requests are verified with recipient binding, signature validation, and timestamp freshness window.
3. Responses are served from SOF's bounded relay cache and encoded as shred payload plus repair nonce (Agave wire-compatible).
4. Relay cache lookups now support exact-index and highest-index-above-threshold semantics for repair serving.
5. Telemetry exports repair-serve counters:
   - `repair_serve_requests_enqueued`
   - `repair_serve_requests_handled`
   - `repair_serve_responses_sent`
   - `repair_serve_cache_misses`
   - `repair_serve_rate_limited`
   - `repair_serve_errors`
   - `repair_serve_queue_drops`
6. Repair scheduling is stream-first:
   - keep Agave-compatible `250 ms` settle and `150 ms` request timeout semantics,
   - only escalate into stalled repair budgets after sustained shred-stream stagnation,
   - keep stalled-mode requests off the slot tip by default (`SOF_REPAIR_MIN_SLOT_LAG_STALLED=2`).
7. Repair peer selection follows Agave-style bounded sampling:
   - candidate peers are discovered from gossip repair/tvu metadata,
   - per-request peer choice uses a stake-weighted sampled subset (default size `10`),
   - local health scoring (RTT/send/source-hit) still ranks peers inside sampled candidates.
8. Repair serve path now applies a bounded response-byte budget (`12_000_000` bytes/sec default) to load-shed under pressure.
9. Runtime defaults keep repair pressure low (`SOF_REPAIR_MAX_REQUESTS_PER_TICK=4`, hard cap `24`) so live stream intake remains first priority.
10. Runtime relay fanout is hard-capped (`SOF_UDP_RELAY_FANOUT <= 200`) to avoid retransmit amplification.

Peer discovery policy:

1. Use gossip for broad peer discovery.
2. Add SOF capability advertisement/handshake to distinguish relay-capable SOF peers from generic gossip peers.
3. Maintain a bounded active upstream set selected by health, latency, and quality scoring.
4. Keep a small direct validator-facing ingest path as fallback and eclipse-risk mitigation.

### 3. Loop Prevention and Abuse Controls

Relay forwarding must prevent amplification:

1. Per-packet relay metadata with hop budget (or equivalent bounded forwarding marker).
2. Recent relay-origin dedupe to prevent bounce loops across peers.
3. Per-peer token bucket limits for stream and pull/serve paths.
4. Connection limits and optional peer allowlist controls.

Initial implementation posture for loop safety:

1. Forward only packets that pass parse, duplicate suppression, and verification gates.
2. Apply duplicate suppression before relay-cache insert and before outbound forwarding to reduce duplicate reserve pressure.
3. Keep relay fanout hard-capped (`<=200`) and enforce global send budgets per second.
4. Use source gating (`turbine` source ports by default) plus temporary send-error backoff to avoid retransmit inflation under fault conditions.

Agave-aligned guardrails to preserve:

1. Signed request envelopes for pull/repair-like requests, including recipient + freshness window checks.
2. Load-shed path before expensive verification when queue/budget pressure is high.
3. Stake-aware or trust-aware prioritization under pressure (for SOF peers this can be stake, reputation, or explicit allowlist tiers).
4. Explicit response byte budget per interval and stricter budget while node is in latency-sensitive duty.

### 4. Runtime Behavior

`sof-observer` runtime keeps relay/cache active by default:

1. Ingest from configured upstream source(s).
2. Insert valid shred packets into relay cache.
3. Act as a relay client by offering stream + pull/serve to peers under strict limits.
4. Export telemetry for ingress source mix, cache hit ratio, served shreds, rate-limited drops, and loop-prevention drops.

Emergency-disable switches may exist for incident response, but default deployment should always contribute relay/cache capacity.

### 5. Fork/Reorg Substrate (Generic, Non-Arb)

To support low-latency consumers that need commitment-aware decisions without RPC dependence:

1. Maintain a bounded in-memory slot-parent graph (`SOF_FORK_WINDOW_SLOTS`) derived from live/recovered shreds.
2. Emit generic slot-status transitions (`processed` / `confirmed` / `finalized` / `orphaned`) and canonical reorg events to plugins.
3. Keep this layer strategy-agnostic: no built-in account cache, trading logic, or app-specific rollback policy in core runtime.

## API and Config Direction

Configuration should be additive and conservative. Direction:

1. Keep explicit bind and gossip settings for network placement.
2. Add relay-cache controls:
   - `SOF_RELAY_CACHE_WINDOW_MS` (default target `10000`)
   - `SOF_RELAY_CACHE_MAX_SHREDS`
   - `SOF_UDP_RELAY_ENABLED`
   - `SOF_UDP_RELAY_REFRESH_MS`
   - `SOF_UDP_RELAY_PEER_CANDIDATES`
   - `SOF_UDP_RELAY_FANOUT`
   - `SOF_UDP_RELAY_MAX_SENDS_PER_SEC`
3. Add per-peer and global relay rate controls.
4. Avoid exposing a "disable relay/cache entirely" path in standard docs.

## Rollout Plan

1. Phase 1: Ring buffer and telemetry
   - Add cache data structure and metrics.
   - Mirror ingress packets into cache with bounded memory.
2. Phase 2: Pull/serve protocol
   - Add bounded request/response path using cache.
   - Add limits and overload behavior tests.
3. Phase 3: Loop prevention
   - Add forwarding markers and relay-origin dedupe.
   - Validate no ping-pong under mesh topologies.
4. Phase 4: Default-on migration
   - Promote relay/cache to default runtime behavior.
   - Keep temporary emergency toggles for rollback.
5. Phase 5: Docs and operations
   - Update architecture and ops docs with capacity planning guidance.

## Implementation Guidance

Implementation should prefer adopting proven Agave patterns over novel protocol design where practical:

1. Reuse bounded request/response semantics from repair-style flows (request families, response verification, nonce/expiry tracking).
2. Reuse stake/trust-aware prioritization and load-shed posture under pressure.
3. Reuse explicit byte-budget rate limiting patterns instead of ad-hoc throttles.
4. Reuse deterministic, bounded fanout selection ideas from turbine topology logic for peer relay upstream/downstream selection.
5. Port concepts, not blind constants: SOF-specific defaults must be benchmarked and documented.

## Alternatives Considered

1. Keep relay optional:
   - Rejected: adoption still scales validator pressure with node count.
2. Split network into relay and observer classes:
   - Rejected: operational complexity and non-cooperative defaults.
3. Validator-only ingest with no peer serving:
   - Rejected: fails scalability objective.

## Consequences

Positive:

1. SOF node growth is more likely to improve network resilience.
2. Lower validator-facing marginal demand per additional SOF node.
3. Better local recovery from short packet loss bursts.

Tradeoffs:

1. More protocol surface and state to secure/test.
2. Additional memory and bandwidth overhead on all nodes.
3. Need strict defaults to avoid accidental public relay abuse.

## Migration and Compatibility

1. Keep Agave-compatible repair serve/request behavior stable during rollout.
2. Introduce cache and UDP relay tuning in backward-compatible manner where possible.
3. Keep default-on cooperative posture after operational validation.

## Compliance Checks

1. Hot-path operations remain bounded (no unbounded queue or spawn).
2. Relay cache and serve limits are hard-enforced.
3. Loop-prevention and rate-limit counters are visible in telemetry.
4. Fuzz/integration tests cover malformed requests, amplification attempts, and mesh loop scenarios.

## Upstream Reference Files

The "Reference Signals from Agave" section is based on:

1. `agave/net-utils/src/lib.rs` (`VALIDATOR_PORT_RANGE`, `MINIMUM_VALIDATOR_PORT_RANGE_WIDTH`)
2. `agave/validator/src/cli.rs` (default dynamic port range wiring)
3. `agave/gossip/src/cluster_info.rs` (TVU socket defaults)
4. `agave/gossip/src/node.rs` (socket binding and advertised TVU/serve-repair endpoints)
5. `agave/turbine/src/cluster_nodes.rs` (`DATA_PLANE_FANOUT`, `MAX_NUM_TURBINE_HOPS`, TVU dedup behavior)
6. `agave/turbine/src/retransmit_stage.rs` (retransmit dedupe and cache posture)
7. `agave/core/src/repair/serve_repair.rs` (serve limits, signature checks, load shed, stake prioritization)
8. `agave/core/src/repair/repair_service.rs` (repair defer/timeout and weighted peer sampling)
9. `agave/core/src/repair/outstanding_requests.rs` (nonce/expiry tracking)
10. `agave/core/src/repair/packet_threshold.rs` (dynamic packet drop thresholding)
