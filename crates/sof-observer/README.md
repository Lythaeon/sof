# sof

`sof` is the observer/runtime crate in the SOF stack.

This crate is what you depend on when you want to:

- ingest Solana data from raw shreds or processed providers
- run plugins against a bounded, multicore runtime
- derive local control-plane and commitment signals without rebuilding the ingest substrate
- expose readiness, health, replay, and queue-pressure semantics through one runtime surface

The crate is intentionally infrastructure-shaped. It is not just a hook registry. It owns the
runtime plumbing under those hooks so application code can stay focused on Solana logic instead of
transport, replay, reconnect, dispatch, and observability.

Crate responsibilities:

- raw shred ingestion (direct UDP, relay, optional gossip bootstrap)
- trusted raw-shred ingest with explicit verification posture
- processed provider ingest (Yellowstone, LaserStream, websocket `transactionSubscribe`, generic)
- dataset reconstruction and transaction extraction
- plugin and derived-state dispatch
- local commitment tagging (`processed` / `confirmed` / `finalized`) without RPC dependency
- bounded runtime health, readiness, and queue observability

## Why Not Rebuild This Per Application

Most teams can write the application logic they want much faster than they can correctly and
efficiently rebuild the substrate under it.

If you build this layer from scratch for every Solana service, you end up re-solving the same
problems:

- raw or provider-stream ingest
- reconnect and backoff behavior
- duplicate suppression and other correctness boundaries
- verification posture and trust modeling
- packet/FEC/dataset reconstruction work
- low-level hot-path tuning around instructions, cache misses, allocations, and copies
- health, readiness, telemetry, and bounded degradation under pressure

SOF packages that work into one runtime so application developers can stay focused on the Solana
program or downstream service they actually want to build.

That is also why SOF tries to keep semantics consistent across ingress modes. The goal is that a
developer writes one plugin/runtime consumer model while SOF owns the provider-specific runtime
plumbing and performance discipline underneath it.

That performance claim is intentionally scoped: on the validated release fixtures on this branch,
no regression was observed on ingest-critical runtime/provider paths, and most of those paths were
net-positive against the older baseline implementations.

## Plugin Contract

The plugin model is intentionally explicit:

- hook subscriptions are static at startup
- borrowed classifiers run on the hot path and should stay cheap
- async hooks run off the ingest hot path through bounded queues
- queue pressure drops hook events instead of stalling ingest
- non-transaction hooks share one bounded queue
- accepted transactions use separate inline-critical, critical, and background lanes
- full queues drop the incoming event; SOF does not evict older queued plugin events
- queue ownership is shared per host/lane, not per plugin
- SOF does not currently guarantee per-plugin fairness under pressure

In other words: overflow is drop-new, not drop-oldest.
- `PluginDispatchMode::Sequential` preserves registration order for one queued event
- `PluginDispatchMode::BoundedConcurrent(n)` gives bounded parallelism instead of strict per-event
  callback ordering
- plugins are not the authoritative replay surface; derived-state consumers are

That means SOF is trying to protect the runtime first and make ordering/backpressure tradeoffs
visible, not implicit.

Queue telemetry is available at aggregate host/lane level:

- `sof_plugin_general_queue_depth`
- `sof_plugin_general_dropped_events_total`
- `sof_plugin_transaction_inline_critical_queue_depth`
- `sof_plugin_transaction_critical_queue_depth`
- `sof_plugin_transaction_background_queue_depth`

Those metrics let operators detect backpressure, event loss, and degraded provider behavior in real
time. Per-plugin pressure visibility is not exposed yet.

## Explicit Trust Model

SOF exposes two explicit raw-shred trust modes:

- `public_untrusted`
  - public gossip or direct public peers
  - keep shred verification on by default
  - highest independence, highest observer-side CPU cost
- `trusted_raw_shred_provider`
  - raw shreds delivered by a provider or private shred-distribution network you explicitly trust
  - best fit when you want SOF's shred-native model without paying public-gossip verification cost
  - this is the expected fast path for low-latency production SOF

