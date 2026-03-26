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
- lower-copy, lower-churn hot paths that are already tuned
- explicit trust modes instead of silent verification shortcuts
- bounded degradation, health signals, and typed error surfaces
- replay-aware downstream behavior for stateful consumers
- a cleaner foundation for building Solana services repeatedly

## Why SOF Can Be Better Than Rebuilding It Yourself

SOF is useful because it already spent the engineering effort on the details most projects eventually trip over:

- fewer avoidable instructions in hot paths
- fewer avoidable allocations and copies
- borrowed/shared data where the runtime can safely keep it borrowed/shared
- SIMD-based parsing where it is a real win
- replay dedupe and semantic duplicate suppression
- provider reconnect, replay, watchdog, and startup validation behavior
- typed health, readiness, and degradation reporting
- a consistent transaction/plugin model across multiple ingress families

If you build your own Solana runtime stack from scratch for every service, you end up paying that optimization and correctness tax every time.

## The Main Idea

SOF separates **how data enters the system** from **how your application consumes it**.

You can feed SOF from:

- public gossip or direct peers
- a trusted raw shred provider
- Yellowstone gRPC
- LaserStream
- websocket `transactionSubscribe`
- a custom processed provider producer

And still keep the same local runtime/plugin surface where the semantics line up.

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
- websocket uses reconnect plus best-effort HTTP gap backfill
  - it has no native replay cursor
  - SOF cannot make it stronger than the provider actually is

### 3. Generic Provider Mode

Use `ProviderStreamMode::Generic` when your application already has a custom producer and wants to feed SOF directly.

This is the flexible processed-provider mode:

- custom producers can send richer control-plane updates
- generic mode can power `sof-tx` adapters if it provides the full control-plane surface
- capability mismatch can either warn or fail

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

## The Workspace

This repository contains three main crates:

- `sof`
  - the observer/runtime crate
  - raw-shred ingest, provider-stream ingest, derived state, plugins, extensions, runtime operations
- `sof-tx`
  - the transaction SDK
  - building, signing, and submitting Solana transactions
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
sof = { version = "0.12.0", features = ["gossip-bootstrap"] }
sof = { version = "0.12.0", features = ["provider-grpc"] }
sof = { version = "0.12.0", features = ["provider-websocket"] }
sof-tx = { version = "0.12.0", features = ["sof-adapters"] }
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
