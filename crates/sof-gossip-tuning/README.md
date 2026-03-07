# sof-gossip-tuning

Typed gossip and ingest tuning presets for SOF hosts.

This crate does two things:

1. models the subset of tuning SOF can already apply directly
2. keeps desired upstream gossip queue capacities explicit without pretending they are wired

## What SOF Can Apply Today

- ingest queue mode
- ingest queue capacity
- UDP batch size
- UDP batch coalesce window
- receiver pin-by-port / fixed receiver core
- TVU receive socket count

Use it with `sof::runtime::RuntimeSetup`:

```rust
use sof_gossip_tuning::{GossipTuningProfile, HostProfilePreset};

let setup = sof::runtime::RuntimeSetup::new()
    .with_gossip_tuning_profile(GossipTuningProfile::preset(HostProfilePreset::Vps));
```

## What Remains Advisory

`PendingGossipQueuePlan` is intentionally advisory until SOF can thread those capacities into the
actual Agave gossip bootstrap path:

- gossip receiver channel capacity
- socket consume channel capacity
- gossip response channel capacity
- high-level fanout posture
