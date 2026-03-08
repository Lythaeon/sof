# sof-gossip-tuning

Typed gossip and ingest tuning presets for SOF hosts.

This crate does two things:

1. models the subset of tuning SOF can already apply directly
2. keeps bundled gossip queue capacities explicit and typed

Its structure follows the same split used elsewhere in SOF: domain types and preset values live in
the domain layer, while the application service projects those profiles into SOF's runtime builder
through an explicit output port.

## What SOF Can Apply Today

- ingest queue mode
- ingest queue capacity
- UDP batch size
- UDP batch coalesce window
- receiver pin-by-port / fixed receiver core
- TVU receive socket count
- gossip receiver / socket-consume / response channel capacities

Use it with `sof::runtime::RuntimeSetup`:

```rust
use sof_gossip_tuning::{GossipTuningProfile, HostProfilePreset};

let setup = sof::runtime::RuntimeSetup::new()
    .with_gossip_tuning_profile(GossipTuningProfile::preset(HostProfilePreset::Vps));
```

Built-in presets:

- `Home`: bounded ingest and conservative socket fanout for small self-hosted machines
- `Vps`: lockfree ingest, moderate gossip queue targets, and dual-socket fanout for constrained public hosts
- `Dedicated`: aggressive fanout and larger queue budgets for dedicated ingest machines

## Queue Plans

`PendingGossipQueuePlan` remains available as a compact view of the gossip queue budget:

- gossip receiver channel capacity
- socket consume channel capacity
- gossip response channel capacity
- high-level fanout posture

## Fuzzing

The crate includes a dedicated fuzz target for profile projection through the runtime tuning port:

```bash
cargo +nightly fuzz run profile_runtime_port
```
