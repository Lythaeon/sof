# Decision Records Map

The repository keeps two document families under `docs/architecture/`.

## ADRs

Architecture Decision Records capture durable decisions and tradeoffs.

Key ADR themes in this repository:

- vertical slices and type-driven design
- module boundary rules and `mod.rs` policy
- static dispatch and performance layout
- testing and fuzzing requirements
- transaction SDK routing model
- always-on relay and shred-cache mesh
- runtime extension capability model
- plugin config and lifecycle hooks
- derived-state replay contracts
- runtime-owned observability endpoints

Use ADRs when you need to understand why a decision exists and what tradeoff was accepted.

## ARDs

Architecture Requirements Documents capture the current engineering rules the repository expects
contributors to follow.

Key ARD themes:

- project structure and code goals
- testing strategy and quality gates
- slice dependency contracts
- error taxonomy
- newtype and value-object guidance
- hot-path performance discipline
- observability and operability standards
- ADR process and governance

Use ARDs when you are making changes and need to know the non-negotiable constraints.

## When To Add A New ADR

Add an ADR when a change affects:

- slice boundaries or dependency direction
- orchestration patterns
- error-model or type-system policy
- meaningful latency or throughput tradeoffs

If the change is only clarifying current expected behavior, it often belongs in an ARD or regular
documentation update instead.
