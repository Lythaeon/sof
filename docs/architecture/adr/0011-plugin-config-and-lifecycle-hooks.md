# ADR-0011: Plugin Config and Lifecycle Hooks

- Status: Implemented
- Date: 2026-03-21
- Decision makers: `sof-observer` maintainers
- Related: `docs/architecture/framework-plugin-hooks.md`, `docs/architecture/adr/0008-runtime-extension-capability-and-filtered-ingress.md`, `docs/architecture/ard/0007-infrastructure-composition-and-runtime-model.md`

## Context

`ObserverPlugin` originally exposed one `wants_*` method per hook family.

That shape worked, but it had two problems:

1. static subscription intent was fragmented across many tiny boolean methods,
2. plugins had no explicit lifecycle hooks for initialization or cleanup.

The host already builds immutable dispatch targets once at startup, so the subscription model is
fundamentally static. The trait surface should reflect that directly instead of pretending each
hook family needs an independent method.

At the same time, real plugins increasingly need lifecycle behavior:

1. initialize local resources,
2. validate environment or external dependencies,
3. emit startup diagnostics,
4. flush or tear down helper state on shutdown.

Leaving that lifecycle behavior to ad hoc code around the runtime entrypoint makes plugin behavior
less portable and less observable.

## Decision

Adopt an explicit plugin config and lifecycle model.

1. Replace per-hook `wants_*` methods with one `config() -> PluginConfig`.
2. Keep `PluginConfig` static and host-owned.
3. Add `setup(ctx)` for plugin initialization.
4. Add `shutdown(ctx)` for plugin cleanup.
5. Keep transaction/account-touch acceptance and routing classifiers separate because they remain
   event-dependent hot-path decisions rather than static subscriptions.

## Rationale

### Why `PluginConfig`

`PluginConfig` matches runtime reality better than scattered `wants_*` methods:

1. subscriptions are read once during host construction,
2. dispatch tables are precomputed once,
3. hook enablement does not change during runtime.

That means one returned config object is the natural abstraction boundary.

### Why lifecycle hooks

Plugins should be self-contained units of runtime behavior.

Adding `setup` and `shutdown` gives plugins a clear place to:

1. allocate or validate plugin-local state,
2. log initialization facts,
3. fail startup explicitly when prerequisites are missing,
4. flush final state during clean shutdown.

This keeps plugin lifecycle behavior inside the plugin contract instead of spreading it across app
entrypoints.

### Why not combine config and startup

We explicitly do not combine subscription declaration and startup side effects into one method.

Reason:

1. config is static metadata,
2. startup is operational behavior that may fail,
3. mixing both makes host construction less predictable and harder to reason about.

## Consequences

Positive:

1. plugin subscriptions are simpler to read and document,
2. host construction matches the actual static-dispatch model,
3. plugins gain an explicit lifecycle without becoming runtime extensions,
4. runtime startup can fail fast on plugin initialization errors.

Tradeoffs:

1. this is a breaking API change for existing plugin implementations,
2. plugin startup is now part of runtime startup behavior,
3. shutdown remains best-effort and should not be treated as a transactional durability boundary.

## Operational Semantics

The packaged runtime applies the following lifecycle order:

1. plugins are registered into `PluginHost`,
2. the host evaluates `PluginConfig` once and precomputes dispatch targets,
3. `setup` runs sequentially in registration order,
4. ingest/runtime main loop runs,
5. `shutdown` runs sequentially in reverse registration order.

If one plugin fails during startup, runtime startup aborts and SOF attempts to run shutdown hooks
for any plugins that already started successfully.

## Alternatives Considered

### 1. Keep `wants_*` and add lifecycle hooks only

Rejected because it preserves the noisy trait surface without matching the static-host reality.

### 2. Combine setup and subscription declaration in one startup hook

Rejected because it mixes pure metadata with fallible side effects.

### 3. Move plugins fully toward the `RuntimeExtension` manifest model

Rejected because plugins remain semantic event consumers rather than capability/resource owners.

## Follow-Up

The plugin hooks reference documentation and GitBook examples must stay aligned with:

1. `PluginConfig` as the static subscription surface,
2. automatic runtime startup/shutdown invocation,
3. the continued distinction between plugin lifecycle hooks and runtime-extension manifests.
