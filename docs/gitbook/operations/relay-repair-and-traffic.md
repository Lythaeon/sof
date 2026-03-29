# Relay, Repair, and Traffic

In gossip mode, SOF is not observer-only by default.

That is the main reason this page exists.

If that sentence is surprising, go back to [Before You Start](../getting-started/before-you-start.md)
first. The rest of this guide assumes the difference between direct UDP mode and gossip-bootstrap
mode is already clear.

## Default Network Posture

When `gossip-bootstrap` is enabled, SOF can:

- receive live traffic from the gossip-discovered network
- relay recent shreds through a bounded cache
- serve bounded repair responses

This is cluster-participating behavior. Use it because you need it, not because you assume gossip
is the premium path.

## Control Plane Only

If you only need gossip-derived cluster topology without gossip shred ingest, use:

```bash
SOF_GOSSIP_RUNTIME_MODE=control_plane_only
```

Or in typed runtime setup:

```rust
use sof::runtime::{GossipRuntimeMode, RuntimeSetup};

let setup = RuntimeSetup::new()
    .with_gossip_runtime_mode(GossipRuntimeMode::ControlPlaneOnly);
```

That mode keeps the gossip control plane alive but does not start gossip-backed shred ingest,
relay, repair, or runtime switching. It is useful when `sof-tx` needs topology/leader inputs but
your actual data ingress comes from somewhere else. You still need a recent-blockhash source from
SOF's other ingest paths, RPC, or your own control plane.

## Reduce Outbound Activity

If your goal is to keep ingest while cutting outward network activity, start here:

```bash
SOF_UDP_RELAY_ENABLED=false
SOF_REPAIR_ENABLED=false
```

Before disabling behavior entirely, also consider narrower changes such as:

- narrowing `SOF_GOSSIP_VALIDATORS`
- keeping `SOF_UDP_RELAY_REQUIRE_TURBINE_SOURCE_PORTS=true`
- lowering `SOF_UDP_RELAY_MAX_SENDS_PER_SEC` incrementally

## Why Boundedness Matters

Relay and repair are not allowed to grow without limit.

Examples of explicit bounds:

- relay cache window and maximum retained shreds
- relay fanout and sends-per-second budgets
- repair request budgets per tick
- per-slot repair caps
- per-peer serve limits and byte budgets

These limits are part of the anti-amplification and hot-path discipline, not just tuning details.
