# SOF Documentation

SOF is a Solana observer/runtime and transaction toolkit.

It exists for teams that want to run their own ingest and control-plane layer instead of rebuilding
that layer in every service.

The public crates are:

- `sof`: observer/runtime
- `sof-tx`: transaction construction and submission
- `sof-gossip-tuning`: typed tuning profiles for `sof`

There is also one internal backend crate:

- `sof-solana-gossip`: vendored gossip bootstrap backend used by optional gossip mode

The most important point up front:

SOF is not automatically the fastest way to see Solana traffic.

Latency starts with ingress. If the host sees shreds late, SOF starts late too. The fastest real
deployments usually use one or more of these:

- a private raw shred distribution network
- direct validator-adjacent or peer-adjacent ingress
- host placement close to the source, often in the same datacenter

Public gossip is the independent baseline. It is useful when you want to own the public edge of
the stack yourself. It is usually not the fastest source of shreds.

What SOF gives you is different:

- one bounded runtime for raw shreds, processed providers, or a typed custom provider feed
- one plugin and derived-state model with aligned semantics where the ingress supports them
- explicit replay, dedupe, health, readiness, and queue boundaries
- one place where the low-level Solana ingest work is implemented and measured once

## Start Here

If you are evaluating SOF as a product, read these first:

- [Why SOF Exists](use-sof/why-sof-exists.md)
- [SOF Compared To The Usual Alternatives](use-sof/sof-compared.md)
- [Before You Start](getting-started/before-you-start.md)
- [Common Questions](getting-started/common-questions.md)
- [System Overview](architecture/system-overview.md)

## Choose Your Track

### Use SOF

Choose this track if you want to:

- embed `sof` or `sof-tx`
- run SOF on a host
- compare ingress modes and trust postures
- decide whether SOF fits your service at all

Start here: [Use SOF](use-sof/README.md)

### Maintain SOF

Choose this track if you are working inside the repository and need:

- workspace layout
- architecture constraints
- testing policy
- contributor process

Start here: [Maintain SOF](maintainers/README.md)

## Product Model

- `sof`
  - local observer/runtime
  - can start from raw shreds, processed provider streams, or a typed custom provider feed
- `sof-tx`
  - transaction SDK
  - can use RPC, Jito, signed-byte flows, and optional local control-plane inputs
- `sof-gossip-tuning`
  - typed host presets for `sof`

SOF is not a validator and it is not a wallet framework. The runtime is shaped more like bounded
infrastructure software:

- explicit queues
- explicit trust posture
- explicit replay and dedupe boundaries
- explicit health and readiness signals

That is the posture this documentation set assumes.
