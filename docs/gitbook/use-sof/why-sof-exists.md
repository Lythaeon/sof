# Why SOF Exists

SOF exists because many Solana services need more control than RPC gives them, but less weight than
running a full validator-shaped stack.

That does not mean SOF is always the fastest answer.

The important distinction is:

- SOF is a runtime foundation
- ingress still determines how early the host sees traffic

If you put SOF behind a late source, it starts from late data. If you put SOF behind an early
source, it can keep parsing, local state, filtering, replay, and downstream logic on the same box
with less custom glue.

## The Real Problem

Teams usually hit one of these problems:

- RPC is too far from the traffic source
- managed providers define the stream shape and operational boundary for you
- custom ingest stacks are tedious to build correctly
- every service ends up reimplementing reconnect, replay, dedupe, filtering, health, and queueing

SOF exists to stop that repetition.

## What SOF Is Good At

SOF is good at owning the local runtime boundary:

- ingest
- parsing or provider adaptation
- replay and dedupe boundaries
- plugin and derived-state dispatch
- health, readiness, and metrics
- local control-plane derivation where the ingress mode supports it

That lets teams focus on the service they actually want to build.

## What SOF Is Not

SOF is not:

- a guarantee of best possible latency
- a validator
- a wallet framework
- the only sensible way to build Solana infrastructure

If you want the earliest possible raw data, the better answer is often:

- private shred distribution
- direct validator-adjacent ingress
- better host placement

SOF becomes valuable there because it gives those setups one bounded runtime instead of another
custom stack.

## Trust Posture Matters

SOF does not treat all ingress as equally trusted.

For raw shreds there are two distinct postures:

- `public_untrusted`
  - verification on by default
  - strongest independence
  - highest observer CPU cost
- `trusted_raw_shred_provider`
  - verification off by default
  - intended for private, trusted raw feeds
  - misuse can admit invalid data

Processed providers such as Yellowstone, LaserStream, or websocket transaction feeds are a separate
category. They are useful, but they are not raw-shred trust modes.

## Why Teams Use SOF Anyway

Because even when the ingress question is settled, the runtime work still remains:

- provider-specific reconnect and replay handling
- queue boundaries and overload behavior
- hot-path copy and allocation control
- removing redundant work that only burns CPU
- introducing fast paths that avoid deeper work when the answer is already known
- consistent hooks and filters across modes
- health, readiness, and metrics
- local control-plane and derived-state plumbing

SOF is the reusable layer for that work.

## How SOF Gets Fast

SOF is not a pile of "probably faster" changes. The normal performance loop is:

- form a concrete bottleneck hypothesis
- capture a baseline
- change one thing
- re-run A/B validation with release fixtures
- check `perf` and runtime metrics
- keep the change only if the data holds

That matters because several candidate optimizations were explicitly measured and then reverted when
they did not produce a stable win. The goal is to remove repeated guesswork, not just repeated
code.

## This Is A Multi-Release Story

The performance shape of SOF came from several release lines, not just one.

### `0.7.x` and `0.8.x`

These releases moved SOF away from a mostly serial ingest shape and toward:

- multi-core packet-worker ingest
- lower-copy dataset and transaction handling
- narrower plugin fanout
- cheaper dispatch and scratch-buffer reuse
- better dataset reassembly locality

That is where a lot of the basic "SOF is not burning obvious CPU for no reason" work came from.

### `0.12.0`

`0.12.0` tightened the shred-to-plugin path and made the inline transaction path explicit,
measurable, and easier to profile.

Validated VPS latency improved from:

- `59.978 / 8.007 / 6.415 ms`

to:

- `44.929 / 6.593 / 5.370 ms`

for:

- `first_shred / last_required_shred / ready -> plugin`

That release also added the benchmark and profiling surface used to keep later work honest:

- hot-path benchmark harnesses
- standalone profiling binaries
- symbolized `perf` investigation on live VPS traffic

It also carried measured dispatch/stateful-fanout improvements instead of only latency work. One
of the derived-state profiling slices improved from:

- `12424.649 ns/iter`

to:

- `8603.920 ns/iter`

on the final validated branch state.

### `0.13.0`

`0.13.0` carried the largest single concentration of measured provider/runtime hot-path work so far.

Representative validated fixture results on that line:

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
ingest wins.

## What The Optimizations Usually Look Like

The wins above did not come from one trick. They came from repeated patterns:

- removing redundant work from hot paths
- adding fast paths so ignored or low-value traffic dies earlier
- using borrowed/shared views where full owned materialization is not needed yet
- cutting copies and allocations
- reducing instruction count for the same work
- reducing branching and, where the data showed it, improving cache behavior
- narrowing plugin fanout and dispatch overhead instead of rescanning everything

Examples of that pattern across releases:

- borrowed transaction classification
- compiled transaction prefilters
- skipping owned decode on prefiltered misses
- skipping completed-dataset tx decode when the prefilter already proves nothing will consume it
- reducing transaction dispatch handoff overhead
- reducing generic plugin dispatch overhead

## What SOF Is Actually Claiming

The claim is not:

- SOF is automatically the fastest way to see Solana data

The claim is:

- SOF removes a large amount of local runtime waste
- SOF gives you one reusable runtime instead of rebuilding ingest and dispatch every time
- SOF keeps measured wins and rejects regressions
- SOF makes the runtime behavior more explicit and more observable while staying fast on the
  validated hot paths

So the value is not just "speed." It is speed plus correctness boundaries plus operational clarity.

## The Short Version

SOF exists so teams can own their Solana runtime boundary without rebuilding the same ingest,
correctness, and operations machinery over and over again.
