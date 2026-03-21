# ADR-0010: Dedicated Derived-State Feed for Official Stateful Extensions

- Status: Implemented
- Date: 2026-03-06
- Decision makers: `sof-observer` maintainers
- Related: `docs/architecture/adr/0009-derived-state-extensions-and-replay-contracts.md`, `docs/architecture/derived-state-extension-contract.md`, `docs/architecture/derived-state-feed-contract.md`, `docs/architecture/framework-plugin-hooks.md`, `docs/architecture/runtime-extension-hooks.md`

## Context

ADR-0009 established that official stateful extensions should begin outside core runtime, but
should not rely indefinitely on the existing `ObserverPlugin` or `RuntimeExtension` surfaces as
their authoritative state feed.

Current situation:

1. `ObserverPlugin` is decoded-event oriented, but its delivery model is intentionally
   best-effort under pressure.
2. `RuntimeExtension` is capability/resource/filter oriented, not decoded-state-feed oriented.
3. Official stateful systems such as a local bank, geyser-like local stream, or queryable
   account/program index need:
   - deterministic ordering,
   - explicit rollback semantics,
   - replay/checkpoint support,
   - visible lag and drop handling.

That gap suggests a third substrate.

## Decision

Introduce a new dedicated derived-state feed for official stateful extensions in a follow-up
implementation phase.

This feed will be separate from both:

1. `ObserverPlugin`, and
2. `RuntimeExtension`.

Its purpose is to deliver ordered, replayable, rollback-aware state events to long-lived
stateful consumers.

## Goals

1. Provide an explicit authoritative feed for official stateful extensions.
2. Keep observational hooks lightweight and independent from state-replication guarantees.
3. Keep runtime resource/capability management independent from state feed semantics.
4. Support snapshot + replay systems without making core runtime itself become the bank.
5. Surface lag, overflow, and rebuild semantics as first-class contract elements.

## Non-Goals

1. Replace `ObserverPlugin` for normal metrics/logging/classification plugins.
2. Replace `RuntimeExtension` for sockets/connectors and filtered packet resources.
3. Implement validator-grade bank execution semantics in SOF core.
4. Define durable storage format in this ADR.

## Feed Shape

The derived-state feed should be built from higher-level runtime truth, not raw packets.

Minimum event families:

1. ordered transaction-applied records,
2. ordered slot-status transition records,
3. ordered reorg records,
4. optional derived account-touch records,
5. replay/checkpoint metadata records.

The exact public type names are open, but the feed must preserve these semantics:

1. transaction/state events are ordered within a canonical stream,
2. slot and reorg events can invalidate prior provisional outputs,
3. account-touch events are explicitly derivative, not the primary source of truth,
4. replay metadata is sufficient for snapshot/rebuild orchestration.

## Delivery Contract

The feed contract must document:

1. ordering scope,
2. replay guarantees,
3. rollback guarantees,
4. lag/backpressure behavior,
5. checkpoint integration points.

Initial target contract:

1. ordered by runtime-defined canonical sequence,
2. replayable from persisted source or checkpoint + log,
3. rollback-aware via slot-status/reorg companion events,
4. bounded delivery with explicit lag/drop telemetry,
5. extension-visible watermark/checkpoint positions.

## Consumer Model

Stateful derived-state consumers should look more like materializers than ordinary plugins.

The consumer model should support:

1. startup from checkpoint,
2. feed catch-up,
3. steady-state live apply,
4. rollback/reorg handling,
5. graceful shutdown with checkpoint persistence.

Current implemented shape:

1. static `config() -> DerivedStateConsumerConfig`
2. optional `setup`
3. `load_checkpoint`
4. `apply`
5. `flush_checkpoint`
6. optional `shutdown`

The durability boundary remains `load_checkpoint` / `flush_checkpoint`, while startup and
shutdown hooks are consumer-local lifecycle helpers.

Current lifecycle timing:

1. host construction captures static subscriptions only,
2. `setup` runs when the host is initialized or first used for checkpoint/load/apply work,
3. `shutdown` runs during cooperative runtime teardown or worker drop after successful setup.

## Relationship to Existing Systems

### `ObserverPlugin`

Remains for:

1. observation,
2. metrics,
3. logging,
4. lightweight filters,
5. advisory side effects.

It should not absorb replay/checkpoint semantics.

### `RuntimeExtension`

Remains for:

1. capability-managed sockets/connectors,
2. filtered packet/resource streams,
3. extension-owned runtime resources.

It may be used alongside a derived-state consumer, but it should not become the state-feed
contract itself.

### Official Stateful Extension

A future local bank or geyser-like extension may combine:

1. the dedicated derived-state feed for authoritative inputs,
2. `RuntimeExtension` for connectors/query endpoints if needed,
3. optional `ObserverPlugin` usage for extra advisory signals.

## Alternatives Considered

### 1. Keep using `ObserverPlugin`

Rejected because:

1. it is intentionally best-effort under queue pressure,
2. it is optimized for observation, not checkpoint/replay,
3. forcing stronger guarantees into it would harm its existing role.

### 2. Extend `RuntimeExtension` to carry decoded-state events

Rejected because:

1. it is centered on resources and packet filters,
2. replay/rollback semantics are orthogonal to capability/resource ownership,
3. mixing the two would make the abstraction muddy.

### 3. Move bank/state directly into core runtime

Rejected for now because:

1. it commits core too early,
2. it reduces architectural flexibility,
3. external official extensions are a better proving ground for contract discovery.

## Consequences

### Positive

1. A stateful extension gets the right abstraction instead of abusing an observational one.
2. Core runtime stays disciplined while still supporting serious derived-state systems.
3. Contract gaps become explicit, reviewable, and testable.

### Negative

1. Adds a third integration surface to the project.
2. Requires careful naming and lifecycle design to avoid overlap/confusion.
3. Introduces more up-front API work before bank-like extensions feel complete.

## Initial Implementation Questions

1. What is the canonical sequence unit for the feed?
2. What minimum metadata is required for exact replay?
3. How should lag and dropped-feed conditions be surfaced?
4. Should feed persistence live in core or remain consumer-owned?
5. What test matrix is required to prove replay == live apply?

## Compliance Checks

Before adopting a first implementation of the feed:

1. feed ordering is documented,
2. rollback behavior is documented,
3. replay source/checkpoint model is documented,
4. lag/drop semantics are documented,
5. at least one official stateful consumer validates the abstraction end-to-end.

The current concrete target contract is captured in `docs/architecture/derived-state-feed-contract.md`.
