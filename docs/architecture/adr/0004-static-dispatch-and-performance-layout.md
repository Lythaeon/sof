# ADR-0004: Static Dispatch and Performance Layout

- Status: Accepted
- Date: 2026-02-15
- Decision makers: `sof-observer` maintainers

## Context

The observer includes hot packet parse and reassembly paths where dispatch and memory layout directly affect throughput and latency.

## Decision

1. Prefer static dispatch via generics/trait bounds in hot paths.
2. Avoid `async_trait` when trait-variant patterns or associated futures keep static dispatch.
3. Keep hot paths cache-friendly.
4. Separate cold/error formatting-heavy paths from hot logic.
5. Apply inlining intentionally based on measurements.

## Consequences

- Lower runtime overhead in critical loops.
- More predictable performance behavior.
- Potentially more complex generic signatures.

## Compliance checks

- Performance-sensitive changes include profiling/benchmark evidence.
- Hot-path APIs avoid dynamic dispatch unless explicitly justified.
