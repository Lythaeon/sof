# ARD-0006: Performance and Hot-Path Playbook

- Version: 1.0
- Date: 2026-02-15
- Applies to: performance-sensitive paths in `sof-observer`

## Goals

- Reliability: stable latency/throughput under expected load.
- Efficiency: lower CPU/cache/memory overhead.
- Speed: maximize packet parse and reassembly throughput.
- Maintainability: measurable, reviewable optimization choices.

## Principles

- Measure first; optimize based on profiling data.
- Prefer static dispatch in hot paths.
- Keep hot functions small and predictable.

## Hot/cold separation

- Keep formatting-heavy and rare error branches in cold paths.
- Keep hot data compact and locality-friendly.
- Avoid unnecessary allocations/copies in tight loops.

## Inlining policy

- Use inlining intentionally for tiny hot helpers when measurements justify.
- Avoid blanket inlining that harms code size and instruction cache behavior.

## Data movement policy

- Prefer borrowing over cloning where safe.
- Minimize buffer churn and transient allocations.
- Use bounded capacities for maps/queues in hot pipelines.

## Verification

- Changes to hot paths include benchmark/profiling evidence.
- Regressions in throughput/latency block merge until resolved or accepted via ADR.
- Use the `crates/sof-observer/benches/hot_paths.rs` Criterion harness for SOF-owned relay, dataset-reassembly, and derived-state dispatch checks before inventing one-off microbenches.
- Keep hot-path benches workload-shaped and bounded: prefer representative batch sizes and queue/cache sizes over synthetic "infinite loop" measurements that hide coordination costs.
- When comparing hosts, record the host class explicitly (for example laptop vs VPS) because scheduler and networking topology materially affect multicore runtime behavior.

## Exit criteria

1. Optimization decisions reference measurement evidence.
2. Hot paths avoid dynamic dispatch unless justified.
3. Memory and CPU costs stay within agreed budgets.
