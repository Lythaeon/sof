# ARD-0003: Slice Dependency Contracts

- Version: 1.0
- Date: 2026-02-15
- Applies to: crate `crates/sof-observer`

## Goals

- Reliability: avoid accidental coupling and hidden side effects.
- Efficiency: keep dependencies minimal and compile/test cycles smaller.
- Speed: preserve hot-path locality and avoid indirection overhead.
- Maintainability: clear ownership and module boundaries.

## Contract

- Slices are independent: `ingest`, `shred`, `reassembly`.
- Direct cross-slice calls are not allowed.
- Communication between slices happens only through infra orchestration.
- Ports define contracts; adapters satisfy contracts.

## Allowed dependency direction

- Domain types and logic stay inside owning slice.
- Infra/composition depends on slice APIs.
- Slices do not depend on each other internals.

## Enforcement

- Code review rejects cross-slice import violations.
- `cargo make arch-check` fails CI on forbidden slice-to-slice imports.
- `cargo make arch-check` also rejects `mod.rs` item definitions and inline module bodies.
- `mod.rs` files expose declarations/re-exports only.
- Integration tests validate composed behavior without breaking slice isolation.

## Refactoring rule

- If two slices need shared logic, extract a focused shared domain module with explicit ownership.
- Do not bypass boundaries with convenience imports.

## Exit criteria

1. New code respects slice ownership boundaries.
2. Cross-slice behavior is wired in infra only.
3. API changes preserve explicit contracts.