`processed_provider_stream` products such as Yellowstone gRPC, LaserStream, or websocket feeds are
easier to consume, but they are a different product category. They are not `SOF_SHRED_TRUST_MODE`
values because they do not feed SOF raw shreds.

`trusted_raw_shred_provider` disables local shred verification by default. Misuse can let invalid
data enter the observer pipeline. Treat it as a trust-boundary choice, not a generic speed knob.

SOF exposes those processed feeds through `ProviderStreamMode`. In that path, provider updates go
straight into transaction or transaction-view-batch dispatch instead of the packet/shred/FEC
pipeline.

Implemented provider-stream adapters:

- Yellowstone gRPC
- LaserStream gRPC
- websocket `transactionSubscribe`

Built-in hook surface by provider mode:

- Yellowstone gRPC: `on_transaction`
- LaserStream gRPC: `on_transaction`
- websocket `transactionSubscribe`: `on_transaction`
- built-in processed providers do not expose standalone control-plane hooks such
  as `on_recent_blockhash`, `on_slot_status`, `on_cluster_topology`,
  `on_leader_schedule`, or `on_reorg`

Built-in durability behavior:

- Yellowstone gRPC: explicit replay modes
  - `Live`: start at stream head
  - `Resume` (default): start live, resume from tracked slot after reconnect
  - `FromSlot(n)`: start from slot `n`, then continue with tracked resume behavior
  - built-in Yellowstone startup now owns the first acknowledged session as the
    live session; it does not open a throwaway preflight subscription first
- LaserStream gRPC: same explicit replay modes on top of SDK replay and slot-watermark tracking
  - built-in LaserStream startup now keeps the first successful subscribe as
    the live stream too, instead of dropping an initial preflight session
- websocket `transactionSubscribe`: uses a stall watchdog and best-effort HTTP RPC gap backfill on
  reconnect when SOF has a matching HTTP endpoint
  - if replay is enabled, startup now fails unless that HTTP endpoint is explicit or derivable from
    the websocket URL
  - this remains best-effort because `transactionSubscribe` itself has no replay cursor
  - SOF can fill recent slot gaps and suppress replay duplicates, but it cannot promise stronger
    durability than the websocket provider plus HTTP RPC backfill path can actually provide
  - built-in websocket startup also promotes the first acknowledged session to
    the live stream, so there is no extra preflight handoff gap
- built-in provider adapters emit explicit source health transitions into SOF,
  and unexpected provider ingress closure is treated as a runtime failure rather
  than a clean stop
  - provider-source health is also exposed through the runtime observability
    endpoint, so reconnecting/unhealthy provider states are visible as metrics
  - provider `/readyz` stays unready until a built-in source has actually
    reached a healthy session, or until a generic producer has emitted real
    ingress progress
  - generic provider replay dedupe also covers transaction-log and
    transaction-view-batch updates now, not only transaction/control-plane
    events
  - provider replay dedupe is runtime-wide for the active provider ingress
    before plugin/derived-state dispatch; it is not a per-plugin cache
  - SOF does not run raw-shred and provider-stream ingest together inside one
    observer runtime, so there is no cross-family replay dedupe boundary inside
    a single running SOF instance

Provider config defaults are inclusive:

- vote transactions are included unless you explicitly set a vote filter
- failed transactions are included unless you explicitly set a failed filter

Built-in processed provider modes are fixed-surface and fail fast when you ask
for hooks they do not emit. `ProviderStreamMode::Generic` is the flexible mode:
it can accept richer control-plane updates from a custom producer, and
`SOF_PROVIDER_STREAM_CAPABILITY_POLICY` controls whether unsupported requests
warn or fail there.

When generic mode continues under `warn`, SOF now exports that degraded
capability state through runtime observability metrics instead of only emitting
one startup log line.

If a generic provider is intentionally finite, enable
`SOF_PROVIDER_STREAM_ALLOW_EOF=true` so a bounded stream can terminate cleanly
instead of being treated as an unexpected live-ingress closure.

