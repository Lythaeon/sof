# ARD-0007: Infrastructure Composition and Runtime Model

- Version: 1.0
- Date: 2026-02-15
- Applies to: runtime wiring in `src/runtime.rs`, `src/app/runtime.rs`, and infra modules

## Goals

- Reliability: predictable startup, shutdown, and backpressure behavior.
- Efficiency: minimal orchestration overhead.
- Speed: keep coordination out of hot domain loops.
- Maintainability: one clear composition root and lifecycle model.

## Composition rules

- Infra is the only layer that wires slice interactions.
- Domain slices expose contracts; infra provides concrete adapters.
- Runtime concerns remain outside core domain logic.

## Lifecycle model

- Define explicit startup order for sockets/channels/tasks.
- Define shutdown policy for task cancellation and draining.
- Handle backpressure with bounded channels and explicit overflow policy.

## Fault containment

- Task failures must be surfaced and classified.
- Recoverable components may be restarted with bounded strategy.
- Terminal faults must fail fast with actionable diagnostics.

## Configuration

- Configuration is validated at startup.
- Defaults are safe and explicit.
- Invalid configuration fails fast before serving traffic.

## Exit criteria

1. Composition root clearly owns cross-slice orchestration.
2. Lifecycle states are explicit and tested.
3. Backpressure and failure policies are documented and enforced.
