# ARD-0010: Simplification Without Capability Regression

- Version: 1.0
- Date: 2026-04-11
- Applies to: `sof` runtime APIs, workspace crate layout, configuration surface, and runtime-facing docs

## Goals

- Reliability: simplification must not weaken correctness, replay, or backpressure guarantees.
- Efficiency: reduce code and API friction without adding runtime churn or hidden work.
- Speed: preserve or improve hot-path latency and throughput.
- Maintainability: make module boundaries, naming, and config intent easier to understand.

## Problem statement

SOF is intentionally infrastructure-shaped. Some of that complexity is real:

- raw shred ingest
- processed provider-stream ingest
- bounded multicore dispatch
- queue pressure and degradation handling
- replay-aware derived-state
- health and readiness semantics
- multiple trust and delivery postures

But not all current complexity is equally valuable.

SOF should become easier to read, configure, and embed without pretending the runtime itself is
simple. The requirement is therefore:

- remove accidental complexity
- preserve necessary complexity
- never trade away measured performance or documented semantics for aesthetic cleanup

## Simplification principles

- Simplify public shape first.
- Keep hot paths explicit.
- Prefer typed policies over many loosely-related knobs.
- Prefer literal names over generic helpers.
- Prefer narrower composition roots over cross-cutting wiring spread across many files.
- Preserve semantic distinctions that protect correctness.

## Required distinction: real complexity vs accidental complexity

The following are considered real complexity and MUST NOT be abstracted away beyond recognition:

- ingress ownership and bounded admission
- packet/shred/FEC/dataset reconstruction stages
- replay-safe derived-state boundaries
- queue ownership, drop policy, and ordering semantics
- trust posture and verification choices
- provider capability differences
- shutdown and drain behavior

The following are considered accidental complexity and SHOULD be reduced aggressively:

- oversized files with unrelated concerns
- vague module names
- duplicated env/config parsing patterns
- stringly configuration where typed policy is possible
- redundant builder methods with unclear precedence
- multiple parallel ways to express the same runtime choice
- examples that require too much substrate knowledge for common paths

## Allowed simplification targets

### 1. Module and file layout

- Split oversized files by explicit concern.
- Make `lib.rs` and top-level `mod.rs` files routing surfaces, not implementation dumps.
- Group runtime code by stable responsibility instead of by historical growth.
- Prefer file names that describe one concept directly.

Examples:

- support helpers split into named modules
- runtime config parsing isolated from runtime execution
- downstream dispatch policy separated from provider-specific startup wiring

### 2. Public API shape

- Add narrower builders and typed config bundles for common setups.
- Keep advanced escape hatches available behind explicit low-level APIs.
- Reduce duplicate entrypoints that express the same behavior in slightly different forms.
- Standardize naming across Rust, TypeScript, and Python where the same concept exists.

### 3. Configuration surface

- Collapse knob clusters into typed policy enums or typed config structs when the knobs represent
  one operating posture.
- Reject unknown config values explicitly.
- Keep defaults safe, explicit, and documented.
- Preserve advanced overrides only when they map to real verified workload needs.

### 4. Documentation and examples

- Provide one obvious path for common use cases.
- Keep advanced operational detail in separate focused docs.
- State guarantees and non-guarantees explicitly.
- Show runtime choice boundaries instead of implying that all modes are equivalent.

## Forbidden simplification patterns

The following are architecture regressions even if they reduce line count:

- replacing typed runtime contracts with unstructured maps or boolean bags
- collapsing observational plugin delivery and replay-safe derived-state into one surface
- hiding queue drop behavior, ordering rules, or trust posture behind vague “smart defaults”
- introducing unbounded queues to make APIs feel simpler
- moving hot-path behavior behind generic trait object stacks or allocation-heavy adapters without
  measurement proving no regression
- merging provider families in a way that erases real capability differences
- removing explicit lifecycle states, drain policy, or startup validation in the name of ergonomics

## Safe simplification patterns

The following are preferred:

- policy bundling:
  - example: one typed runtime delivery profile instead of many downstream queue toggles
- facade layering:
  - simple API for common paths, explicit lower-level API for advanced paths
- module extraction:
  - move coherent utility or policy code into named files without changing semantics
- typed parsing:
  - parse env/file values into typed enums and structs early, fail fast on invalid values
- composition-root cleanup:
  - keep orchestration in infra/app runtime layers, not spread through domain slices
- benchmark-backed refactor:
  - refactor internals only with before/after latency and throughput evidence on affected paths

## Performance guardrails

Simplification work MUST follow these rules:

- hot-path changes require benchmark or profile evidence
- abstraction added to hot paths must justify itself with measured neutrality or benefit
- allocations, copies, queue hops, and dynamic dispatch added to hot paths are suspect by default
- code motion that is semantics-preserving but performance-neutral is allowed
- code motion that improves readability but regresses critical paths is rejected

Relevant hot paths include at minimum:

- packet ingest
- shred parsing and verification
- FEC recovery and dataset reconstruction
- transaction extraction and classification
- inline-critical plugin delivery
- provider-stream transaction ingest

## Stability and semantics guardrails

Simplification work MUST preserve:

- bounded ingress and bounded downstream memory growth
- explicit overflow behavior
- typed failure surfaces where they already exist
- trust-mode semantics
- provider capability validation
- replay and checkpoint meaning for derived-state
- runtime health, readiness, and degradation signaling

If a simplification changes any of those semantics, it is not a refactor. It is an architecture
change and requires a separate ADR.

## Preferred migration strategy

Simplification should usually proceed in this order:

1. document current semantics before changing structure
2. split files and isolate concerns without behavior changes
3. add typed policy/config facades above existing behavior
4. deprecate duplicate or confusing surfaces
5. only then evaluate deeper runtime-policy consolidation

This order matters because it keeps verification local and reduces accidental semantic drift.

## Acceptance criteria for simplification work

A simplification change is acceptable only if all of the following are true:

1. feature surface is preserved or intentionally superseded with clear migration
2. public semantics are at least as explicit as before
3. critical-path performance is preserved or improved on affected paths
4. failure, replay, and backpressure behavior remain verified
5. code layout or API clarity is materially better, not merely different

## Required verification

Depending on scope, simplification work should include:

- unit tests for moved logic
- regression tests for config parsing and precedence
- integration tests for startup, shutdown, and degraded-runtime behavior
- perf or benchmark comparisons for affected hot paths
- fuzz coverage where parser or wire-surface logic changes
- doc updates for any user-visible naming or configuration changes

## Heuristics for deciding whether to simplify

Simplify when:

- one concept appears in too many places
- multiple APIs express one runtime choice
- file size hides coherent responsibilities
- users can make semantically invalid combinations too easily
- docs need too much prose to explain one ordinary path

Do not simplify when:

- the complexity is enforcing a real runtime boundary
- the complexity is carrying a measured performance optimization
- the complexity exists to make failure modes explicit
- the “simpler” version would blur capability differences or correctness contracts

## Exit criteria

1. Simplification proposals state what accidental complexity is being removed and what real
   complexity remains intentionally explicit.
2. Hot-path refactors include performance verification.
3. Public API or config simplification preserves typed semantics and explicit failure behavior.
4. Module/layout cleanup leaves code easier to navigate with no hidden behavioral changes.
