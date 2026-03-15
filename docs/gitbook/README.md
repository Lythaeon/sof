# SOF Documentation

SOF is a Solana infrastructure toolkit.

It is built for services that need one or both of these:

- a local observer/runtime that ingests live Solana traffic and derives control-plane state
- a transaction SDK that uses that control plane for predictable low-latency submission

In practice, SOF is three public products with one internal backend:

- `sof`: observer/runtime
- `sof-tx`: transaction SDK
- `sof-gossip-tuning`: typed host tuning
- `sof-solana-gossip`: vendored bootstrap backend used internally by `sof`

SOF is not a wallet framework and it is not a validator. The project is shaped more like market
infrastructure software: bounded queues, explicit runtime posture, local control-plane state, and
operationally visible tradeoffs.

This book has two separate reading tracks because external users and repository maintainers need
different documentation.

If Solana network internals are still new to you, do not jump straight into crate pages. Start
with:

- [Before You Start](getting-started/before-you-start.md)
- [Common Questions](getting-started/common-questions.md)

## Choose Your Track

### Use SOF

Choose this if you are outside the repository and want to:

- embed `sof` or `sof-tx`
- operate SOF on a host
- evaluate deployment modes and runtime behavior

Start here: [Use SOF](use-sof/README.md)

Recommended next decisions:

- [Before You Start](getting-started/before-you-start.md)
- [Common Questions](getting-started/common-questions.md)
- [Choose Your Control Plane Source](use-sof/control-plane-sourcing.md)
- [Common Recipes](use-sof/common-recipes.md)

### Maintain SOF

Choose this if you are working inside the repository and need:

- workspace layout
- architecture rules
- testing and contributor policy

Start here: [Maintain SOF](maintainers/README.md)

## Workspace Components

- `sof`: the packaged observer/runtime for shred ingest, relay, repair, verification, dataset
  reconstruction, plugin hooks, and runtime extensions
- `sof-tx`: the transaction SDK for building, signing, and submitting Solana transactions through
  RPC, direct leader routing, Jito, or hybrid fallback using provider-supplied control-plane inputs
- `sof-gossip-tuning`: typed tuning profiles for hosts embedding `sof`
- `sof-solana-gossip`: the vendored gossip backend used by the optional `gossip-bootstrap` path

## Design Posture

SOF is opinionated about bounded behavior:

- queues are explicit
- relay and repair are bounded
- replay and local state are first-class concerns
- hot paths avoid unnecessary copies and unbounded coordination
- operational tradeoffs are documented instead of hidden behind defaults

That posture is the through-line for the codebase and for this documentation set.
