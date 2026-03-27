# `sof-gossip-tuning`

`sof-gossip-tuning` provides a typed control surface for host tuning.

It exists so embedders can configure SOF in code without scattering string environment overrides
and copied numeric presets through their applications.

## What The Crate Models

- ingest queue mode and capacity
- UDP batch sizing and coalesce window
- receiver fanout and pinning
- TVU socket count
- bundled gossip queue capacities used by the optional bootstrap path
- bundled gossip drain budget and worker counts
- shred dedupe capacity

## Why It Matters

Infrastructure code usually drifts into one of two bad states:

- environment-variable sprawl with no typed validation
- hard-coded numeric presets copied across services

This crate keeps those values explicit, validated, and reusable.

It is mostly about repeatability and operations hygiene. It does not replace the bigger latency
decisions around ingress choice and host placement.
