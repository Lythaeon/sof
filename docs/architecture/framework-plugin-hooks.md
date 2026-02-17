# Framework Plugin Hooks

`sof-observer` exposes a plugin framework in `sof::framework` so consumers can run
custom logic without forking the runtime.

## Public API Surface

- Trait:
  - `ObserverPlugin` (also re-exported as `Plugin`)
- Event payloads:
  - `ClusterTopologyEvent`
  - `RawPacketEvent`
  - `ShredEvent`
  - `DatasetEvent`
  - `TransactionEvent`
  - `ObservedRecentBlockhashEvent`
  - `LeaderScheduleEvent`
- Host/runtime wiring:
  - `PluginDispatchMode`
  - `PluginHost`
  - `PluginHostBuilder`
  - `PluginHost::latest_observed_recent_blockhash`
  - `PluginHost::latest_observed_tpu_leader`

## Hook Semantics

Current hook count: `7` (must stay in sync with `sof::framework::ObserverPlugin`).

Callbacks are invoked in this order as data flows through runtime:

1. `on_raw_packet`
2. `on_shred`
3. `on_dataset`
4. `on_transaction`
5. `on_recent_blockhash`
6. `on_cluster_topology` (near-real-time, gossip-bootstrap only)
7. `on_leader_schedule` (event-driven, gossip-bootstrap only)

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
- `on_recent_blockhash`:
  - Fires when runtime observes a newer recent blockhash from decoded datasets.
  - Deduplicated by slot/hash in `PluginHost` to avoid per-transaction hook volume.
- `on_cluster_topology`:
  - Fires on meaningful cluster node diffs (added/removed/updated), polled about every 250ms.
  - Emits periodic snapshots for full state reconciliation.
  - Includes gossip/tpu/tvu/rpc endpoints when advertised.
- `on_leader_schedule`:
  - Fires on meaningful leader-assignment diffs (added/removed/updated), event-driven from live slot-leader updates.
  - No polling loop is used for leader emission.
  - Requires live shred processing (`SOF_LIVE_SHREDS_ENABLED=true`) and shred verification (`SOF_VERIFY_SHREDS=true`).
  - `snapshot_leaders` remains in the event schema for compatibility and is typically empty.

## Host Construction

Preferred host creation:

```rust
use sof::framework::{PluginDispatchMode, PluginHost};

let host = PluginHost::builder()
    .with_dispatch_mode(PluginDispatchMode::BoundedConcurrent(8))
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

The packaged runtime dispatches hooks asynchronously using a bounded queue:

- packet ingress and dataset workers enqueue events using non-blocking `try_send`
- a dedicated plugin dispatch worker executes async hook callbacks
- dispatch mode defaults to `Sequential` and can be set to `BoundedConcurrent(N)`
- plugin panics are isolated per hook dispatch; SOF logs the panic and continues
- when the queue is full, hook events are dropped to protect ingest latency

To run the packaged runtime entrypoint:

- Path: `crates/sof-observer/examples/observer_runtime.rs`
- Run: `cargo run --release -p sof --example observer_runtime`

To run the packaged runtime with a plugin attached:

- Path: `crates/sof-observer/examples/observer_with_non_vote_plugin.rs`
- Run: `cargo run --release -p sof --example observer_with_non_vote_plugin`

To run multiple plugins in one host:

- Path: `crates/sof-observer/examples/observer_with_multiple_plugins.rs`
- Run: `cargo run --release -p sof --example observer_with_multiple_plugins`

## Example Plugin

Raydium transaction filter logger:

- Path: `crates/sof-observer/examples/raydium_contract.rs`
- Run (gossip bootstrap): `cargo run --release -p sof --example raydium_contract --features gossip-bootstrap`
- Filters transactions by Raydium program IDs (LaunchLab, CPMM, Legacy V4, Stable Swap, CLMM, Burn & Earn, AMM Routing, Staking, Farm Staking, Ecosystem Farm) and logs only matching transactions.
- Each matching log includes which Raydium program families were touched (`cpmm`, `v4`, `clmm`).

Additional example:

- Path: `crates/sof-observer/examples/non_vote_tx_logger.rs`
- Run (gossip bootstrap): `cargo run --release -p sof --example non_vote_tx_logger --features gossip-bootstrap`
- Progress-log cadence in the example is intentionally throttled:
  dataset every `200`, shreds every `10_000`, vote-only tx every `500` (plus first seen).
- Missing transaction signatures are rendered as `NO_SIGNATURE` for consistent logs.

With `--features gossip-bootstrap`, `SOF_GOSSIP_ENTRYPOINT` defaults to mainnet bootstrap endpoints:
`104.204.142.108:8001,64.130.54.173:8001,85.195.118.195:8001,160.202.131.177:8001`
with `entrypoint.mainnet-beta.solana.com:8001` as a final fallback.
To force direct listener mode (`SOF_BIND`, default `0.0.0.0:8001`) without relay, set `SOF_GOSSIP_ENTRYPOINT` to an empty value.
