# ADR-0013: Runtime Delivery Profiles

- Status: Proposed
- Date: 2026-04-11

## Context

SOF's current runtime model has a clear bias:

- protect ingest first
- keep queues bounded
- avoid stalling hot paths on slow downstream consumers
- prefer freshness over completeness on observational surfaces under pressure

That bias is correct for low-latency Solana workloads, but it is not the right operating posture
for every consumer shape.

Some adopters primarily want:

- the lowest possible callback latency for trading/searcher paths
- more controlled degradation for monitoring and analytics paths
- stronger downstream ordering and drain discipline for restart-safe stateful consumers

Today those tradeoffs are mostly implicit in one runtime philosophy. That has two problems:

1. adopters must accept one bundled bias even when their workload is different
2. opening the wrong level of configurability would turn the runtime into an untestable pile of
   independent toggles

The requirement is therefore not "make everything configurable". The requirement is:

- preserve one coherent runtime model
- make the runtime's delivery bias explicit
- keep the ingest hot path bounded and protected in every supported mode

This ADR also aligns with:

- ADR-0008: runtime-owned extension and ingress boundaries remain host-owned
- ADR-0009 and ADR-0010: derived-state remains the replay-safe and rollback-aware contract
- ARD-0007: runtime orchestration and queueing policy remain explicit infrastructure decisions

## Decision

SOF will expose one typed runtime policy enum that selects the runtime's downstream delivery bias.

Type name:

- `RuntimeDeliveryProfile`

Initial variants:

- `LatencyOptimized`
- `Balanced`
- `DeliveryDisciplined`

This enum is a policy bundle, not a bag of independent low-level toggles.

Naming note:

- `DeliveryDisciplined` is preferred over `DeliveryOptimized` because it signals stronger
  ordering/drain discipline without implying durability, completeness, or upgraded upstream
  guarantees.

The profile selection MUST:

- be represented as a typed enum in Rust
- be mirrored as a typed enum in TypeScript and Python SDKs
- fail configuration loading on unknown values instead of silently falling back
- apply at the runtime-instance level, not per hook or per plugin in the first version

The profile selection MUST NOT:

- change the fundamental ingest ownership model
- allow unbounded queues
- promise lossless behavior on surfaces that are observational by design
- turn plugin callbacks into the replay-safe contract

The default profile MUST preserve current SOF behavior as closely as possible. That means the
first implementation default is:

- `LatencyOptimized`

## Profile Invariants

Across all `RuntimeDeliveryProfile` variants, the following remain true:

- ingress remains bounded
- ingest is never blocked by observational consumers
- plugins and runtime extensions are not the replay-safe contract
- derived-state remains the restart-safe, rollback-aware surface
- no profile upgrades upstream provider durability or transport semantics
- no profile permits unbounded memory growth

## Operational Definitions

Terms used in this ADR:

- hot lanes: ingress-adjacent or latency-sensitive paths that continue to protect ingest budget in
  every profile
- non-hot lanes: runtime-owned downstream queues whose delivery discipline may change by profile

`Backpressure` in this ADR means only runtime-owned downstream admission control on non-hot lanes.
It may include:

- producer-side waiting on non-hot owned queues
- reduced concurrency or serialized draining
- temporary suppression of non-critical fanout
- slowdown of extension or consumer work admission

It does not mean:

- blocking provider ingest
- stalling socket ownership or protocol parsing
- claiming upstream systems now offer stronger retention

`Ordering` in this ADR is scoped explicitly:

- within a lane: queue and callback order for one runtime-owned lane
- across lanes: no global total ordering is introduced by any profile
- callbacks for same event: callbacks on same owned lane follow that lane's policy; callbacks on
  different lanes may still observe and complete in different orders
- first version: sequential or reduced-concurrency behavior is profile-driven runtime config, not a
  per-plugin or per-hook override

## Delivery Profile Semantics

The following sections define the minimum contract for each profile. Exact numeric queue sizes,
concurrency limits, and timeout budgets remain implementation details, but the behavioral meaning
must stay stable.

### `LatencyOptimized`

