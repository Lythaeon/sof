# ADR-0003: Type-Driven Design and Typed Error Model

- Status: Accepted
- Date: 2026-02-15
- Decision makers: `sof-observer` maintainers

## Context

Primitive-heavy APIs and stringly errors hide invariants and make failure handling brittle.

## Decision

1. Newtypes are used for values with domain invariants.
2. Constructors/`TryFrom` enforce invariants at boundaries.
3. Domain/application errors use enums with `thiserror`.
4. Semantic error categories are not represented as ad-hoc strings.
5. Magic numbers are not allowed in domain/protocol logic; use named constants with documentation of meaning and source.

## Consequences

- Stronger compile-time guarantees and safer APIs.
- Better machine-matchable failure handling.
- Increased initial modeling effort.

## Compliance checks

- New invariant-bearing values are introduced as dedicated types.
- New failure modes are modeled as enum variants with structured context.
- Protocol offsets, limits, and flag masks are introduced as named constants and documented.
