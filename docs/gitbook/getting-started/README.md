# Getting Started

Start here to get from evaluation to a first working integration.

If Solana traffic internals still feel unfamiliar, do not skip the onboarding pages:

1. [Before You Start](before-you-start.md)
2. [Common Questions](common-questions.md)

Before you start, keep the product model simple:

- if you need ingest, local state, plugin events, or operations guidance, start with `sof`
- if you need transaction building and submission, start with `sof-tx`
- if you need both local observation and local submission decisions, use them together

## Who This Is For

- Runtime embedders who need a local Solana ingest engine
- Execution services that want locally sourced leader and blockhash signals
- Operators bringing up SOF on a VPS, dedicated host, or custom ingress stack

## Recommended Reading Order

1. [Choose the Right SOF Path](../use-sof/adoption-paths.md)
2. [Before You Start](before-you-start.md)
3. [Common Questions](common-questions.md)
4. [Install SOF](install-sof.md)
5. [First Runtime Bring-Up](first-runtime.md)
6. [Crates](../crates/README.md)
7. [Operations](../operations/README.md)

Repository and contributor material lives under [Maintain SOF](../maintainers/README.md).

## What You Should Know Before Deploying

- `sof` can be an active relay client in gossip mode, not just a passive observer
- `sof-tx` is designed for execution services, not for wallet UX
- typed tuning profiles are preferred over ad hoc environment bundles
- derived-state and local control-plane freshness are part of the design, not side topics

If that list already feels too dense, read [Before You Start](before-you-start.md) before going
further.

Once you are comfortable with those basics, move into the crate guides and operations chapters.
