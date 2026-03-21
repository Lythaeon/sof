# ADR-0009: External Derived-State Extensions and Replay Contracts

- Status: Implemented
- Date: 2026-03-06
- Decision makers: `sof-observer` maintainers
- Related: `docs/architecture/framework-plugin-hooks.md`, `docs/architecture/runtime-extension-hooks.md`, `docs/architecture/derived-state-extension-contract.md`, `docs/architecture/derived-state-feed-contract.md`, `docs/architecture/adr/0008-runtime-extension-capability-and-filtered-ingress.md`, `docs/architecture/ard/0007-infrastructure-composition-and-runtime-model.md`

## Context

`sof` is moving beyond packet observation into a local Solana data-plane:

1. packet ingest,
2. shred parsing and reassembly,
3. transaction decoding and classification,
4. local slot/reorg tracking, and
5. leader-aware transaction submission.

The next obvious layer is derived state on top of that flow: local-bank style materialization, geyser-like streams, account discovery/invalidation, and program-specific indexes.

That direction is valid, but it raises a structural risk. If stateful consumers are implemented as ordinary plugins without an explicit contract, core correctness will leak into ad hoc extension code:

1. ordering assumptions will diverge,
2. rollback semantics will become inconsistent,
3. replay/rebuild behavior will be undefined,
4. resource usage will be unbounded or implicit, and
5. "best effort observability" will be mixed with "authoritative local state."

We need a deliberate model for official derived-state systems before a local bank or geyser-like stream is adopted as a first-class SOF capability.

## Decision

Adopt an external-first model for derived-state systems.

1. A local bank or geyser-like state engine will begin as an official extension on top of `sof`, not as immediate core runtime state.
2. `sof` core remains responsible for ingest, ordering, decode, commitment/reorg truth, and explicit event contracts.
3. Official derived-state extensions must be built against documented replay, ordering, rollback, and resource contracts rather than relying on implicit plugin behavior.
4. Promotion of extension concepts into core requires proof that the concept is fundamental, reused, and impossible to express cleanly through the extension contract.

This ADR does not define the full extension API implementation yet. It defines the required contract categories and architectural guardrails.

## Substrate Direction

Official stateful extensions should not overload the existing surfaces as-is.

Direction:

1. `ObserverPlugin` remains the primary observational decoded-event hook system.
2. `RuntimeExtension` remains the primary capability/resource/filter substrate.
3. Official deterministic stateful consumers should target a new dedicated derived-state substrate in a follow-up ADR, rather than forcing replay/rollback semantics into either existing system.

Reasoning:

1. `ObserverPlugin` delivery is intentionally best-effort under pressure to protect ingest latency.
2. `RuntimeExtension` is resource/packet centric rather than decoded-state centric.
3. A local bank or geyser-like stream needs explicit ordering, rollback, replay, and checkpoint semantics that neither current subsystem was designed to guarantee.

Interim policy:

1. prototypes may use `ObserverPlugin`,
2. official stateful extensions must document where they depend on stronger guarantees than `ObserverPlugin` currently provides,
3. missing guarantees discovered by those prototypes become input to the follow-up substrate ADR.

## Goals

1. Keep core runtime scope disciplined while enabling richer official extensions.
2. Make replay, rollback, and ordering semantics explicit before stateful extensions depend on them.
3. Preserve the distinction between observational hooks and deterministic derived-state inputs.
4. Allow a local bank or geyser-like stream to evolve quickly outside core while still being "official."
5. Create criteria for when a derived-state concept should move from extension space into core runtime/state abstractions.

## Non-Goals

1. Implement a local bank inside core runtime in this ADR.
2. Define validator-grade account execution or bank semantics in `sof` v1.
3. Treat every existing plugin callback as authoritative enough for state materialization.
4. Replace `ObserverPlugin` or `RuntimeExtension` immediately.

## Required Contract Areas

Any official derived-state extension must have answers for the following.

### 1. Input Guarantees

The extension contract must define:

1. which event families are authoritative inputs,
2. which event families are advisory only,
3. whether delivery is at-most-once, at-least-once, or best-effort,
4. whether ordering is global or scoped, and
5. what metadata is guaranteed on every event.

At minimum, SOF must classify input surfaces into two groups:

1. observational hooks:
   - useful for metrics, inspection, side effects, and best-effort logic,
   - not sufficient by themselves for authoritative local state;
2. derived-state feeds:
   - explicitly ordered,
   - replayable,
   - rollback-aware,
   - intended for deterministic consumers.

### 2. Ordering Model

