# Contributing

This repository expects contributors to preserve architecture clarity, hot-path discipline, and
operational readability.

## Standard Workflow

1. make a focused change in the owning slice
2. add or update tests with the behavior change
3. run the local quality gates
4. update docs when behavior, configuration, or operations changed

## Architecture Rules

- keep slices isolated
- keep `mod.rs` files to declaration and re-export duties
- use typed errors and validated value objects instead of stringly categories
- keep orchestration in app and infra layers, not in leaf slices

Architecture boundary enforcement:

```bash
cargo make arch-check
```

## ADR Discipline

Add an ADR when you change:

- slice boundaries or dependency direction
- orchestration patterns
- dispatch strategy
- type-system policy
- meaningful latency or throughput tradeoffs

Do not edit accepted ADRs in place to change history. Add a new ADR that supersedes the old one.

## Pull Request Expectations

Good PRs in this repository include:

- a clear description of what changed and why
- scope and impact notes
- testing evidence
- documentation updates when relevant
- ADR references when architecture changed

## Commit Style

Supported commit prefixes include:

- `docs`
- `feat`
- `fix`
- `refactor`
- `test`
- `chore`
- `perf`

Keep commit subjects short, imperative, and specific.
