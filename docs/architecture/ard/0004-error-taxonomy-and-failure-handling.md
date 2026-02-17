# ARD-0004: Error Taxonomy and Failure Handling

- Version: 1.0
- Date: 2026-02-15
- Applies to: workspace `sof`

## Goals

- Reliability: predictable failure behavior and recoverability.
- Efficiency: cheap classification and routing of failures.
- Speed: avoid expensive error formatting on hot paths.
- Maintainability: machine-matchable error categories.

## Error model

- Use typed enums with `thiserror` for domain/application errors.
- Avoid stringly semantic categories.
- Preserve structured context fields (slot/index/source) in variants.

## Classification

- Validation errors: bad inputs, malformed packets.
- Domain state errors: invariant break, impossible transitions.
- Infra errors: IO, channel/backpressure, runtime task failures.

## Handling policy

- Parse/validation failures are expected and non-fatal when possible.
- Infra failures should be surfaced with actionable context.
- Retriable vs terminal failures must be explicit in variant naming/docs.

## Logging policy

- Log structured fields, not only display text.
- Avoid heavy string formatting in hot loops.
- Attach correlation keys for diagnosis.

## Exit criteria

1. New failure paths add enum variants, not string categories.
2. Callers can branch by variant for policy decisions.
3. Error logs include structured diagnostic fields.
