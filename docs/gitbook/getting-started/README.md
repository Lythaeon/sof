# Getting Started

This section is the shortest path for an external user to go from evaluation to first working
integration.

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
2. [Install SOF](install-sof.md)
3. [First Runtime Bring-Up](first-runtime.md)
4. [Crates](../crates/README.md)
5. [Operations](../operations/README.md)

If you are maintaining the repository rather than consuming SOF, use the
[Maintain SOF](../maintainers/README.md) track instead.

## What You Should Know Before Deploying

- `sof` can be an active relay client in gossip mode, not just a passive observer
- `sof-tx` is designed for execution services, not for wallet UX
- typed tuning profiles are preferred over ad hoc environment bundles
- derived-state and local control-plane freshness are part of the design, not side topics

Once you are comfortable with those basics, move into the crate guides and operations chapters.
