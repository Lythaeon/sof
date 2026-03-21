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
    PluginConfig::new().with_transaction().with_dataset()
}
```

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

- `on_startup(ctx)`
- `on_shutdown(ctx)`

Use them when a plugin needs to:

- validate configuration
- initialize helper state
- open or prepare plugin-local resources
- emit startup diagnostics
- flush final state on shutdown

Typical lifecycle shape:

```rust
async fn on_startup(&self, ctx: PluginStartupContext) -> Result<(), PluginStartupError> {
    tracing::info!(plugin = ctx.plugin_name, "plugin startup completed");
    Ok(())
}

async fn on_shutdown(&self, ctx: PluginShutdownContext) {
    tracing::info!(plugin = ctx.plugin_name, "plugin shutdown completed");
}
```

## Runtime Semantics

When a `PluginHost` is passed into the packaged SOF runtime:

1. plugin configs are read and dispatch targets are built
2. `on_startup` runs once in registration order
3. normal event hooks run during the main loop
4. `on_shutdown` runs once in reverse registration order

If one plugin fails during startup, SOF aborts runtime startup and shuts down any plugins that had
already started.

## What Stays Dynamic

`PluginConfig` is only for static hook enablement.

These decisions still remain per-event and dynamic:

- `accepts_transaction_ref`
- `transaction_interest_ref`
- `accepts_account_touch_ref`

That split matters because those methods are hot-path classifiers, not startup metadata.
