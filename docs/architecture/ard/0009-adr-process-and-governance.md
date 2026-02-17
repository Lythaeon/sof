# ARD-0009: ADR Process and Governance

- Version: 1.0
- Date: 2026-02-15
- Applies to: architecture-level decisions in `sof`

## Goals

- Reliability: avoid undocumented architectural drift.
- Efficiency: faster decision reviews with consistent templates.
- Speed: reduce re-litigation of settled decisions.
- Maintainability: traceable rationale and evolution history.

## ADR required when

- Changing slice boundaries or dependency direction.
- Introducing new infra orchestration patterns.
- Changing error model, type-system policy, or dispatch strategy.
- Accepting performance tradeoffs that affect latency/throughput budgets.

## ADR template minimum

- Context and problem statement.
- Decision and alternatives considered.
- Consequences and tradeoffs.
- Migration plan and rollback strategy.
- Compliance and verification steps.

## Lifecycle

- Proposed -> Accepted -> Superseded/Deprecated.
- Superseding ADR must reference prior ADR explicitly.
- Code changes must link to accepted ADR when required.

## Governance

- Architecture-impacting PRs require maintainer review.
- Decisions are immutable once accepted; changes require a new ADR.
- Exceptions are time-boxed and tracked with owner.

## Exit criteria

1. Required changes include an ADR before merge.
2. ADR status is kept current as architecture evolves.
3. Superseded decisions remain discoverable.
