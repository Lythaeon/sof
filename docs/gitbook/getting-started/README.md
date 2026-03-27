# Getting Started

This section is for getting from evaluation to a first working setup.

Keep the product model simple:

- use `sof` when you need local ingest, local state, plugins, or operations control
- use `sof-tx` when you need transaction construction and submission
- use both when one service observes traffic and submits from that local view

The other thing to get clear early:

- ingress determines how early your host sees traffic
- SOF is useful because it gives you one reusable runtime across those ingress choices

Use [Before You Start](before-you-start.md) for the full latency and trust model.

## Recommended Reading Order

1. [Choose the Right SOF Path](../use-sof/adoption-paths.md)
2. [Before You Start](before-you-start.md)
3. [Common Questions](common-questions.md)
4. [Install SOF](install-sof.md)
5. [First Runtime Bring-Up](first-runtime.md)
6. [Crates](../crates/README.md)
7. [Operations](../operations/README.md)

Repository and contributor material lives under [Maintain SOF](../maintainers/README.md).

## Who This Is For

- teams embedding a local Solana observer/runtime
- execution services that want local blockhash or leader inputs
- operators bringing up SOF on a VPS, dedicated host, or custom ingress stack

## What To Decide Early

- are you using `sof`, `sof-tx`, or both
- what ingress reaches your host earliest
- whether you want public-edge independence or trusted low-latency ingress
- whether the first version needs only observation or also local submission

Once those decisions are clear, the crate guides and operations pages become much easier to read.