The same capability checks apply to derived-state consumers, not just plugins.

SOF's internal transaction classifier hooks, including `transaction_prefilter`,
`accepts_transaction_ref`, and `transaction_interest_ref`, work on the
Yellowstone, LaserStream, and websocket transaction adapters because all three
feed full transactions into `on_transaction`.

Transaction-family hooks can also choose delivery commitment uniformly across
all ingest modes:

```rust
use sof::framework::{PluginConfig, TxCommitmentStatus};

let config = PluginConfig::new()
    .with_transaction()
    .at_commitment(TxCommitmentStatus::Confirmed);

let exact = PluginConfig::new()
    .with_transaction()
    .only_at_commitment(TxCommitmentStatus::Finalized);
```

If neither selector is set, SOF defaults to
`at_commitment(TxCommitmentStatus::Processed)`, so transaction-family hooks see
all commitment levels.

`sof-tx` is a different case: the existing SOF adapters are complete today on
raw-shred/gossip runtimes, or on `ProviderStreamMode::Generic` when the custom
producer also supplies the full control-plane feed. Built-in Yellowstone,
LaserStream, and websocket adapters remain transaction-first today, so SOF
explicitly rejects those adapters at runtime/config validation time.

That tradeoff should be explicit: public gossip is the independent baseline, trusted raw shred
distribution is the fast path, and processed provider streams are a different observer model.

Switching ingress families is therefore not only a connectivity choice. It is also a semantic
choice:

- raw-shred modes expose the richest local observer/control-plane surface
- built-in processed providers are narrower on purpose
- `ProviderStreamMode::Generic` exists when a custom producer needs to restore that richer surface

`ProviderStreamMode::Generic` is SOF's typed adapter boundary. A custom
producer ingests any upstream format it wants and maps it into
`ProviderStreamUpdate` before handing it to the runtime.

That update surface is:

- `Transaction`
- `SerializedTransaction`
- `TransactionLog`
- `TransactionViewBatch`
- `RecentBlockhash`
- `SlotStatus`
- `ClusterTopology`
- `LeaderSchedule`
- `Reorg`
- `Health`

The runtime then routes those typed updates into the normal SOF surfaces:

- `Transaction` / `SerializedTransaction`
  - `on_transaction`
  - derived-state transaction apply when enabled
  - synthesized `on_recent_blockhash` from the transaction message when requested
- `TransactionLog`
  - `on_transaction_log`
- `TransactionViewBatch`
  - `on_transaction_view_batch`
- `RecentBlockhash`
  - `on_recent_blockhash`
- `SlotStatus`
  - `on_slot_status`
- `ClusterTopology`
  - `on_cluster_topology`
- `LeaderSchedule`
  - `on_leader_schedule`
- `Reorg`
  - `on_reorg`
- `Health`
  - provider health/readiness/observability only
  - not a plugin callback

So `Generic` should be read as “custom provider adapter feeds SOF's typed
provider event surface.”

Programmatic setup uses the typed runtime API:

```rust
use sof::runtime::{RuntimeSetup, ShredTrustMode};

let setup = RuntimeSetup::new()
    .with_shred_trust_mode(ShredTrustMode::TrustedRawShredProvider);
```

The equivalent env knob is:

```bash
SOF_SHRED_TRUST_MODE=trusted_raw_shred_provider
```

Do not treat this as a generic “fast mode” switch. It is only correct when the upstream raw shred
source is explicitly trusted. If you are still on public gossip or public peers, `public_untrusted`
is the right mode.

If you need to analyze only a specific gossip peer set, pin runtime switching to the configured
entrypoints:

```bash
SOF_GOSSIP_ENTRYPOINT=1.2.3.4:8001,5.6.7.8:8001
SOF_GOSSIP_ENTRYPOINT_PINNED=true
```

Trusted raw shred ingress still runs through the normal SOF pipeline after admission:

- parse and classify raw packets
- optional FEC recovery
- dataset and transaction reconstruction
- plugin and runtime-extension dispatch

