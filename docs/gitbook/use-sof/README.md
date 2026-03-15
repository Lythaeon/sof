# Use SOF

This track is for people who want to use SOF as a product, not maintain the repository.

At the highest level:

- `sof` is the observer/runtime product
- `sof-tx` is the execution and submission product
- `sof-gossip-tuning` is the typed tuning layer for embedded `sof` hosts

Read this section if you are:

- embedding `sof` into your own runtime or service
- using `sof-tx` for execution or routing
- operating SOF on a host and tuning deployment behavior
- evaluating whether SOF fits your architecture

If you are still trying to understand the network model itself, start with the onboarding pages
before you go deeper into crate and operations material:

- [Before You Start](../getting-started/before-you-start.md)
- [Common Questions](../getting-started/common-questions.md)

## Start Here

1. [Choose the Right SOF Path](adoption-paths.md)
2. [Before You Start](../getting-started/before-you-start.md)
3. [Common Questions](../getting-started/common-questions.md)
4. [Choose Your Control Plane Source](control-plane-sourcing.md)
5. [Common Recipes](common-recipes.md)
6. [Build An Observer Service](observer-service.md)
7. [Build One Process That Observes And Submits](observe-and-submit-service.md)
8. [Build A Live-Only Stream Service](live-only-stream-service.md)
9. [Getting Started](../getting-started/README.md)
10. [First Runtime Bring-Up](../getting-started/first-runtime.md)
11. [Crates](../crates/README.md)
12. [Operations](../operations/README.md)

## What This Track Optimizes For

- choosing the right crate or combination of crates
- getting to a working runtime quickly
- understanding the operational posture before production use
- avoiding repository-internal detail unless it is needed for integration decisions

If you are changing SOF itself, move to [Maintain SOF](../maintainers/README.md).
