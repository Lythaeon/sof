# `sof-gossip-tuning`

`sof-gossip-tuning` provides a typed control surface for host tuning. It exists so embedders can
configure SOF in code without scattering string environment overrides and magic numbers through
their applications.

## What The Crate Models

- ingest queue mode
- ingest queue capacity
- UDP batch sizing and coalesce window
- receiver fanout and pinning
- TVU socket count
- bundled gossip queue capacities used by the optional bootstrap path

## Why It Matters

Infrastructure software usually drifts into one of two bad states:

- environment variable sprawl with no typed validation
- hard-coded numeric presets copied across services

This crate keeps those values explicit, validated, and reusable.

## Built-In Presets

| Preset | Intended Host Profile |
| --- | --- |
| `Home` | small self-hosted machine, conservative fanout |
| `Vps` | constrained public host with moderate queue budgets |
| `Dedicated` | dedicated ingest machine with more aggressive fanout |

## Typical Usage

```rust
use sof_gossip_tuning::{GossipTuningProfile, HostProfilePreset};

let setup = sof::runtime::RuntimeSetup::new()
    .with_gossip_tuning_profile(GossipTuningProfile::preset(HostProfilePreset::Vps));
```

## Architectural Role

The crate follows the same shape used elsewhere in the workspace:

- domain types and validated values live in `domain`
- application services project those values into runtime configuration through explicit ports

That keeps tuning logic from turning into a bag of helper functions with no ownership boundary.

## When To Reach For It

Use this crate when:

- you embed `sof` into a long-lived service
- you need repeatable host profiles across environments
- you want tuning values reviewed and versioned like code

Skip it only if you are doing one-off local experiments and do not care about preserving a typed
configuration surface.
