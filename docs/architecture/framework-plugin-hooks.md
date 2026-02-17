# Framework Plugin Hooks

`sof-observer` exposes a plugin framework in `sof_observer::framework` so consumers can run
custom logic without forking the runtime.

## Public API Surface

- Trait:
  - `ObserverPlugin` (also re-exported as `Plugin`)
- Event payloads:
  - `RawPacketEvent`
  - `ShredEvent`
  - `DatasetEvent`
  - `TransactionEvent`
- Host/runtime wiring:
  - `PluginHost`
  - `PluginHostBuilder`

## Hook Semantics

Callbacks are invoked in this order as data flows through runtime:

1. `on_raw_packet`
2. `on_shred`
3. `on_dataset`
4. `on_transaction`

Detailed behavior:

- `on_raw_packet`:
  - Fires for every ingress UDP packet before parse/verify.
  - Includes packet bytes and source socket address.
- `on_shred`:
  - Fires only for packets that parsed as shreds.
  - Includes parsed shred header plus original packet bytes.
- `on_dataset`:
  - Fires for each contiguous reconstructed data range.
  - Includes slot/index bounds, payload length, and tx count.
- `on_transaction`:
  - Fires for each decoded transaction inside a dataset.
  - Includes slot, optional signature, tx payload, and SOF `TxKind`.

## Host Construction

Preferred host creation:

```rust
use sof::framework::PluginHost;

let host = PluginHost::builder()
    .add_plugin(MyPlugin)
    .build();
```

Builder aliases kept for compatibility:

- `with_plugin` == `add_plugin`
- `with_plugin_arc` == `add_shared_plugin`
- `with_plugins` == `add_plugins`
- `with_plugin_arcs` == `add_shared_plugins`

## Plugin Naming

- `Plugin::name()` defaults to `type_name::<Self>()`.
- Override `name()` when you want stable short identifiers in logs/metrics.
- `PluginHost::plugin_names()` returns registration order for startup diagnostics.

## Runtime Wiring

The packaged runtime executes hooks directly on hot paths:

- packet ingress loop (`on_raw_packet`, `on_shred`)
- dataset workers (`on_dataset`, `on_transaction`)
- plugin panics are isolated per hook dispatch; SOF logs the panic and continues

This keeps the engine reusable while preserving low-latency ingest behavior.

## Thread-Safety And Locking Model

SOF runtime internals prefer actor-style ownership over shared mutable locking:

- bounded channels for cross-task handoff
- atomics for counters/flags
- lock-free queues for dataset work dispatch

This minimizes lock contention on ingest hot paths.

Plugin callbacks, however, may be invoked concurrently from different runtime tasks. Because of
that, plugins must treat internal shared state as concurrent state.

Recommended plugin patterns:

- high-rate counters/flags: `Arc<AtomicU64>` / `Arc<AtomicBool>`
- low-rate mutable maps/sets/config snapshots: `Arc<std::sync::Mutex<T>>` or `Arc<std::sync::RwLock<T>>`
- expensive or blocking work: offload via bounded channel to a dedicated worker task

Avoid taking long-lived locks inside hook bodies.

## Performance Rules For Plugin Authors

- Keep hook handlers non-blocking.
- Avoid external network/database calls on hook threads.
- Use bounded channels to offload expensive processing.
- Prefer minimal-copy reads from event payloads.
- Use sampling/throttling when emitting logs on high-volume hooks.

To run the packaged runtime entrypoint:

- Path: `examples/runtime/observer_runtime.rs`
- Run: `cargo run --release -p sof --example observer_runtime`

To run the packaged runtime with a plugin attached:

- Path: `examples/runtime/observer_with_non_vote_plugin.rs`
- Run: `cargo run --release -p sof --example observer_with_non_vote_plugin`

To run multiple plugins in one host:

- Path: `examples/runtime/observer_with_multiple_plugins.rs`
- Run: `cargo run --release -p sof --example observer_with_multiple_plugins`

## Example Plugin

Raydium transaction filter logger:

- Path: `crates/sof-observer/examples/raydium_contract.rs`
- Run (gossip bootstrap): `SOF_GOSSIP_ENTRYPOINT=entrypoint.mainnet-beta.solana.com:8001 SOF_PORT_RANGE=12000-12100 RUST_LOG=info cargo run --release -p sof --example raydium_contract --features gossip-bootstrap`
- Filters transactions by Raydium program IDs (LaunchLab, CPMM, Legacy V4, Stable Swap, CLMM, Burn & Earn, AMM Routing, Staking, Farm Staking, Ecosystem Farm) and logs only matching transactions.
- Each matching log includes which Raydium program families were touched (`cpmm`, `v4`, `clmm`).

Additional example:

- Path: `crates/sof-observer/examples/non_vote_tx_logger.rs`
- Run (gossip bootstrap): `SOF_GOSSIP_ENTRYPOINT=entrypoint.mainnet-beta.solana.com:8001 SOF_PORT_RANGE=12000-12100 RUST_LOG=info cargo run --release -p sof --example non_vote_tx_logger --features gossip-bootstrap`
- Progress-log cadence in the example is intentionally throttled:
  dataset every `200`, shreds every `10_000`, vote-only tx every `500` (plus first seen).
- Missing transaction signatures are rendered as `NO_SIGNATURE` for consistent logs.

With `--features gossip-bootstrap`, `SOF_GOSSIP_ENTRYPOINT` defaults to mainnet bootstrap endpoints:
`104.204.142.108:8001,64.130.54.173:8001,85.195.118.195:8001,160.202.131.177:8001`
with `entrypoint.mainnet-beta.solana.com:8001` as a final fallback.
To force direct listener mode (`SOF_BIND`, default `0.0.0.0:8001`) without relay, set `SOF_GOSSIP_ENTRYPOINT` to an empty value.
