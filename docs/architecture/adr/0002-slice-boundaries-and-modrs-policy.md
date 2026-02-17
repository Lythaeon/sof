# ADR-0002: Slice Boundaries and `mod.rs` Policy

- Status: Accepted
- Date: 2026-02-15
- Decision makers: `sof-observer` maintainers

## Context

Slice coupling and module leakage increase change risk and make regressions harder to reason about.

## Decision

1. Slices do not call each other directly.
2. `mod.rs` contains only module declarations and re-exports.
3. Composition/infrastructure is the only place where slice interactions are wired.

## Consequences

- Lower coupling and fewer hidden side effects.
- Easier independent testing and refactoring of slices.
- More explicit composition work in infra.

## Compliance checks

- Code review rejects direct cross-slice imports/calls.
- Code review rejects implementation logic in `mod.rs`.
