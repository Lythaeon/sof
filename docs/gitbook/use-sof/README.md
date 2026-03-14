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

## Start Here

1. [Choose the Right SOF Path](adoption-paths.md)
2. [Choose Your Control Plane Source](control-plane-sourcing.md)
3. [Common Recipes](common-recipes.md)
4. [Getting Started](../getting-started/README.md)
5. [First Runtime Bring-Up](../getting-started/first-runtime.md)
6. [Crates](../crates/README.md)
7. [Operations](../operations/README.md)

## What This Track Optimizes For

- choosing the right crate or combination of crates
- getting to a working runtime quickly
- understanding the operational posture before production use
- avoiding repository-internal detail unless it is needed for integration decisions

If you are changing SOF itself, move to [Maintain SOF](../maintainers/README.md).
