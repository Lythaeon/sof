# Use SOF

Use this section when the goal is to decide whether SOF belongs in your service, and if it does,
how to run it honestly.

At the highest level:

- `sof` is the observer/runtime
- `sof-tx` is the execution and submission SDK
- `sof-gossip-tuning` is the typed tuning layer for `sof`

This section is for:

- teams evaluating SOF against RPC, managed providers, validators, or their own ingest stack
- operators choosing between direct UDP, gossip, private raw ingress, or processed providers
- services combining local observation with local submission

## Read These First

1. [Why SOF Exists](why-sof-exists.md)
2. [SOF Compared To The Usual Alternatives](sof-compared.md)
3. [Choose the Right SOF Path](adoption-paths.md)
4. [Before You Start](../getting-started/before-you-start.md)
5. [Common Questions](../getting-started/common-questions.md)

## Then Use The Pages By Job

- control-plane sourcing
  - [Choose Your Control Plane Source](control-plane-sourcing.md)
- service shape
  - [Build An Observer Service](observer-service.md)
  - [Build One Process That Observes And Submits](observe-and-submit-service.md)
  - [Build A Live-Only Stream Service](live-only-stream-service.md)
- recipes and practical choices
  - [Common Recipes](common-recipes.md)
  - [Getting Started](../getting-started/README.md)
  - [Operations](../operations/README.md)

## What This Track Assumes

- ingress choice matters more than slogans about being "fast"
- public gossip is useful, but not usually the lowest-latency source
- private raw feeds, direct validator-adjacent ingress, and good host placement usually beat it
- SOF is most useful when you want one reusable runtime instead of rebuilding the same Solana
  ingest and correctness machinery in every service

Repository-internal material lives under [Maintain SOF](../maintainers/README.md).
