# First Runtime Bring-Up

This page is the shortest path to running `sof` locally and understanding which mode you are in.

## Try It From This Repository

If you are evaluating SOF from a repository checkout, start with the packaged examples.

### Simplest Runtime Start

Run the packaged example:

```bash
cargo run --release -p sof --example observer_runtime
```

That path uses SOF's direct UDP listener mode.

### Enable Gossip Bootstrap

If you want SOF to discover cluster peers and run the gossip-backed bootstrap path, build with the
feature enabled:

```bash
cargo run --release -p sof --example observer_runtime --features gossip-bootstrap
```

Important runtime effect:

- without `gossip-bootstrap`, `sof` remains a local observer/runtime
- with `gossip-bootstrap`, SOF can also operate as an active relay and bounded repair client

## Embed It In Your Own App

If you are consuming `sof` as a crate, start with `RuntimeSetup` or the plain runtime entrypoints.

### Programmatic Setup

Most embedders should configure runtime startup with `RuntimeSetup` rather than stuffing business
logic into environment strings.

```rust
#[tokio::main]
async fn main() -> Result<(), sof::runtime::RuntimeError> {
    let setup = sof::runtime::RuntimeSetup::new()
        .with_startup_step_logs(true);
    sof::runtime::run_async_with_setup(&setup).await
}
```

Useful setup helpers include:

- `with_bind_addr(...)`
- `with_gossip_entrypoints(...)`
- `with_gossip_validators(...)`
- `with_gossip_tuning_profile(...)`
- `with_derived_state_config(...)`
- `with_packet_workers(...)`
- `with_dataset_workers(...)`

## First Operational Knobs

Use only these when you are bringing up a host for the first time:

- `RUST_LOG`
- `SOF_BIND`
- `SOF_GOSSIP_ENTRYPOINT` for gossip mode

If you need less outbound activity while keeping ingest active:

- `SOF_UDP_RELAY_ENABLED=false`
- `SOF_REPAIR_ENABLED=false`

Do not start with the advanced queue, repair, or gossip thread knobs unless you are already
measuring a concrete problem.

## Examples Worth Trying

- `observer_runtime`: base runtime bring-up
- `observer_with_non_vote_plugin`: plugin wiring
- `runtime_extension_websocket_connector`: extension resource model
- `derived_state_slot_mirror`: derived-state style consumer example; run it with
  `SOF_RUN_EXAMPLE=1` because it is intentionally guarded
- `kernel_bypass_ingress_metrics`: external ingress path
