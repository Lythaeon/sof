# SOF

SOF is a runtime foundation for building low-latency Solana services.

It gives you a reusable ingest, reconstruction, control-plane, and plugin/dispatch layer so you do not have to rebuild Solana-facing infrastructure from scratch for every project.

This repository is for teams that want to build things like:

- low-latency transaction observers
- market data and execution services
- strategy engines
- local control-plane and leader-aware routing services
- restart-safe Solana state consumers

## What SOF Actually Is

SOF is not just a collection of hooks.

It is a packaged runtime substrate for Solana applications:

- raw-shred ingest and reconstruction
- processed provider-stream ingest
- local transaction classification and dispatch
- replay-aware derived state
- runtime health, readiness, and degradation handling
- transaction submission and execution adapters

The goal is simple: application code should focus on the Solana logic you care about, while SOF owns the hard low-level work underneath it.

## Plugin And Consumer Contract

SOF's plugin surface is intentionally bounded. The important rules are:

- plugin hook subscriptions are static at startup; SOF does not add or remove plugins at runtime
- borrowed classifier hooks such as `transaction_prefilter`, `accepts_transaction_ref`, and
  `transaction_interest_ref` run on the hot path and should stay cheap
- normal async plugin hooks run off the ingest hot path through bounded dispatch queues
- queue pressure drops plugin events instead of stalling packet ingest
- non-transaction hooks share one bounded general queue
- accepted transactions use separate bounded lanes for inline-critical, critical, and background work
- full queues drop the incoming event; SOF does not evict older queued plugin events to make room
- queue ownership is host-wide per lane, not per plugin
- SOF does not currently guarantee per-plugin fairness under pressure

In other words: overflow is drop-new, not drop-oldest.
- `PluginDispatchMode::Sequential` preserves registration order for one queued event
- `PluginDispatchMode::BoundedConcurrent(n)` trades that strict per-event callback ordering for
  bounded parallelism
- plugins are the observational surface, not the authoritative replay contract; if you need
  restart-safe deterministic state, use derived-state consumers instead

That is deliberate. SOF protects ingest first, then gives you explicit ways to choose where you
want latency, ordering, and replay guarantees.

Queue visibility exists too, but today it is aggregate:

- general plugin queue depth and drop counters
- transaction lane queue depth and drop counters
- dataset and packet-worker queue depth and drop counters

Those metrics let operators detect backpressure, event loss, and degraded provider behavior in real
time. Per-plugin queue pressure is not a first-class metric surface yet.

## Why Build On SOF

Most Solana teams do not fail on business logic first. They burn time on the substrate:

- reconnect logic
- replay gaps
- duplicate suppression
- packet parsing and reconstruction
- provider-specific glue
- queue sizing and multicore fanout
- correctness boundaries under partial failure
- latency regressions from copies, allocations, and cache churn

SOF absorbs that work once and exposes a stable runtime surface.

That gives you:

- one runtime model across raw shreds and processed provider streams
- one transaction commitment selector across gossip, trusted raw shreds, gRPC providers, and websocket providers
- lower-copy, lower-churn hot paths that are already tuned
- explicit trust modes instead of silent verification shortcuts
- bounded degradation, health signals, and typed error surfaces
- replay-aware downstream behavior for stateful consumers
- a cleaner foundation for building Solana services repeatedly

## Why SOF Can Be Better Than Rebuilding It Yourself

SOF is useful because it already spent engineering effort on the runtime details most projects end
up rediscovering:

- lower-copy, lower-allocation hot paths
- borrowed/shared data where the runtime can safely keep it borrowed/shared
- fast paths that avoid work the runtime does not actually need to do
- removal of redundant work that used to survive deeper into the pipeline
- replay dedupe, reconnect, watchdog, and startup validation behavior
- typed health, readiness, and degradation reporting
- one consistent transaction/plugin model across multiple ingress families

That work is cumulative across releases, not isolated to one branch:

- `0.7.x` and `0.8.x` moved SOF toward multi-core ingest, narrower plugin fanout, lower-copy
  packet and dispatch paths, and cheaper dataset reassembly
- `0.12.0` tightened the shred-to-plugin path and improved validated VPS latency from
  `59.978 / 8.007 / 6.415 ms` to `44.929 / 6.593 / 5.370 ms` for
  `first_shred / last_required_shred / ready -> plugin`
- `0.13.0` carried the largest single batch of measured provider/runtime hot-path work, including:
  - provider transaction-kind classification: `34112us -> 4487us` (`~7.6x`)
  - provider transaction dispatch path: `39157us -> 5751us` (`~6.8x`)
  - provider serialized-ignore path: `42422us -> 23760us` (`~44%` faster)
  - websocket full-transaction parse path: `162560us -> 133309us` (`~18%` faster)