Before an official bank-like extension exists, SOF must define the canonical ordering unit for derived-state consumers.

Candidate units:

1. raw packet,
2. parsed shred,
3. reconstructed dataset,
4. decoded transaction,
5. slot transition.

Decision criteria:

1. determinism under replay,
2. ability to express reorgs cleanly,
3. ability to rebuild downstream state from persisted inputs,
4. performance and queue pressure implications.

Current guidance:

1. raw packet and parsed shred events are too low-level for most derived state,
2. decoded transaction plus slot transition events are the likely minimum useful contract,
3. account-touch style events are derivative and should not become the sole source of truth.

### 3. Rollback and Reorg Semantics

Derived-state extensions must not invent their own rollback meaning.

The contract must define:

1. when an event is provisional,
2. when it becomes confirmed,
3. when it becomes finalized,
4. how revocation is expressed on reorg,
5. whether consumers must support rewind, compensation events, or rebuild-from-checkpoint.

Minimum bar:

1. slot status and reorg transitions must be usable to invalidate derived state,
2. replay must produce the same result as live processing from the same ordered input sequence,
3. any extension claiming geyser-like semantics must document how non-finalized outputs are revoked or superseded.

### 4. Replay and Rebuild

Official derived-state systems must define a rebuild strategy before they are considered stable.

The design must answer:

1. what persisted input or snapshot source is used for rebuild,
2. whether rebuild is exact or approximate,
3. how checkpoints are versioned,
4. how schema changes are migrated,
5. what happens after crash recovery or partial persistence loss.

Acceptable models may include:

1. replay from persisted ordered event logs,
2. snapshot + incremental replay,
3. snapshot-only with explicit best-effort semantics.

The chosen model must be explicit. Silent "reconstruct from whatever callbacks happened to arrive" is not acceptable for official derived-state systems.

### 5. Resource and Isolation Model

Derived-state extensions are long-lived stateful systems, not lightweight event listeners.

The contract must therefore define:

1. memory budgets,
2. queue and backpressure behavior,
3. snapshot/write amplification limits,
4. startup and shutdown behavior,
5. fault containment when an extension falls behind or crashes.

Core runtime must remain able to:

1. detect extension lag,
2. surface drops or overload explicitly,
3. prevent one extension from degrading ingest correctness.

### 6. Output and Query Model

Official extensions should expose outputs through a deliberate interface, not only side effects in callbacks.

Candidate output surfaces:

1. query API for point-in-time reads,
2. append-only stream for downstream consumers,
3. snapshot export,
4. invalidation/update feed.

Every output surface must document:

1. consistency level,
2. freshness semantics,
3. rollback behavior,
4. compatibility guarantees.

## Architectural Guidance

### Core Runtime Responsibilities

Core should own:

1. ingress and decode correctness,
2. ordering and lifecycle guarantees,
3. slot/commitment/reorg truth,
4. replayable event contracts,
5. bounded dispatch and observability.

### Official Extension Responsibilities

Official derived-state extensions should own:

1. state materialization,
2. persistence,
3. query indexing,
4. domain-specific projections,
5. rebuild policies within the contract defined by core.

### Promotion Criteria

An extension concept may move into core only when at least one of the following is true:

1. multiple official extensions need the same primitive,
2. correctness cannot be achieved from the external contract,
3. the runtime must coordinate the feature centrally for safety or performance,
4. the abstraction becomes foundational rather than domain-specific.

## Consequences

### Positive

1. Core remains disciplined and execution-adjacent rather than becoming an accidental validator clone.
2. Official stateful systems can evolve quickly without forcing premature core commitments.
3. Missing runtime guarantees become visible early through extension pressure.
4. Replay/reorg semantics are treated as architecture work, not plugin folklore.

### Negative

1. More upfront architecture work is required before shipping ambitious extensions.
2. Some useful extension features will need contract work before implementation.
3. The extension/core boundary may need multiple iterations before it stabilizes.

## Initial Work Items

1. Classify current hooks and runtime outputs into observational vs derived-state-capable surfaces.
2. Define the minimum replayable event contract for official stateful extensions.
3. Document provisional/confirmed/finalized/reorg semantics for extension consumers.
4. Define extension lag/backpressure telemetry for stateful official extensions.
5. Draft the follow-up ADR for a dedicated derived-state substrate.

## Compliance Checks

Before adopting an official bank-like extension:

1. its ordering unit is documented,
2. rollback behavior is documented,
3. replay/rebuild strategy is documented,
4. resource limits are documented,
5. its required runtime guarantees are mapped either to existing contracts or to follow-up ADRs.