The trust-mode change only affects the default verification posture. It does not bypass
reconstruction or plugin delivery.

See the concrete example in
[`examples/trusted_raw_shred_provider.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/trusted_raw_shred_provider.rs).
For Yellowstone gRPC, see
[`examples/provider_stream_yellowstone_grpc.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/provider_stream_yellowstone_grpc.rs).
For LaserStream, see
[`examples/provider_stream_laserstream.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/provider_stream_laserstream.rs).
For websocket `transactionSubscribe`, see
[`examples/provider_stream_websocket_transaction.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/provider_stream_websocket_transaction.rs).

Build flags:

- Yellowstone gRPC and LaserStream gRPC: `provider-grpc`
- websocket `transactionSubscribe`: `provider-websocket`

## At a Glance

- Embed SOF directly inside a Tokio application
- Attach `Plugin` or `RuntimeExtension` consumers
- Run with built-in UDP ingress or external kernel-bypass ingress
- Treat SOF as a local market-data and control-plane engine, not just a passive observer
- Reuse one optimized runtime foundation instead of rebuilding ingest/perf/correctness plumbing per service
- Use packet-worker and dataset-worker fanout to keep multi-core hosts busy under sustained shred load
- Consume local slot/reorg/transaction/account-touch signals
- Use the replayable derived-state feed for restart-safe stateful consumers
- Apply typed gossip and ingest tuning profiles instead of env-string bundles
- Keep more runtime work on borrowed/shared data instead of eagerly allocating owned transaction or dataset payload copies
- Drop duplicate or conflicting shred observations before they can re-emit duplicate dataset or transaction events downstream
- Treat robustness and accuracy as first-class runtime behavior, not downstream application glue

## Scheduling Model Today

SOF already has an explicit execution shape:

- raw ingress fans out into packet workers
- completed datasets fan out into dataset workers
- provider sessions are supervised independently and feed the same downstream runtime surface where
  semantics line up
- plugin dispatch is explicitly queued and bounded

What is not claimed yet:

- a first-class NUMA-aware scheduler
- automatic host-topology placement
- one universal worker geometry for every host class

Pinning and thread-count controls exist, but high-end placement still needs measurement on the
actual host.

Current playbook:

- public single-socket VPS: start from `sof-gossip-tuning`'s validated `Vps` preset
- processed provider mode: tune replay/durability and source health first, not packet/shred knobs
- trusted raw-shred mode: keep receive, packet-worker, and dataset-worker placement local to the same socket when possible
- multi-socket hosts: treat cross-socket fanout as opt-in after measurement, not a default

## Install

```bash
cargo add sof
```

Optional gossip bootstrap support at compile time:

```toml
sof = { version = "0.13.0", features = ["gossip-bootstrap"] }
```

Optional external `kernel-bypass` ingress support:

```toml
sof = { version = "0.13.0", features = ["kernel-bypass"] }
```

The bundled `sof-solana-gossip` backend defaults to SOF's lightweight in-memory duplicate/conflict
path. The heavier ledger-backed duplicate-shred tooling remains available behind the vendored
crate's explicit `solana-ledger` feature.

## Semantic Shred Dedupe

SOF now treats shred dedupe as a semantic correctness boundary, not just a packet-cache hint.

- One shared semantic shred registry is used across both ingest and canonical emission stages.
- Exact repeats are dropped before they can waste verify/FEC/reassembly work.
- Conflicting repeats are also suppressed before they can re-emit duplicate downstream events.
- The HFT/observer contract is that normal downstream consumers should not need their own duplicate
  shred suppression logic.

The shared registry publishes runtime telemetry for:

- current and max retained shred identities
- current and max eviction-queue depth
- capacity-driven vs expiry-driven evictions
- ingress duplicate/conflict drops
- canonical duplicate/conflict drops

Those metrics are intended to help tune `SOF_SHRED_DEDUP_CAPACITY` and
`SOF_SHRED_DEDUP_TTL_MS` under real traffic instead of guessing.

