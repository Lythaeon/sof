# sof-gossip-tuning

Typed gossip and ingest tuning presets for SOF hosts.

This crate does two things:

1. models the subset of tuning SOF can already apply directly
2. keeps desired upstream gossip queue capacities explicit without pretending they are wired

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

Use it with `sof::runtime::RuntimeSetup`:

```rust
use sof_gossip_tuning::{GossipTuningProfile, HostProfilePreset};

let setup = sof::runtime::RuntimeSetup::new()
    .with_gossip_tuning_profile(GossipTuningProfile::preset(HostProfilePreset::Vps));
```

Built-in presets:

- `Home`: bounded ingest and conservative socket fanout for small self-hosted machines
- `Vps`: lockfree ingest and wider gossip capacity for constrained but public-facing hosts
- `Dedicated`: aggressive fanout and larger queue budgets for dedicated ingest machines

## What Remains Advisory

`PendingGossipQueuePlan` is intentionally advisory until SOF can thread those capacities into the
actual Agave gossip bootstrap path:

- gossip receiver channel capacity
- socket consume channel capacity
- gossip response channel capacity
- high-level fanout posture

## Fuzzing

The crate includes a dedicated fuzz target for profile projection through the runtime tuning port:

```bash
cargo +nightly fuzz run profile_runtime_port
```
