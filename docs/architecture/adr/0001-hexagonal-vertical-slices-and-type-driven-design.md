# ADR-0001: Hexagonal Vertical Slices and DDD

- Status: Accepted
- Date: 2026-02-15
- Decision makers: `sof-observer` maintainers

## Context

The observer handles packet ingest, shred parsing, and slot reassembly under strict correctness and throughput constraints. We need a structural model that preserves ownership and keeps domain logic isolated from infrastructure details.

## Decision

The project adopts hexagonal architecture with vertical slices aligned to DDD capabilities.

1. Slices are organized by domain capability, not technical layer.
2. Each slice defines explicit ports and domain contracts.
3. Adapters implement ports at boundaries and do not leak infra concerns inward.
4. Infrastructure coordinates slice interactions.

## Consequences

- Clear dependency direction and ownership.
- Better maintainability through explicit boundaries.
- Higher upfront design effort when introducing new slices.

## Compliance checks

- Architecture/code reviews verify slice ownership and boundary clarity.
- Cross-slice flow is introduced only via infra orchestration.