## Quick Start

Run the bundled runtime example:

```bash
cargo run --release -p sof --example observer_runtime
```

With gossip bootstrap:

```bash
cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

Basic programmatic setup:

```rust
use sof::runtime::{ObserverRuntime, RuntimeSetup};

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let setup = RuntimeSetup::new().with_startup_step_logs(true);

    ObserverRuntime::new()
        .with_setup(setup)
        .run_until_termination_signal()
        .await
}
```

## Runtime API

Embed directly in Tokio:

```rust
use sof::runtime::ObserverRuntime;

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    ObserverRuntime::new().run_until_termination_signal().await
}
```

Or use programmatic setup:

```rust
use std::net::SocketAddr;
use sof::runtime::{ObserverRuntime, RuntimeSetup};

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let setup = RuntimeSetup::new()
        .with_bind_addr(SocketAddr::from(([0, 0, 0, 0], 8001)))
        .with_observability_bind_addr(SocketAddr::from(([127, 0, 0, 1], 9108)))
        .with_startup_step_logs(true);

    ObserverRuntime::new()
        .with_setup(setup)
        .run_until_termination_signal()
        .await
}
```

When `SOF_OBSERVABILITY_BIND` (or `RuntimeSetup::with_observability_bind_addr`) is set, the
packaged runtime also serves:

- `/metrics`
- `/healthz`
- `/readyz`

Or apply one typed gossip/ingest profile instead of stringly env overrides:

```rust
use sof::runtime::{ObserverRuntime, RuntimeSetup};
use sof_gossip_tuning::{GossipTuningProfile, HostProfilePreset};

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let setup = RuntimeSetup::new()
        .with_gossip_tuning_profile(GossipTuningProfile::preset(HostProfilePreset::Vps));

    ObserverRuntime::new()
        .with_setup(setup)
        .run_until_termination_signal()
        .await
}
```

Linux busy-poll is available as an explicit host-side experiment when you want to trade CPU
efficiency for steadier UDP receive behavior:

```rust
use sof::runtime::ObserverRuntime;
use sof_gossip_tuning::{GossipTuningProfile, HostProfilePreset};

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let setup = sof::runtime::RuntimeSetup::new()
        .with_gossip_tuning_profile(GossipTuningProfile::preset(HostProfilePreset::Vps))
        .with_udp_busy_poll_us(50)
        .with_udp_busy_poll_budget(64)
        .with_udp_prefer_busy_poll(true);

    ObserverRuntime::new()
        .with_setup(setup)
        .run_until_termination_signal()
        .await
}
```

With external `kernel-bypass` ingress, feed `RawPacketBatch` values through SOF's ingress queue:

```rust
#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let (tx, rx) = sof::runtime::create_kernel_bypass_ingress_queue();
    // Publish batches from your bypass receiver thread:
    // let _ok = tx.send_batch(batch, false);
    // Spawn your kernel-bypass receiver and forward batches into `tx`.
    sof::runtime::ObserverRuntime::new()
        .with_kernel_bypass_ingress(rx)
        .run_until_termination_signal()
        .await
}
```

The packaged noop inline observer example now supports AF_XDP external ingress directly when built
with `--features "kernel-bypass gossip-bootstrap"` and launched with `SOF_AF_XDP_IFACE=<iface>`.

Run the kernel-bypass ingress metrics example for 180 seconds:

```bash
SOF_KERNEL_BYPASS_EXAMPLE_DURATION_SECS=180 \
  cargo run --release -p sof --example kernel_bypass_ingress_metrics --features kernel-bypass
```

Run the same example against live Solana gossip traffic (real chain data):

```bash
SOF_KERNEL_BYPASS_EXAMPLE_SOURCE=gossip \
SOF_KERNEL_BYPASS_EXAMPLE_DURATION_SECS=180 \
RUST_LOG=info \
  cargo run --release -p sof --example kernel_bypass_ingress_metrics --features "kernel-bypass gossip-bootstrap"
