# sof-gossip-tuning

Typed gossip and ingest tuning presets for SOF hosts.

This crate does two things:

1. models the subset of tuning SOF can already apply directly
2. keeps bundled gossip queue and worker capacities explicit and typed

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
- gossip drain budget and worker counts
- shred dedupe capacity

Use it with `sof::runtime::RuntimeSetup`:

```rust
use sof_gossip_tuning::{GossipTuningProfile, HostProfilePreset};

let setup = sof::runtime::RuntimeSetup::new()
    .with_gossip_tuning_profile(GossipTuningProfile::preset(HostProfilePreset::Vps));
```

Built-in presets:

- `Home`: bounded ingest and conservative socket fanout for small self-hosted machines
- `Vps`: validated public-host profile with dual-socket fanout, deeper gossip queues, `4` gossip workers per stage, and `524288` shred dedupe capacity
- `Dedicated`: aggressive fanout and larger queue/dedupe budgets for dedicated ingest machines

The `Vps` preset mirrors the public Hetzner-style profile that was validated against live mainnet
traffic:

- `SOF_UDP_BATCH_SIZE=96`
- `SOF_TVU_SOCKETS=2`
- `SOF_UDP_RECEIVER_PIN_BY_PORT=true`
- `SOF_GOSSIP_RECEIVER_CHANNEL_CAPACITY=131072`
- `SOF_GOSSIP_SOCKET_CONSUME_CHANNEL_CAPACITY=65536`
- `SOF_GOSSIP_RESPONSE_CHANNEL_CAPACITY=65536`
- `SOF_GOSSIP_CHANNEL_CONSUME_CAPACITY=4096`
- `SOF_GOSSIP_CONSUME_THREADS=4`
- `SOF_GOSSIP_LISTEN_THREADS=4`
- `SOF_GOSSIP_RUN_THREADS=4`
- `SOF_SHRED_DEDUP_CAPACITY=524288`

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
