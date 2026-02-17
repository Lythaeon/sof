# Contributing to SOF

This guide defines how to contribute changes to `sof` while keeping architecture, reliability, and release standards intact.

## Scope

This repository is a Rust workspace with the main crate at `crates/sof-observer` (published as `sof`).

Primary references:

- Project overview and run commands: `README.md`
- Architecture index: `docs/architecture/README.md`
- Operations docs index: `docs/operations/README.md`

## Prerequisites

- Rust stable toolchain
- `cargo-make` (used by contributor quality gates)

Quick smoke check:

```bash
cargo check -p sof
```

## Development workflow

1. Make focused changes in the owning slice/module.
2. Add or update tests with the behavior change.
3. Run contributor quality gates locally.
4. Open a PR with a clear summary, risk notes, and test evidence.

## Git commits and pull requests

### Philosophy

- Commits are brief and atomic: one logical change, easy to scan.
- Pull requests carry the full context: why, scope, testing, and impact.

### Commit message format

Use one of:

- `<type>: <subject>`
- `<type>(<scope>): <subject>` (optional scope, e.g. `app`, `repair`, `ci`, `docs`)

Examples:

- `feat: add repair peer ranking telemetry`
- `fix: prevent duplicate repair request reservation`
- `chore(ci): tighten release preflight checks`
- `docs(operations): clarify SOF_TVU_SOCKETS defaults`
- `refactor(app): extract gossip switch guard logic`

Supported types:

- `docs` documentation
- `feat` new feature
- `fix` bug fix
- `refactor` code restructuring
- `test` test changes
- `chore` build, CI, dependencies, tooling
- `perf` performance changes

Subject rules:

- Imperative mood (`add`, `fix`, `refactor`)
- Lowercase start (except proper nouns)
- No trailing period
- Aim for <= 50 characters
- Be specific, not generic

Key rule:

- Keep commit titles short and scannable.
- Put detailed rationale and impact in the PR description, not in commit titles.

### Pull request title format

Use:

- `<type>: <subject> — <impact or scope>`
- Optional scope is allowed: `<type>(<scope>): <subject> — <impact or scope>`

Examples:

- `feat(app): add gossip runtime switch guardrails — reduces false-positive failovers`
- `fix(repair): avoid stale outstanding reservation reuse — improves recovery reliability`
- `docs: clarify advanced env defaults — reduces operator misconfiguration`

### Pull request description requirements

PRs must include:

- Description: what changed and why it matters.
- Changes: concrete list of file/module changes.
- Motivation: business and technical reasons; alternatives considered if relevant.
- Scope and impact: affected slices, compatibility, operational impact.
- Testing: what was run and what scenarios were validated.
- Related docs/issues: ADR/ARD links, operational doc updates, issue references.

For cross-slice or architecture-impacting changes:

- Explicitly list affected slices (`ingest`, `shred`, `reassembly`, `app/runtime`).
- Explain cross-slice interaction changes.
- Link relevant ADR/ARD docs.
- Include migration/rollback notes if applicable.

Reference template:

- `.github/pull_request_template.md`

## Required local checks before PR

Run:

```bash
cargo make ci
```

This includes:

- formatting check
- architecture boundary checks
- clippy matrix (`all-features`, `no-default-features`)
- test matrix (default + all-features)

For dependency-policy checks too:

```bash
cargo make ci-full
```

## Architecture and code rules

Follow the current ARD/ADR constraints:

- Keep slices isolated: `ingest`, `shred`, `reassembly` do not import each other internals.
- Cross-slice orchestration belongs in infra/app/runtime composition layers.
- Keep `mod.rs` files declaration/re-export only.
- Prefer type-driven modeling (newtypes and validated constructors for invariants).
- Use typed enum errors (`thiserror`) instead of stringly error categories.
- Replace semantic magic numbers with documented named constants.

Enforcement:

- `cargo make arch-check` validates slice boundaries and `mod.rs` policy.

Reference docs:

- `docs/architecture/ard/0001-project-structure-and-code-goals.md`
- `docs/architecture/ard/0003-slice-dependency-contracts.md`
- `docs/architecture/ard/0004-error-taxonomy-and-failure-handling.md`
- `docs/architecture/ard/0005-type-system-and-newtype-guidelines.md`
- `docs/architecture/ard/0007-infrastructure-composition-and-runtime-model.md`

## Testing expectations

- For behavior changes, update or add tests that fail before the fix and pass after.
- Every bug fix should include a regression test.
- Keep tests deterministic and fast.
- If you touch parser/reassembly invariants, expand edge-case coverage (including fuzz-oriented cases where applicable).

Reference:

- `docs/architecture/ard/0002-testing-strategy-and-quality-gates.md`

## Observability and operability requirements

For operationally significant changes:

- Add/update structured logs, metrics, and traces with stable fields.
- Keep telemetry lightweight on hot paths.
- Update operational docs/runbooks when behavior or config changes.

References:

- `docs/architecture/ard/0008-observability-and-operability-standards.md`
- `docs/operations/README.md`
- `docs/operations/advanced-env.md`

## ADR requirements for architecture-impacting changes

Add an ADR when changing architecture-level constraints, including:

- slice boundaries/dependency direction
- infra orchestration patterns
- error model/type-system/dispatch strategy
- material latency/throughput tradeoffs

Reference:

- `docs/architecture/ard/0009-adr-process-and-governance.md`

## CI and release notes

- PRs and pushes to `main` run CI via `.github/workflows/ci.yml`.
- Release checks and publish are handled by `.github/workflows/release-crates.yml`.
- Manual release preflight is available via `workflow_dispatch` (runs checks, does not publish).

## PR checklist

Before requesting review, confirm:

- [ ] `cargo make ci` passes locally
- [ ] tests cover new behavior and regressions
- [ ] architecture boundaries are preserved
- [ ] docs are updated if behavior/config/operations changed
- [ ] ADR added/updated when required