```

Run AF_XDP external-ingress example (requires Linux, AF_XDP-capable NIC setup, and privileges to create XDP sockets/programs):

```bash
SOF_AF_XDP_IFACE=enp17s0 \
SOF_AF_XDP_EXAMPLE_DURATION_SECS=180 \
  cargo run --release -p sof --example af_xdp_kernel_bypass_ingress_metrics --features "kernel-bypass gossip-bootstrap"
```

Notes for high-ingest runs:

- The example configures `SOF_PORT_RANGE=12000-12100` and `SOF_GOSSIP_PORT=8001`.
- It defaults live gossip mode to `SOF_INGEST_QUEUE_MODE=lockfree` with `SOF_INGEST_QUEUE_CAPACITY=262144`.
- The bundled gossip backend also exposes `SOF_GOSSIP_CONSUME_THREADS`, `SOF_GOSSIP_LISTEN_THREADS`, `SOF_GOSSIP_SOCKET_CONSUME_PARALLEL_PACKET_THRESHOLD`, `SOF_GOSSIP_LISTEN_PARALLEL_BATCH_THRESHOLD`, `SOF_GOSSIP_LISTEN_PARALLEL_MESSAGE_THRESHOLD`, and `SOF_GOSSIP_STATS_INTERVAL_SECS` for host-specific tuning.
- `SOF_UDP_DROP_ON_CHANNEL_FULL` only applies to SOF's built-in UDP receiver path (non-external ingress).
- Queue mode is configurable with `SOF_INGEST_QUEUE_MODE`:
  - `bounded` (default): Tokio bounded channel.
  - `unbounded`: Tokio unbounded channel (no backpressure drops; memory grows with load).
  - `lockfree`: lock-free `ArrayQueue` ring + async wakeups.
- Ring/bounded capacity is configurable with `SOF_INGEST_QUEUE_CAPACITY` (default `16384`).

## Plugin Quickstart

```rust
use async_trait::async_trait;
use sof::{
    event::TxKind,
    framework::{Plugin, PluginConfig, PluginHost, TransactionEvent},
    runtime::ObserverRuntime,
};

#[derive(Clone, Copy, Debug, Default)]
struct NonVoteLogger;

#[async_trait]
impl Plugin for NonVoteLogger {
    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction()
    }

    async fn on_transaction(&self, event: &TransactionEvent) {
        if event.kind == TxKind::VoteOnly {
            return;
        }
        tracing::info!(slot = event.slot, kind = ?event.kind, "transaction observed");
    }
}

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let host = PluginHost::builder().add_plugin(NonVoteLogger).build();

    ObserverRuntime::new()
        .with_plugin_host(host)
        .run_until_termination_signal()
        .await
}
```

For sparse plugin subscriptions, prefer `PluginConfig::new().with_*()` so the enabled hooks stand
out clearly. Use a raw `PluginConfig { .. }` literal only when many flags are enabled and the full
shape is easier to scan.

For low-latency transaction consumers, prefer the explicit inline path:

```rust
use async_trait::async_trait;
use sof::{
    event::TxKind,
    framework::{Plugin, PluginConfig, PluginHost, TransactionDispatchMode, TransactionEvent},
    runtime::ObserverRuntime,
};

#[derive(Clone, Copy, Debug, Default)]
struct InlineTxLogger;