This is the current SOF-style operating posture.

Required behavior:

- ingest never waits on plugin or extension consumers
- bounded queues remain tight
- observational lanes may drop new work under pressure
- no producer-side waiting is introduced on downstream-owned queues
- concurrent downstream dispatch is allowed where that reduces latency and does not violate an
  existing ordering contract
- lane-local ordering is preserved only where an existing queue contract already provides it
- no cross-lane ordering guarantee is introduced
- shutdown drain budgets stay short and explicit
- freshness is preferred over completeness on observational surfaces

Intended use:

- bots
- searchers
- latency-sensitive monitors
- callback-driven consumers that care more about newest data than full retention

### `Balanced`

This is the general-purpose profile for users who still want bounded latency but want less
aggressive shedding.

Required behavior:

- ingest still does not wait on slow downstream consumers
- queue budgets are larger than `LatencyOptimized`
- pressure signaling and telemetry are stricter and earlier
- on non-hot owned lanes, runtime may slow admission, reduce concurrency, or temporarily suppress
  non-critical fanout before shedding
- downstream dispatch should prefer stable lane-local FIFO ordering where practical on non-hot lanes
- no cross-lane ordering guarantee is introduced
- observational lanes may still shed work, but only after stronger buffering and clearer pressure
  escalation than `LatencyOptimized`
- shutdown drain budgets are longer than `LatencyOptimized`, but still bounded

Intended use:

- mixed production services
- monitoring systems
- user-facing services that want controlled degradation without paying full delivery cost

### `DeliveryDisciplined`

This is the strongest downstream delivery discipline SOF can honestly provide without violating its
bounded-ingest model.

Required behavior:

- ingest ownership and boundedness remain intact
- downstream queues are the largest allowed by the selected profile set
- on non-hot owned lanes, runtime should exhaust bounded admission slowdown, reduced concurrency,
  serialized draining, and temporary non-critical fanout suppression before shedding owned work
- within a non-hot owned lane, deterministic FIFO ordering is the default unless a lane-specific
  contract explicitly says otherwise
- callbacks on same non-hot owned lane should default to serialized draining unless that lane
  already has an explicit concurrency contract
- no cross-lane ordering guarantee is introduced, including for callbacks triggered by same event
- shutdown drain behavior is strongest here, but still bounded
- pressure must surface before shedding
- where a consumer needs replay, rollback, or restart safety, the runtime should direct that user
  to derived-state contracts rather than claiming plugin callbacks are now durable

Non-guarantees:

- this profile does not mean "no data loss"
- it does not mean "high retention with eventual completeness"
- it does not upgrade upstream provider guarantees
- it does not make websocket feeds stronger than websocket
- it does not make observational plugin callbacks equivalent to derived-state

Intended use:

- stateful consumers that value ordered downstream processing
- analytics/monitoring systems that prefer higher retention and more predictable degradation
- adopters willing to pay more latency and resource cost for stronger runtime discipline

## Scope of Influence

`RuntimeDeliveryProfile` is allowed to influence:

- plugin dispatch queue sizing and overflow behavior
- runtime-extension worker queue sizing and overflow behavior
- downstream admission control on non-hot lanes, including producer-side waits to owned queues,
  reduced concurrency, serialized draining, and temporary suppression of non-critical fanout
- downstream ordering and concurrency defaults on non-hot lanes
- drain budgets during shutdown
- observability thresholds, warning levels, and degradation signaling
- derived-state consumer buffering defaults, warning thresholds, and checkpoint-adjacent queue
  sizes only where runtime already owns those mechanics

`RuntimeDeliveryProfile` is not allowed to influence:

- raw ingest ownership of sockets or provider streams
- boundedness of ingress queues
- protocol parsing correctness rules
- replay, checkpoint, or cursor semantics already defined by derived-state contracts
- state-correctness posture or rollback meaning of derived-state surfaces
- transport capability limits inherited from provider families

Guardrail:

> Runtime profiles change SOF's downstream queueing, admission, ordering, and drain defaults. They
> do not change ingest boundedness, replay guarantees, or provider durability.

