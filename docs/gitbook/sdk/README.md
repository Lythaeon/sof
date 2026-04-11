# SDKs

Use this section when the goal is to build against SOF from application code instead of wiring the
Rust runtime manually.

Right now the public SDK story is:

- `@sof/sdk` for TypeScript application authoring
- native runtime host packages that the TypeScript SDK resolves automatically at install/runtime

The important boundary is simple:

- TypeScript owns app composition and business callbacks
- Rust still owns ingress, queues, replay, fan-in, gossip, direct shreds, and the runtime hot path

That means the SDK is meant to feel simple without turning SOF into a JavaScript reimplementation.

## Read These First

1. [TypeScript SDK](typescript.md)
2. [Runtime Host and Packaging](runtime-host.md)
3. [Why SOF Exists](../use-sof/why-sof-exists.md)
4. [System Overview](../architecture/system-overview.md)

## What This Track Is For

- teams that want to write SOF apps in TypeScript
- teams that want the Rust runtime underneath without thinking about Cargo
- teams that need provider ingress, fan-in, gossip, or direct shreds from a simpler application API

## What This Track Is Not

- a full foreign-language replacement for the Rust workspace
- a promise that Python is already at feature parity
- a bypass around SOF runtime ownership

The TypeScript SDK is intentionally a thin application-facing layer over the existing Rust runtime.