#[async_trait]
impl Plugin for InlineTxLogger {
    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction_mode(TransactionDispatchMode::Inline)
    }

    async fn on_transaction(&self, event: &TransactionEvent) {
        if event.kind == TxKind::VoteOnly {
            return;
        }
        tracing::info!(slot = event.slot, kind = ?event.kind, "inline transaction observed");
    }
}

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let host = PluginHost::builder().add_plugin(InlineTxLogger).build();

    ObserverRuntime::new()
        .with_plugin_host(host)
        .run_until_termination_signal()
        .await
}
```

`TransactionDispatchMode::Inline` is an explicit delivery contract for `on_transaction`.
SOF now tries to dispatch that hook as soon as an anchored contiguous dataset prefix
contains one full serialized transaction for the plugin that asked for it, instead of
waiting for the whole dataset by default. If the runtime still cannot anchor the dataset
prefix early, inline dispatch falls back to the completed-dataset point for that tx.
If other plugins or subsystems still need deferred dataset processing, SOF can deliver
the inline transaction hook first and then continue the same dataset through the
standard dataset-worker path for those remaining consumers.

For account/signature-driven transaction filters, prefer `TransactionPrefilter`
over custom `transaction_interest_ref` logic:

```rust
use async_trait::async_trait;
use solana_pubkey::Pubkey;
use sof::framework::{
    Plugin, PluginConfig, TransactionDispatchMode, TransactionInterest, TransactionPrefilter,
};

#[derive(Clone, Debug)]
struct PoolWatcher {
    filter: TransactionPrefilter,
}

impl Default for PoolWatcher {
    fn default() -> Self {
        let pool = Pubkey::new_unique();
        let program = Pubkey::new_unique();
        Self {
            filter: TransactionPrefilter::new(TransactionInterest::Critical)
                .with_account_required([pool, program]),
        }
    }
}

#[async_trait]
impl Plugin for PoolWatcher {
    fn config(&self) -> PluginConfig {
        PluginConfig::new().with_transaction_mode(TransactionDispatchMode::Inline)
    }

    fn transaction_prefilter(&self) -> Option<&TransactionPrefilter> {
        Some(&self.filter)
    }
}
```

When every in-scope inline transaction plugin uses a compiled prefilter and all
of them ignore the tx, SOF can classify that transaction from a sanitized view
and skip full owned `VersionedTransaction` decode entirely.

When observability is enabled, SOF exports exact inline latency counters:

- `sof_inline_transaction_plugin_first_shred_lag_us_total`
- `sof_latest_inline_transaction_plugin_first_shred_lag_us`
- `sof_max_inline_transaction_plugin_first_shred_lag_us`
- `sof_inline_transaction_plugin_last_shred_lag_us_total`
- `sof_latest_inline_transaction_plugin_last_shred_lag_us`
- `sof_max_inline_transaction_plugin_last_shred_lag_us`
- `sof_inline_transaction_plugin_completed_dataset_lag_us_total`
- `sof_latest_inline_transaction_plugin_completed_dataset_lag_us`
- `sof_max_inline_transaction_plugin_completed_dataset_lag_us`

Those track, respectively:

- first observed shred that contributes to the inline tx path -> inline `on_transaction`
  callback start
- last observed shred required to dispatch the inline tx -> inline `on_transaction`
  callback start
- inline dispatch-ready timestamp -> inline `on_transaction` callback start

## RuntimeExtension Quickstart

```rust
use async_trait::async_trait;
use sof::framework::{
    ExtensionCapability, ExtensionContext, ExtensionManifest, PacketSubscription,
    RuntimeExtension, RuntimeExtensionHost, RuntimePacketSourceKind,
};
use sof::runtime::ObserverRuntime;

#[derive(Debug, Clone, Copy)]
struct IngressExtension;

