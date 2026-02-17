# ARD-0001: Project Structure and Code Goals

- Version: 1.0
- Date: 2026-02-15
- Applies to: workspace `sof`, crate `crates/sof-observer`

## Current structure manifest

```text
crates/sof-observer/src/
  app/
    config/
      mod.rs
    runtime.rs
    state.rs
  ingest/
    mod.rs
  reassembly/
    mod.rs
    slot.rs
  shred/
    mod.rs
    wire/
      mod.rs
  verify/
    core/
      mod.rs
  lib.rs
  runtime.rs
```

### Slice responsibilities

- `ingest`: UDP packet ingress boundary.
- `shred`: wire-level shred parsing and protocol representation.
- `reassembly`: slot accumulation and completion logic.
- `app/*`: runtime composition internals and config/state plumbing.
- `runtime.rs`: public runtime entrypoint facade (`run`/`run_async`) for embedding.
- `lib.rs`: public module exposure.

## Target architecture

### 1) Hexagonal, vertical slices, DDD

- Organize by business capability (slice) first.
- Keep domain rules inside the owning slice.
- Represent slice boundaries with explicit ports.

### 2) No cross-calls between slices

- `ingest`, `shred`, and `reassembly` remain independent.
- One slice must not import another slice's internals.
- Cross-slice flow is coordinated only by infra/composition.

### 3) Infra orchestrates slices

- Composition root wires ports/adapters.
- Infra handles runtime concerns (tasks, sockets, channels, scheduling).
- Domain slices remain runtime-framework agnostic where practical.

### 4) `mod.rs` policy

- `mod.rs` files only contain module declarations and re-exports.
- No executable logic, no domain rules, no parsing/reassembly implementation in `mod.rs`.

### 5) Type-driven design

- Introduce newtypes for identifiers, bounds, and validated values.
- Use constructors/`TryFrom` to enforce invariants at boundaries.
- Favor impossible-state-unrepresentable modeling.

### 6) Error model

- Domain/application errors are enum-based and derived with `thiserror`.
- Avoid free-form string literals for semantic error categories.
- Keep parse/validation error variants precise and machine-matchable.

### 7) Dispatch strategy

- Prefer static dispatch in core paths.
- Avoid `async_trait` where it introduces unnecessary dynamic dispatch.
- Use trait-variant patterns (for example, with `trait_variant`) when async ergonomics are needed without abandoning static dispatch.

### 8) Performance layout

- Keep hot parse/reassembly paths cache-friendly.
- Use inlining intentionally on hot functions where measurements support it.
- Separate cold/error paths and heavy formatting from hot logic.

### 9) Fuzzing and invariant hardening

- Add fuzz targets for wire parsing and reassembly boundaries.
- Focus on malformed/truncated/corrupted packet inputs and state-machine edge cases.
- Promote findings into invariant-enforcing types and targeted regression tests.

### 10) Test-Driven Development (TDD)

- Treat tests as executable specifications for domain behavior.
- For behavior changes, add or update a failing test first, then implement to pass.
- Keep fast unit tests close to owning slices; use integration tests for infra wiring and cross-slice orchestration.
- Add regression tests for every bug fix and every discovered invariant violation.

### 11) Magic numbers and constants

- Avoid magic numbers in domain, protocol, and hot-path logic.
- Extract offsets, masks, limits, and defaults into named constants.
- Document each constant with what it represents and where it comes from (protocol spec, benchmark, operational limit, or safety bound).

## Acceptance criteria for new code

1. New feature is implemented inside a single owning slice.
2. Cross-slice interactions happen only through infra wiring.
3. `mod.rs` remains declaration/re-export only.
4. New domain values use dedicated types where invariants exist.
5. Error surfaces are enum + `thiserror`, no stringly categories.
6. Critical-path APIs favor static dispatch.
7. Relevant fuzz target exists or is updated.
8. Tests are introduced or updated first (or in the same change) to specify expected behavior and invariants.
9. Numeric literals with semantic meaning are replaced by documented named constants.
