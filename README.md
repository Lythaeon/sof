# SOF

SOF is a Solana-focused Rust workspace for low-latency data ingest and transaction execution.

It is built more like financial systems infrastructure than a typical crypto framework:
bounded pipelines, local control-plane state, restart-safe derived-state feeds, and execution
paths designed for services that care about latency, replay, and operational discipline.

SOF removes external API and service-layer overhead. It does not remove the upstream visibility
problem by itself. End-to-end latency still depends first on how quickly your host sees shreds.
If you want SOF to be meaningfully faster than RPC-first or provider-stream architectures, give it
the earliest ingress you can:

- direct low-latency access to validators or other useful peers
- or an external shred propagation network feeding the host

That is when SOF's lower-overhead local ingest, parsing, control-plane derivation, and in-process
consumer model matter most: the data reaches your application on the same system without another
heavy external dependency in the hot path.

## Why Build On SOF Instead Of Starting From Scratch

Most Solana teams do not want to spend their engineering budget rebuilding the same low-level
runtime machinery for every new service.

The hard part is usually not the business logic. It is everything underneath it:

- provider-specific ingest and reconnect behavior
- packet parsing, duplicate suppression, verification, and reconstruction
- reducing instructions, copies, allocator churn, and cache misses in the hot path
- keeping hooks and filters consistent across raw-shred and provider-stream modes
- bounded degradation, health/readiness signals, restart behavior, and runtime observability

SOF exists to absorb that cost once and make it reusable.

That means a Solana team can build on a runtime already optimized for:

- fewer unnecessary instructions in hot paths
- fewer avoidable allocations and copies
- shared or borrowed data where it is safe to do so
- SIMD-friendly parsing where it actually helps
- consistent hook semantics across multiple ingress/provider modes
- correctness boundaries such as semantic dedupe, explicit verification posture, and replay-safe
  downstream behavior

The practical value is simple: application developers can focus on their Solana program or service
logic while SOF owns the low-level ingest/runtime discipline.

SOF also treats robustness and accuracy as first-class runtime concerns. Duplicate/conflict
suppression, verification posture, replayable derived state, and bounded runtime behavior are part
of the framework itself instead of ad hoc application glue.

## Trust And Ingress Modes

SOF has two explicit raw-shred trust modes:

- `public_untrusted`: public gossip or other public peers, verification on by default, highest
  independence, highest observer-side CPU cost
- `trusted_raw_shred_provider`: raw shred distribution from a provider you explicitly trust,
  verification off by default, earlier ingress and lower observer-side CPU cost

`processed_provider_stream` products such as Yellowstone gRPC, LaserStream, or websocket feeds are
useful, but they are a different ingest category. They are not `SOF_SHRED_TRUST_MODE` values
because they do not hand SOF raw shreds.

SOF exposes that ingest family explicitly through `ProviderStreamMode`. Use it when you want
Yellowstone/LaserStream-style provider feeds to enter the SOF plugin runtime directly without
pretending they are raw shreds.

Implemented provider-stream adapters today:

- Yellowstone gRPC
- LaserStream gRPC
- websocket `transactionSubscribe`

Built-in hook surface by provider mode:

- Yellowstone gRPC: `on_transaction`
- LaserStream gRPC: `on_transaction`
- websocket `transactionSubscribe`: `on_transaction`

Provider config defaults are inclusive:

- vote transactions are included unless you explicitly set a vote filter
- failed transactions are included unless you explicitly set a failed filter

Provider runtime capability policy applies only to hooks that are impossible in
provider-stream mode. It does not reject generic provider updates such as
`on_recent_blockhash`, `on_slot_status`, or `on_cluster_topology` when a custom
producer pushes those updates into the provider queue.

That also means SOF's internal transaction classifier hooks such as
`transaction_prefilter`, `accepts_transaction_ref`, and `transaction_interest_ref`
apply to all three built-in provider transaction adapters because they all
materialize full transactions before dispatch.

The intended positioning is straightforward:

- use public gossip/direct peers when you want independence and are willing to own the whole stack
- use a trusted raw shred network when you want SOF's fastest practical raw-shred path
- use processed provider streams when you do not need the raw-shred SOF model

The trust tradeoff should be explicit: a trusted raw shred provider replaces some local
verification and public-edge independence with upstream trust in exchange for earlier ingress and
much lower CPU waste than public multi-source gossip. See
[`docs/gitbook/operations/deployment-modes.md`](docs/gitbook/operations/deployment-modes.md) for
the full deployment matrix.

For gossip-based analysis workflows, SOF can also pin runtime selection to the configured
entrypoint set with `SOF_GOSSIP_ENTRYPOINT_PINNED=true`. In that mode, runtime switching stays
inside the user-supplied entrypoints instead of expanding to discovered peer candidates.

In trusted raw shred mode, SOF still runs its normal downstream observer path after packets are
admitted: parse, FEC/reassembly, dataset reconstruction, and plugin dispatch. The default change
is the verification posture, not a separate plugin/runtime surface.

It is split into three user-facing crates:

- `sof`: observer/runtime crate for shred ingest, relay/cache, dataset reconstruction, plugin and runtime-extension events, fork/reorg tracking, and local commitment tagging without RPC dependency
- `sof-tx`: transaction SDK for building, signing, and submitting Solana transactions through RPC, direct leader routing, hybrid fallback, and optional kernel-bypass transports
- `sof-gossip-tuning`: typed gossip and ingest tuning presets for hosts embedding `sof`

## Highlights

- Multi-core packet ingest, FEC recovery, and dataset reconstruction
- Provider adapters and hook paths optimized once at the framework level instead of per app
- Bundled gossip backend tuning for queue depths, worker counts, CPU pinning, and small-batch serial fallbacks
- Local market-facing control-plane signals for leader, topology, blockhash, replay, and fork state
- Local `processed` / `confirmed` / `finalized` transaction tagging
- Semantic shred dedupe that suppresses duplicate or conflicting downstream event emission
- Plugin hooks and runtime extensions for downstream logic
- Lower-copy hot paths through shared dataset payload fragments and borrowed transaction classification
- Provider-stream parsing optimized for lower copy / lower churn paths where possible
- Replayable derived-state feed for restart-safe stateful services
- First-class `sof-tx` adapters for live plugin and replayable derived-state control-plane inputs
- Flow-safety policy evaluation for stale or degraded tx-control-plane state
- Optional gossip bootstrap and external kernel-bypass ingress
- Transaction submission with RPC, direct, hybrid, and kernel-bypass paths
- Typed gossip and ingest tuning presets for embedded SOF hosts

## Repository Layout

- `crates/sof-observer`: published as `sof`
- `crates/sof-tx`: published as `sof-tx`
- `crates/sof-gossip-tuning`: typed host tuning presets for gossip bootstrap and ingest
- `docs/architecture`: ADRs, ARDs, and framework/runtime contracts
- `docs/operations`: deployment and tuning docs
- `scripts`: local tooling and helper scripts

## Requirements

- Rust stable
- `cargo-make` for the full contributor gate

Install `cargo-make` if needed:

```bash
cargo install cargo-make
```

## Install

Observer/runtime crate:

```bash
cargo add sof
```

Transaction SDK:

```bash
cargo add sof-tx
```

Typed host tuning presets:

```bash
cargo add sof-gossip-tuning
```

Feature examples:

```toml
sof = { version = "0.12.0", features = ["gossip-bootstrap"] }
sof-tx = { version = "0.12.0", features = ["sof-adapters"] }
```

Kernel-bypass integrations:

- `sof` supports external ingress APIs through `--features kernel-bypass`
- `sof-tx` supports custom direct transport adapters through `--features kernel-bypass`
- `sof-solana-gossip` defaults to the lightweight in-memory duplicate/conflict path; pass `--features solana-ledger` only if you explicitly want the heavier ledger/RocksDB duplicate-shred tooling

## Duplicate Shred Policy

SOF is optimized by default for low-latency observer and execution workloads, not validator-style
duplicate-shred durability.

- Default behavior: SOF keeps duplicate/conflict suppression in memory and enforces a semantic
  shred uniqueness contract before downstream dataset, transaction, and account-touch emission.
- Opt-in durability: if you embed the vendored `sof-solana-gossip` crate directly and need the
  heavier validator-style duplicate-shred tooling, build it with `--features solana-ledger`.
- Tradeoff: the default path is lower latency and avoids RocksDB/native storage overhead; the
  ledger-backed path is heavier but keeps the older durable duplicate-shred machinery available.

## Quick Start

Run the observer runtime example:

```bash
cargo run --release -p sof --example observer_runtime
```

Run the observer with gossip bootstrap:

```bash
cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

Run the transaction SDK tests:

```bash
cargo test -p sof-tx
```

Run the derived-state service example:

```bash
cargo run --release -p sof --example derived_state_slot_mirror
```

Run the full contributor gate:

```bash
cargo make ci
```

## Basic Setup Guides

- Observer/runtime setup: `crates/sof-observer/README.md`
- Transaction SDK setup: `crates/sof-tx/README.md`
- Typed gossip tuning setup: `crates/sof-gossip-tuning/README.md`
- Docs entry point: `docs/README.md`
- Contribution guide: `CONTRIBUTING.md`

## Operational Notes

`sof` is not observer-only in gossip mode. By default it can also relay shreds and serve bounded repair responses.

To keep ingest/processing but reduce outward network activity:

- `SOF_UDP_RELAY_ENABLED=false`
- `SOF_REPAIR_ENABLED=false`

## Documentation

- GitBook-compatible docs site: `docs/gitbook/README.md`
- Docs home: `docs/README.md`
- Architecture index: `docs/architecture/README.md`
- Operations index: `docs/operations/README.md`
- Runtime bootstrap modes: `docs/architecture/runtime-bootstrap-modes.md`
- Plugin hook model: `docs/architecture/framework-plugin-hooks.md`
- Runtime extension model: `docs/architecture/runtime-extension-hooks.md`
- Derived-state feed contract: `docs/architecture/derived-state-feed-contract.md`
- Transaction SDK ADR: `docs/architecture/adr/0006-transaction-sdk-and-dual-submit-routing.md`

## CI and Release

- CI workflow: `.github/workflows/ci.yml`
- Release workflow: `.github/workflows/release-crates.yml`