#[async_trait]
impl RuntimeExtension for IngressExtension {
    async fn setup(
        &self,
        _ctx: ExtensionContext,
    ) -> Result<ExtensionManifest, sof::framework::extension::ExtensionSetupError> {
        Ok(ExtensionManifest {
            capabilities: vec![ExtensionCapability::ObserveObserverIngress],
            resources: Vec::new(),
            subscriptions: vec![PacketSubscription {
                source_kind: Some(RuntimePacketSourceKind::ObserverIngress),
                ..PacketSubscription::default()
            }],
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let host = RuntimeExtensionHost::builder()
        .add_extension(IngressExtension)
        .build();

    ObserverRuntime::new()
        .with_extension_host(host)
        .run_until_termination_signal()
        .await
}
```

## Plugin Hooks

Current hook set:

- `on_raw_packet`
- `on_shred`
- `on_dataset`
- `on_transaction`
- `on_account_touch`
- `on_slot_status`
- `on_reorg`
- `on_recent_blockhash`
- `on_cluster_topology` (gossip-bootstrap mode)
- `on_leader_schedule` (gossip-bootstrap mode)

`on_transaction` events include:

- `commitment_status`
- `confirmed_slot`
- `finalized_slot`

These commitment fields are derived from local shred-stream slot progress (depth-based), not RPC polling.

`on_account_touch` events include transaction-derived static account-key sets:

- `account_keys`
- `writable_account_keys`
- `readonly_account_keys`
- `lookup_table_account_keys`

This hook is for account discovery/invalidation. It is not a validator post-write account-update feed.

## Derived-State Consumers

SOF also exposes a replayable derived-state feed intended for stateful official extensions and local consumers that need:

- retained feed continuity
- checkpoint persistence
- replay-based recovery after restart or transient failure
- explicit resync/rebuild signaling
- typed control-plane replay for recent blockhash, cluster topology, and leader schedule inputs
- canonical control-plane quality snapshots through `ControlPlaneStateUpdated`
- invalidation and tx-feedback events through `StateInvalidated` and `TxOutcomeObserved`

This is the right substrate for local service layers that want to build a bank, query index, or gRPC stream on top of SOF without depending on validator-native Geyser.

Example implementation:

- `examples/derived_state_slot_mirror.rs`

Replay retention modes:

- `DerivedStateReplayConfig::checkpoint_only()` disables the runtime-owned replay tail and keeps recovery checkpoint-driven.
- `DerivedStateReplayBackend::Disk` retains envelopes on disk without keeping a full in-process mirror of the retained tail.

Design references:

- `../../docs/architecture/derived-state-extension-contract.md`
- `../../docs/architecture/derived-state-feed-contract.md`

`on_slot_status` events include local canonical transitions:

- `processed`
- `confirmed`
- `finalized`
- `orphaned`

## Operational Notes

- Hooks are dispatched off the ingest hot path through a bounded queue.
- Queue pressure drops hook events instead of stalling ingest.
- Typed host tuning is available through `sof-gossip-tuning` and `RuntimeSetup::with_gossip_tuning_profile(...)`.
- `RuntimeExtension` WebSocket connectors support full `ws://` / `wss://` handshake + frame decoding.
- WebSocket close frames emit `RuntimePacketEventClass::ConnectionClosed` in `on_packet_received`.
- WebSocket packet events expose `websocket_frame_type` (`Text`/`Binary`/`Ping`/`Pong`) for startup-time filtering and runtime routing.
- In gossip mode, SOF runs as an active bounded relay client by default (UDP relay + repair serve), not as an observer-only passive consumer.
- `SOF_LIVE_SHREDS_ENABLED=false` enables control-plane-only mode.

## Examples

- `observer_runtime`
- `observer_with_non_vote_plugin`
- `observer_with_multiple_plugins`
- `non_vote_tx_logger`
- `raydium_contract`
- `tpu_leader_logger`
- `runtime_extension_observer_ingress`
- `runtime_extension_udp_listener`
- `runtime_extension_shared_stream`
- `runtime_extension_with_plugins`
- `runtime_extension_websocket_connector`
- `derived_state_slot_mirror`
- `kernel_bypass_ingress_metrics` (`--features kernel-bypass`)

Run kernel-bypass ingress E2E test:

```bash
cargo test -p sof --features kernel-bypass --test kernel_bypass_ingress_e2e -- --nocapture
```

Run any example:

```bash
cargo run --release -p sof --example observer_with_multiple_plugins
```

## Docs

- Workspace docs index: `../../docs/README.md`
- Architecture docs: `../../docs/architecture/README.md`
- Operations docs: `../../docs/operations/README.md`
- Derived-state feed contract: `../../docs/architecture/derived-state-feed-contract.md`
- Reverse-engineering notes: `REVERSE_ENGINEERING.md`
- Contribution guide: `../../CONTRIBUTING.md`