## Configuration Surface

The first supported shape is one runtime-level field:

```rust
pub enum RuntimeDeliveryProfile {
    LatencyOptimized,
    Balanced,
    DeliveryDisciplined,
}
```

This profile should live in typed runtime configuration, not as a stringly bag of booleans.

Allowed configuration entry points:

- Rust config builders and typed config structs
- generated TypeScript and Python SDK config types
- environment or file decoding layers that parse into the typed enum and reject unknown values

The first version MUST NOT add:

- per-hook profile selection
- per-plugin fairness toggles
- arbitrary queue-policy combinators
- hidden fallback from one profile to another

Future narrower overrides, if ever added, require a follow-up ADR and must be justified by a real
measured workload rather than speculative flexibility.

## Guarantees and Non-Guarantees

The docs for this feature must define, per profile:

- whether work may be dropped under pressure
- which owned lanes may apply producer-side waiting or other admission slowdown
- which runtime-owned admission slowdowns are allowed before shedding
- ordering expectations within a lane
- explicit absence of global ordering across lanes
- concurrency expectations, including same-event callback behavior across lanes
- shutdown drain expectations
- memory and latency tradeoffs
- the boundary between observational surfaces and replay-safe surfaces

The docs must not claim:

- exactly-once delivery
- universal losslessness
- fairness guarantees that are not actually enforced
- replay guarantees outside derived-state

## Alternatives Considered

### 1. Many independent runtime toggles

Rejected because it would make the runtime harder to reason about, harder to test, and easier to
misconfigure. SOF needs legible operating modes, not policy chaos.

### 2. Keep one hard-coded policy forever

Rejected because SOF already serves multiple workload shapes, and one implicit bias makes adoption
harder for consumers who want stronger downstream discipline.

### 3. Expose only queue sizes without semantic profiles

Rejected because raw numeric knobs do not communicate the contract. Operators need named behavior
modes with explicit guarantees and non-guarantees.

### 4. Keep `DeliveryOptimized` naming

Rejected because users could still read it as "higher retention" or "almost durable".
`DeliveryDisciplined` better matches actual contract: stronger downstream discipline under bounded
ingest, not durable delivery.

### 5. Promise a "durable" or "no loss" profile

Rejected because SOF cannot honestly guarantee that across all surfaces and provider families.

## Consequences

Positive:

- SOF becomes easier to adopt across more workload types without splitting into separate runtimes
- the runtime bias becomes explicit and reviewable
- TS/Python and Rust integrations can share one typed policy model
- operational tradeoffs become documentable and testable

Trade-offs:

- testing burden increases because the runtime now has multiple supported operating profiles
- benchmark and soak validation must cover profile differences where behavior changes
- some users may still ask for per-plugin tuning that this ADR deliberately does not allow

## Migration and Rollout

The first implementation should roll out in stages:

1. introduce the typed enum and keep `LatencyOptimized` as the default
2. document exact profile contracts before widening knobs
3. add targeted regression coverage for shedding, admission slowdown, drain, and ordering semantics
4. expose the same enum through TS/Python SDK generation
5. only after the global profile is stable, evaluate whether any narrower override is justified

Rollback strategy:

- if profile semantics prove incoherent or too expensive to verify, keep the enum internal or
  remove non-default variants before a stable release that documents them publicly

## Compliance and Verification

Implementation work for this ADR is not complete until the following exist:

- regression tests that prove the documented drop or drain behavior for each profile
- explicit observability for pressure escalation and shedding decisions
- docs that define profile guarantees and non-guarantees without marketing language
- TS/Python enum generation that preserves typed selection and typed failure on unknown values
- validation that no profile allows ingest to be stalled by slow downstream consumers
- behavioral regression scenarios covering burst pressure on plugin lanes, slow consumer on a
  non-hot lane, shutdown with queued work, provider reconnect plus backlog, mixed transaction and
  general queue pressure, and derived-state buffering/warning behavior where profile influence is
  allowed
- soak or benchmark coverage where profile-specific queueing behavior materially differs from default
