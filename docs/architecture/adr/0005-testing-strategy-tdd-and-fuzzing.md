# ADR-0005: Testing Strategy, TDD, and Fuzzing

- Status: Accepted
- Date: 2026-02-15
- Decision makers: `sof-observer` maintainers

## Context

Reliability depends on proving invariants continuously and detecting malformed-input failures before production.

## Decision

1. TDD is the default workflow for behavior and invariant changes.
2. Unit and integration tests are required for domain behavior and composed flows.
3. Fuzz targets are required for parser and boundary-sensitive/state-machine logic.
4. Fuzz-discovered issues must become deterministic regression tests.

## Consequences

- Higher confidence and lower regression risk.
- Faster issue localization during refactors.
- Additional test authoring and CI runtime overhead.

## Compliance checks

- CI runs unit/integration tests and relevant fuzz targets.
- Bug fixes include regression tests.
