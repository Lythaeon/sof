# ARD-0002: Testing Strategy and Quality Gates

- Version: 1.0
- Date: 2026-02-15
- Applies to: workspace `sof`

## Goals

- Reliability: catch regressions and invariant violations early.
- Efficiency: keep feedback loops fast and deterministic.
- Speed: optimize for fast local and CI execution.
- Maintainability: keep tests readable, isolated, and intent-revealing.

## Test pyramid

1. Unit tests in owning slice modules for domain behavior and invariants.
2. Integration tests for infra wiring and cross-slice orchestration at composition boundaries.
3. Fuzz targets for parser/state-machine edge cases and malformed inputs.

## TDD policy

- For behavior changes, write/update failing tests first.
- Implement minimal code to pass.
- Refactor only after green.

## Invariant testing

- Every invariant in a type constructor must have positive and negative tests.
- Every bug fix must add a regression test.
- Fuzz findings must be converted into deterministic regression tests.

## Quality gates

- PR gate requires passing unit and integration tests.
- Parser/reassembly changes require fuzz target execution in CI.
- Flaky tests are treated as defects and fixed before merge.

## Performance guardrails for tests

- Unit tests must remain fast and deterministic.
- Avoid network/time randomness unless isolated behind test doubles.
- Keep fixture sizes representative but minimal.

## Exit criteria

1. New behavior has tests proving expected outcomes.
2. Invariants are covered by tests and type-level checks.
3. No failing or flaky test remains in the merge path.