Just as important: SOF does not keep changes because they look faster in review. The normal loop is
baseline, change one thing, A/B test it, check `perf` and runtime metrics, then keep the change
only if the data holds. Regressions are rejected rather than rationalized.

The detailed performance history and methodology live in
[Why SOF Exists](docs/gitbook/use-sof/why-sof-exists.md).

## The Main Idea

SOF separates **how data enters the system** from **how your application consumes it**.

You can feed SOF from:

- public gossip or direct peers
- a trusted raw shred provider
- Yellowstone gRPC
- LaserStream
- websocket `transactionSubscribe`
- a custom processed provider producer

And still keep one bounded runtime and one plugin/derived-state model with aligned semantics.

That last clause matters. Switching ingest modes is not just a transport change:

- raw-shred modes can emit the richest local control-plane surface
- built-in processed providers are intentionally transaction-first
- `ProviderStreamMode::Generic` is the escape hatch when a custom producer needs to feed richer
  control-plane updates into SOF

So ingress choice is also a semantic choice.

Transaction-family plugins can choose delivery commitment once at the SOF layer:

```rust
use sof::framework::{PluginConfig, TxCommitmentStatus};

let config = PluginConfig::new()
    .with_transaction()
    .at_commitment(TxCommitmentStatus::Confirmed);
```

If you do not set either selector, SOF defaults to
`at_commitment(TxCommitmentStatus::Processed)`, which means transaction-family
hooks receive `processed`, `confirmed`, and `finalized` events.

## When SOF Is Fastest

SOF removes downstream runtime overhead. It does not magically create upstream visibility.

If you want the lowest end-to-end latency, the main variable is still how early your host sees the data.

SOF gets the most leverage when it sits behind:

- direct useful peers
- validator-adjacent raw shred access
- a trusted private shred propagation network

That is where SOF's local ingest, parsing, classification, and in-process dispatch matter most.

## Ingest Modes

### 1. Raw Shreds

Use SOF as a shred-native observer/runtime.

Raw shred trust modes:

- `public_untrusted`
  - verification on by default
  - highest independence
  - highest observer-side CPU cost
- `trusted_raw_shred_provider`
  - verification off by default
  - meant for trusted private shred feeds
  - lower observer-side CPU cost

`trusted_raw_shred_provider` disables local shred verification by default. Misuse can let invalid
data enter the observer pipeline. It is intentionally not the safe default. Use it only when your
upstream trust boundary is explicit, for example validator-adjacent raw shred distribution or a
private provider you have already decided to trust operationally. If that sentence is not clearly
true for your deployment, stay on `public_untrusted`.

Trusted raw-shred mode still uses the normal SOF downstream path after admission:

1. packet parse/classification
2. optional FEC recovery
3. dataset and transaction reconstruction
4. plugin and extension dispatch

### 2. Built-In Processed Provider Streams

Use SOF as a processed transaction/runtime surface instead of a raw-shred observer.

Built-in adapters today:

- Yellowstone gRPC
- LaserStream gRPC
- websocket `transactionSubscribe`

Built-in processed providers are intentionally transaction-first:

- they emit `on_transaction`
- they do not expose standalone built-in control-plane hooks like `on_leader_schedule` or `on_reorg`

Durability model:

- Yellowstone and LaserStream support explicit replay modes
  - `Live`
  - `Resume`
  - `FromSlot(n)`
  - built-in gRPC adapters fail fast before startup completes and keep the
    first acknowledged session as the live session instead of burning a
    throwaway subscribe/unsubscribe cycle first
- websocket uses reconnect plus best-effort HTTP gap backfill
  - it has no native replay cursor
  - SOF cannot make it stronger than the provider actually is
  - built-in websocket mode also keeps the first acknowledged session as the
    live session, so startup does not create an extra blind handoff window

### 3. Generic Provider Mode

Use `ProviderStreamMode::Generic` when your application already has a custom producer and wants to feed SOF directly.

This is the flexible processed-provider mode:

- custom producers can send richer control-plane updates
- generic mode can power `sof-tx` adapters if it provides the full control-plane surface
- capability mismatch can either warn or fail
- provider replay dedupe is runtime-wide for the active provider ingress before plugin and
  derived-state dispatch
- that dedupe is not per-plugin
- SOF does not run raw-shred and provider-stream ingest together inside one observer runtime, so
  there is no cross-family replay dedupe boundary inside a single running SOF instance

Useful knobs:

- `SOF_PROVIDER_STREAM_CAPABILITY_POLICY=warn|strict`
- `SOF_PROVIDER_STREAM_ALLOW_EOF=true` for bounded replay/batch producers

