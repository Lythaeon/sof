# Architecture Docs

This folder captures two complementary architecture artifacts:

- `adr/`: Architecture Decision Records (immutable decisions and rationale).
- `ard/`: Architecture Requirements Documents (current structure and engineering constraints).

## Scope

These docs apply to the `sof-observer` workspace and crate layout.

Network contribution model:

- SOF is an active relay client by default, not a passive observer-only consumer.
- In architecture docs, "relay tier" refers to SOF nodes that receive, cache, and re-serve recent shreds within strict bounds.
- See ADR-0007 for the bounded relay/repair policy details.

Runtime playbooks are in `docs/operations/`.

User-facing crate docs:

- Observer runtime crate: `crates/sof-observer/README.md`
- Transaction SDK crate: `crates/sof-tx/README.md`

## Current Artifact Index

- ADR-0001: `adr/0001-hexagonal-vertical-slices-and-type-driven-design.md`
- ADR-0002: `adr/0002-slice-boundaries-and-modrs-policy.md`
- ADR-0003: `adr/0003-type-driven-design-and-error-model.md`
- ADR-0004: `adr/0004-static-dispatch-and-performance-layout.md`
- ADR-0005: `adr/0005-testing-strategy-tdd-and-fuzzing.md`
- ADR-0006: `adr/0006-transaction-sdk-and-dual-submit-routing.md` (Implemented)
- ADR-0007: `adr/0007-always-on-relay-and-shred-cache-mesh.md` (Implemented)
- ADR-0008: `adr/0008-runtime-extension-capability-and-filtered-ingress.md` (Implemented)
- Framework Hooks: `framework-plugin-hooks.md`
- Runtime Extension Hooks: `runtime-extension-hooks.md`
- Runtime Bootstrap Modes: `runtime-bootstrap-modes.md`
- ARD-0001: `ard/0001-project-structure-and-code-goals.md`
- ARD-0002: `ard/0002-testing-strategy-and-quality-gates.md`
- ARD-0003: `ard/0003-slice-dependency-contracts.md`
- ARD-0004: `ard/0004-error-taxonomy-and-failure-handling.md`
- ARD-0005: `ard/0005-type-system-and-newtype-guidelines.md`
- ARD-0006: `ard/0006-performance-and-hot-path-playbook.md`
- ARD-0007: `ard/0007-infrastructure-composition-and-runtime-model.md`
- ARD-0008: `ard/0008-observability-and-operability-standards.md`
- ARD-0009: `ard/0009-adr-process-and-governance.md`
