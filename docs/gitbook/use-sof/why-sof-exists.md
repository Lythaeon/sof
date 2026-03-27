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

That includes the performance method itself. SOF is not meant to be a pile of "probably faster"
changes. The runtime is tuned by:

- forming explicit bottleneck hypotheses
- testing them with A/B runs
- using `perf`, fixture benchmarks, and runtime metrics to decide what stays

The goal is to remove repeated guesswork, not just repeated code.

That tuning has happened across multiple releases. `0.13.0` carried the largest single batch of
measured runtime/provider improvements so far, including:

- redundant work removed from hot paths
- new fast paths that stop unnecessary work earlier
- fewer copies and allocations in validated provider/runtime paths
- lower instruction count for the same work on several measured paths
- reduced branching and, where the data showed it, better cache behavior

## The Short Version

SOF exists so teams can own their Solana runtime boundary without rebuilding the same ingest,
correctness, and operations machinery over and over again.