## What SOF Does For Reliability And Accuracy

SOF treats reliability and accuracy as runtime concerns, not afterthoughts.

That includes:

- explicit verification posture
- replay dedupe
- semantic duplicate/conflict suppression
- typed provider health
- runtime readiness and degraded-state reporting
- startup validation for built-in provider adapters
- replay-aware derived-state feeds
- fail-fast capability checks where semantics cannot be met

The rule is: if a mode cannot honestly provide a surface, SOF should either make that limitation explicit or reject the configuration.

## Scheduling And Runtime Shape

SOF already has an explicit scheduling model, even though it is not yet a full NUMA-aware runtime:

- raw-shred ingest fans work out into packet workers and dataset workers
- provider modes run their own supervised source sessions and then enter the same downstream host
  surface where semantics line up
- plugin dispatch is explicitly queued and bounded
- selected runtime threads can be pinned with host/runtime controls

What SOF does not claim yet:

- a first-class NUMA scheduler
- automatic topology-aware pin placement for every host
- universal “best” worker geometry across all CPUs and providers

For latency-sensitive deployments, thread placement and host topology still need measurement.

Current operator playbook:

- single-socket VPS or public host: start with the validated `sof-gossip-tuning` VPS profile
- processed provider mode: leave packet/shred/FEC knobs alone first and validate provider durability and health before tuning workers
- trusted raw-shred mode: keep NIC-facing ingest, packet workers, and dataset workers on the same socket when possible
- multi-socket hosts: prefer one socket first unless measurement proves the extra cross-socket fanout is worth it

## The Workspace

This repository contains three main crates:

- `sof`
  - the observer/runtime crate
  - raw-shred ingest, provider-stream ingest, derived state, plugins, extensions, runtime operations
- `sof-tx`
  - the transaction SDK
  - building, signing, and submitting Solana transactions
  - works standalone for RPC, Jito, and signed-byte flows
  - integrates with SOF control-plane/runtime adapters
- `sof-gossip-tuning`
  - typed tuning presets for SOF gossip and ingest deployment

## What You Can Build With It

- passive or active Solana observers
- local execution engines
- “observe and submit” services
- market data systems
- strategy backends
- replay-safe stateful services
- validator-adjacent data services

## Quick Start

Install the crates you need:

```bash
cargo add sof
cargo add sof-tx
cargo add sof-gossip-tuning
```

Useful feature sets:

```toml
sof = { version = "0.15.0", features = ["gossip-bootstrap"] }
sof = { version = "0.15.0", features = ["provider-grpc"] }
sof = { version = "0.15.0", features = ["provider-websocket"] }
sof-tx = { version = "0.15.0", features = ["sof-adapters"] }
```

Run the basic observer example:

```bash
cargo run --release -p sof --example observer_runtime
```

Run with gossip bootstrap:

```bash
cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

Provider examples:

- Yellowstone: `crates/sof-observer/examples/provider_stream_yellowstone_grpc.rs`
- LaserStream: `crates/sof-observer/examples/provider_stream_laserstream.rs`
- websocket: `crates/sof-observer/examples/provider_stream_websocket_transaction.rs`
- trusted raw shred provider: `crates/sof-observer/examples/trusted_raw_shred_provider.rs`

## A Practical Mental Model

Use SOF when you want:

- one optimized runtime foundation
- one plugin/consumer model
- explicit correctness and trust boundaries
- less time spent rebuilding Solana-specific infrastructure

Do not use SOF only because you want a thin wrapper around a provider API.

The value is in the runtime substrate:

- ingest
- dispatch
- replay
- control-plane state
- error handling
- health
- performance work you do not want to reimplement per application

## Documentation

- crate docs: [`crates/sof-observer/README.md`](crates/sof-observer/README.md)
- transaction SDK: [`crates/sof-tx/README.md`](crates/sof-tx/README.md)
- deployment model: [`docs/gitbook/operations/deployment-modes.md`](docs/gitbook/operations/deployment-modes.md)
- why SOF exists: [`docs/gitbook/use-sof/why-sof-exists.md`](docs/gitbook/use-sof/why-sof-exists.md)
- comparison docs: [`docs/gitbook/use-sof/sof-compared.md`](docs/gitbook/use-sof/sof-compared.md)

## Repository Layout

- `crates/sof-observer`
- `crates/sof-tx`
- `crates/sof-gossip-tuning`
- `docs/`
- `scripts/`

## Contributor Gate

SOF uses `cargo-make` for the main contributor gate.

Install it if needed:

```bash
cargo install cargo-make
```

Run the gate:

```bash
cargo make ci
```
