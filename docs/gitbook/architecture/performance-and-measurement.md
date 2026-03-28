# Performance and Measurement

This is the one GitBook page that carries SOF's detailed performance story.

Use it for:

- how SOF performance work is actually done
- what kinds of optimizations have accumulated across releases
- which measured wins are representative
- what SOF is and is not claiming when it says it is optimized

## The Method

SOF is not tuned by intuition alone.

The normal loop is:

1. form a concrete bottleneck hypothesis
2. capture a baseline
3. change one thing
4. re-run A/B validation with release fixtures
5. check `perf` and runtime metrics
6. keep the change only if the data holds

That matters because some candidate optimizations were explicitly measured and then reverted when
they did not produce a stable win. Regressions are rejected rather than rationalized.

## This Is A Multi-Release Story

SOF performance did not appear all at once in `0.13.0`.

### `0.7.x` and `0.8.x`

These releases moved SOF away from a mostly serial ingest shape and toward:

- multi-core packet-worker ingest
- lower-copy dataset and transaction handling
- narrower plugin fanout
- cheaper dispatch and scratch-buffer reuse
- better dataset reassembly locality

This is where much of the basic runtime-efficiency foundation came from.

### `0.12.0`

`0.12.0` tightened the shred-to-plugin path and made inline transaction visibility explicit,
measurable, and easier to profile.

Validated VPS latency on the `0.12.0` line improved from:

- `59.978 / 8.007 / 6.415 ms`

to:

- `44.929 / 6.593 / 5.370 ms`

for:

- `first_shred / last_required_shred / ready -> plugin`

That release also added:

- hot-path benchmark harnesses
- standalone profiling binaries
- symbolized `perf` investigation on live VPS traffic

It also carried measured dispatch/stateful-fanout improvements. One derived-state profiling slice
improved from:

- `12424.649 ns/iter`

to:

- `8603.920 ns/iter`

on the final validated branch state.

### `0.13.0`

`0.13.0` carried the largest single concentration of measured provider/runtime hot-path work so
far.

Representative validated fixture results on the `0.13.0` line:

- provider transaction-kind classification:
  - `34112us -> 4487us`
  - about `7.6x` faster
- provider transaction dispatch path:
  - `39157us -> 5751us`
  - about `6.8x` faster
- provider serialized-ignore path:
  - `42422us -> 23760us`
  - about `44%` faster
- websocket full-transaction parse path:
  - `162560us -> 133309us`
  - about `18%` faster

That line also matters because it combined hardening with performance work. Replay, health,
capability, and observability improvements were kept without giving back the main provider/runtime
ingest wins on the validated `0.13.0` release branch.

## What The Optimizations Usually Look Like

The gains above did not come from one trick. They came from repeated patterns:

- removing redundant work from hot paths
- adding fast paths so ignored or low-value traffic dies earlier
- using borrowed/shared views where full owned materialization is not needed yet
- cutting copies and allocations
- reducing instruction count for the same work
- reducing branching and, where the data showed it, improving cache behavior
- narrowing plugin fanout and dispatch overhead instead of rescanning everything

Representative examples across releases:

- borrowed transaction classification
- compiled transaction prefilters
- skipping owned decode on prefiltered misses
- skipping completed-dataset tx decode when the prefilter already proves nothing will consume it
- reducing transaction dispatch handoff overhead
- reducing generic plugin dispatch overhead
- trimming provider transaction overhead and avoiding serialized payload copies

## What SOF Is Actually Claiming

The claim is not:

- SOF is automatically the fastest way to see Solana data

The claim is:

- SOF removes a large amount of local runtime waste
- SOF gives you one reusable runtime instead of rebuilding ingest and dispatch every time
- SOF keeps measured wins and rejects regressions
- SOF makes runtime behavior more explicit and more observable while staying fast on the validated
  hot paths

That claim is intentionally scoped:

- the measurements above are historical validated release-line results, live checks, and profiled
  slices
- they are not a promise that every later branch will reproduce the exact same absolute numbers
- they are not a claim about every mixed workload or every host topology
- ingress still determines how early a host sees the data

If the earliest possible visibility is the goal, private raw distribution, validator-adjacent
ingress, and host placement still matter more than any local runtime optimization.
