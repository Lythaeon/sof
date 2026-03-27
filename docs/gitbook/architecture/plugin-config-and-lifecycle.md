# Plugin Config and Lifecycle

SOF plugins now have two distinct responsibilities:

1. declare static hook interest with `PluginConfig`
2. optionally participate in startup and shutdown lifecycle hooks

These responsibilities are separate on purpose.

## Static Hook Config

Plugins declare subscriptions with:

```rust
fn config(&self) -> PluginConfig
```

Default behavior:

- `PluginConfig::default()` enables nothing
- `PluginConfig::new()` is the same empty config

Preferred style for sparse plugin configs:

```rust
fn config(&self) -> PluginConfig {
    PluginConfig::new()
        .with_transaction()
        .at_commitment(TxCommitmentStatus::Confirmed)
        .with_dataset()
}
```

Transaction-family hooks support both threshold and exact commitment selection:

- `.at_commitment(TxCommitmentStatus::Confirmed)`
- `.only_at_commitment(TxCommitmentStatus::Finalized)`

If you do not set either selector, the default is
`.at_commitment(TxCommitmentStatus::Processed)`.

When many flags are enabled, raw struct syntax is also fine:

```rust
fn config(&self) -> PluginConfig {
    PluginConfig {
        raw_packet: true,
        shred: true,
        transaction: true,
        ..PluginConfig::default()
    }
}
```

## Why This Exists

The host already treats plugin subscriptions as static:

- it reads them once during host construction
- it precomputes dispatch target lists once
- it does not add or remove plugins at runtime

So one returned config object matches the actual runtime model better than many `wants_*` methods.

## Lifecycle Hooks

Plugins may also implement:

- `setup(ctx)`
- `shutdown(ctx)`

Use them when a plugin needs to:

- validate configuration
- initialize helper state
- open or prepare plugin-local resources
- emit startup diagnostics
- flush final state on shutdown

Typical lifecycle shape:

```rust
async fn setup(&self, ctx: PluginContext) -> Result<(), PluginSetupError> {
    tracing::info!(plugin = ctx.plugin_name, "plugin startup completed");
    Ok(())
}

async fn shutdown(&self, ctx: PluginContext) {
    tracing::info!(plugin = ctx.plugin_name, "plugin shutdown completed");
}
```

## Runtime Semantics

When a `PluginHost` is passed into the packaged SOF runtime:

1. plugin configs are read and dispatch targets are built
2. `setup` runs once in registration order
3. normal event hooks run during the main loop
4. `shutdown` runs once in reverse registration order

If one plugin fails during startup, SOF aborts runtime startup and shuts down any plugins that had
already started.

## Ordering, Backpressure, and Concurrency

These rules should be treated as part of the plugin contract:

- hook subscriptions are static after host construction
- borrowed classifiers run on the hot path before SOF decides whether to allocate/queue a callback
- normal async hook delivery is off the ingest hot path through bounded queues
- queue pressure drops plugin events instead of blocking ingest
- non-transaction hooks share one bounded queue
- accepted transactions use separate bounded inline-critical, critical, and background lanes
- full queues drop the arriving event; SOF does not evict older queued plugin events
- queue ownership is host-wide per lane, not per plugin
- SOF does not currently guarantee per-plugin fairness under pressure

In other words: overflow is drop-new, not drop-oldest.
- `PluginDispatchMode::Sequential` preserves registration order for one queued event
- `PluginDispatchMode::BoundedConcurrent(n)` keeps parallelism bounded but does not promise the
  same strict per-event callback ordering

If your downstream logic needs stronger replay or ordering guarantees than that, it belongs on the
derived-state surface rather than the observational plugin surface.

## Ownership Model

SOF uses borrowed references on the hot path when it can, then hands async hooks runtime-managed
event values by shared reference. Plugin code should treat those callback arguments as callback
scope data, not as objects whose lifetime it owns outside the hook turn.

## Cost Model Guidance

SOF does not currently enforce a universal nanosecond or microsecond budget per
plugin hook, because the right number depends on host class and ingress mode.

But the intended shape is explicit:

- borrowed classifier hooks should usually be allocation-free
- borrowed classifiers should stay branch-light and data-local
- expensive work should move out of the hot-path classifier and into async
  callback code or downstream worker state
- if callback work is large enough to make queue depth or dropped-event metrics
  climb, the plugin is too expensive for the current host/runtime posture

Treat borrowed classifiers as "cheap enough to run for every candidate tx under
load", not as a place for arbitrary business logic.

## What Stays Dynamic

`PluginConfig` is only for static hook enablement.

These decisions still remain per-event and dynamic:

- `accepts_transaction_ref`
- `transaction_interest_ref`
- `accepts_account_touch_ref`

That split matters because those methods are hot-path classifiers, not startup metadata.
