# `sof-solana-gossip`

`sof-solana-gossip` is the vendored gossip backend used by SOF's optional `gossip-bootstrap` path.
It is not part of the public workspace member list, but it is patched into the build through the
workspace root.

This page is mostly maintainer-facing. External users usually only need it when debugging the
bootstrap backend or understanding low-level gossip tuning behavior.

## Why It Exists In This Repository

SOF needs tighter control over gossip runtime behavior than the stock upstream package exposes.
The vendored backend makes room for:

- explicit queue-capacity controls
- worker-count tuning
- CPU pinning for gossip threads
- small-batch serial fallback thresholds
- SOF's lighter duplicate and conflict path by default

## Who Should Care

- operators using `gossip-bootstrap`
- contributors changing runtime gossip behavior
- anyone debugging queue pressure or control-plane traffic inside the bootstrap path

If you are only using direct UDP ingest, you can mostly ignore this crate.

## Feature Model

Relevant features:

- `agave-unstable-api`: required by the backend integration
- `solana-ledger`: opt-in heavier validator-style duplicate-shred tooling

Default SOF posture:

- use the lightweight in-memory duplicate/conflict path
- do not enable ledger-backed duplicate-shred tooling unless you explicitly need that durability
  tradeoff

## Contributor Guidance

Treat this crate as infrastructure, not as general product surface.

- changes here can affect bootstrap throughput and network behavior
- queue and worker defaults should be justified with measurement
- if you change operational posture, update the runbooks and architecture notes as well
