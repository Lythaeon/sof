# ARD-0005: Type System and Newtype Guidelines

- Version: 1.0
- Date: 2026-02-15
- Applies to: workspace `sof`

## Goals

- Reliability: encode invariants at compile-time and construction time.
- Efficiency: avoid repeated runtime validation.
- Speed: keep hot-path checks minimal after validated construction.
- Maintainability: self-documenting APIs and reduced primitive obsession.

## Rules

- Introduce newtypes for IDs, indexes, ranges, validated buffers, and bounded values.
- Constructors or `TryFrom` enforce invariants once at boundaries.
- Keep invalid states unrepresentable where practical.

## API conventions

- Prefer explicit constructors over exposed mutable fields.
- Expose domain-safe accessors.
- Derive only traits that preserve semantics.

## Constants and literals policy

- Replace semantic numeric literals with named constants.
- Keep constants close to the owning domain/protocol module.
- Add short docs for each constant explaining unit, meaning, and origin.
- Group related constants (for example offsets, masks, limits) to keep usage consistent.

## Validation strategy

- Validate early at input boundaries.
- Keep internal core logic operating on validated types.
- Reflect violations using typed errors.

## Testing

- Unit tests cover valid/invalid constructors.
- Property/fuzz tests target boundary values and malformed payloads.

## Exit criteria

1. New domain values with invariants are typed, not primitives.
2. Validation occurs at boundaries, not repeatedly in core loops.
3. Invariant violations map to typed errors.
4. Magic numbers are replaced by documented constants.
