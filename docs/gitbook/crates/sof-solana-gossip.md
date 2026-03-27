# `sof-solana-gossip`

`sof-solana-gossip` is the vendored gossip backend used by SOF's optional `gossip-bootstrap` path.

This page is mostly maintainer-facing. External users usually only need it when debugging the
bootstrap backend or understanding low-level gossip tuning behavior.

## Why It Exists In This Repository

SOF needs tighter control over gossip runtime behavior than the stock upstream package exposes.

The vendored backend makes room for:

- explicit queue-capacity controls
- worker-count tuning
- CPU pinning for gossip threads
- SOF-specific tuning and telemetry
- the lighter default duplicate and conflict path used by SOF

## Who Should Care

- operators using `gossip-bootstrap`
- contributors changing runtime gossip behavior
- anyone debugging queue pressure or control-plane traffic inside the bootstrap path

If you are only using direct UDP or processed providers, you can mostly ignore this crate.
