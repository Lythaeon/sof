# `@sof/sdk`

Unified TypeScript SDK surface for SOF apps and plugins.

## Tooling

- Use `pnpm` for this package.
- `pnpm run build` produces minified ESM output plus `.d.ts` files.
- `pnpm run format:check` verifies Biome formatting.
- `pnpm run lint` runs the type-aware `oxlint` profile.
- `pnpm run check` runs format, lint, typecheck, tests, examples, and package validation.

## Mental Model

- Start with the app-facing APIs: `tryDefinePlugin(...)`, `tryDefineApp(...)`, `tryCreateAppLaunch(...)`, and `runSelectedPlugin(...)`.
- Use runtime-config helpers such as `createRuntimeConfigForProfile(...)`, `serializeRuntimeConfigRecord(...)`, and `parseRuntimeConfig(...)` when you need to build or validate env-backed config.
- Prefer `try...` helpers when invalid input should stay in `Result` form instead of throwing.
- Use subpath imports such as `@sof/sdk/app` or `@sof/sdk/runtime/config` when you want a smaller import surface.

The current package provides:

- checked `Result<T, E>` primitives
- branded/value-object types for domain strings
- enum-backed runtime policy types
- typed runtime config parsing and serialization
- nested derived-state config with safe defaults
- app-first TS APIs for defining apps and plugins
- launch-spec generation for Node-based app entrypoints
- plugin entrypoint helpers for selected-plugin execution

## Quick Start

```ts
import {
  ExtensionCapability,
  RuntimeDeliveryProfile,
  createRuntimeConfigForProfile,
  isErr,
  tryCreateAppLaunch,
  tryDefineApp,
  tryDefinePlugin,
} from "@sof/sdk";

const plugin = tryDefinePlugin({
  name: "demo-plugin",
  capabilities: [ExtensionCapability.ObserveObserverIngress],
});

if (!isErr(plugin)) {
  const app = tryDefineApp({
    name: "demo-sof-app",
    runtime: createRuntimeConfigForProfile(RuntimeDeliveryProfile.Balanced),
    plugins: [plugin.value],
  });

  if (!isErr(app)) {
    const launch = tryCreateAppLaunch(app.value, {
      workerEntrypoint: "./dist/app.js",
    });

    if (!isErr(launch)) {
      launch.value.runtimeEnvironment;
      launch.value.plugins;
    }
  }
}
```

## Plugin API

```ts
import {
  ExtensionCapability,
  isErr,
  ok,
  runtimeExtensionAck,
  tryDefinePlugin,
} from "@sof/sdk";

const plugin = tryDefinePlugin({
  name: "demo-plugin",
  capabilities: [ExtensionCapability.ObserveObserverIngress],
  onStart: () => ok(runtimeExtensionAck()),
  onPacket: () => ok(runtimeExtensionAck()),
  onStop: () => ok(runtimeExtensionAck()),
});

if (!isErr(plugin)) {
  plugin.value.manifest.extensionName;
}
```

## App Entrypoint

```ts
import {
  ExtensionCapability,
  isErr,
  ok,
  runSelectedPlugin,
  runtimeExtensionAck,
  tryDefineApp,
  tryDefinePlugin,
} from "@sof/sdk";

const plugin = tryDefinePlugin({
  name: "demo-plugin",
  capabilities: [ExtensionCapability.ObserveObserverIngress],
  onStart: () => ok(runtimeExtensionAck()),
  onPacket: () => ok(runtimeExtensionAck()),
  onStop: () => ok(runtimeExtensionAck()),
});

if (!isErr(plugin)) {
  const app = tryDefineApp({
    name: "demo-sof-app",
    plugins: [plugin.value],
  });

  if (!isErr(app)) {
    await runSelectedPlugin(app.value);
  }
}
```

## Runtime Config

```ts
import {
  RuntimeDeliveryProfile,
  isErr,
  tryCreateRuntimeConfigForProfile,
  trySerializeRuntimeConfigRecord,
} from "@sof/sdk";

const config = tryCreateRuntimeConfigForProfile(
  RuntimeDeliveryProfile.DeliveryDisciplined,
);

if (!isErr(config)) {
  const env = trySerializeRuntimeConfigRecord(config.value);

  env;
}
```

## Focused Imports

```ts
import {
  createAppLaunch,
  runSelectedPlugin,
  tryDefineApp,
  tryDefinePlugin,
} from "@sof/sdk/app";
import {
  ObserverRuntimeConfig,
  observerRuntimeConfigForProfile,
} from "@sof/sdk/runtime/config";
import {
  ProviderStreamCapabilityPolicy,
  ShredTrustMode,
} from "@sof/sdk/runtime/policy";
import {
  DerivedStateReplayBackend,
  DerivedStateReplayDurability,
} from "@sof/sdk/runtime/derived-state";
import { RuntimeDeliveryProfile } from "@sof/sdk/runtime/delivery-profile";

const config = observerRuntimeConfigForProfile(
  RuntimeDeliveryProfile.DeliveryDisciplined,
  {
    shredTrustMode: ShredTrustMode.TrustedRawShredProvider,
    providerStreamCapabilityPolicy: ProviderStreamCapabilityPolicy.Strict,
    derivedState: {
      replay: {
        backend: DerivedStateReplayBackend.Disk,
        durability: DerivedStateReplayDurability.Fsync,
      },
    },
  },
);

tryDefinePlugin;
tryDefineApp;
createAppLaunch;
runSelectedPlugin;
ObserverRuntimeConfig.latencyOptimized();
config;
```

## Examples

Runnable examples live in `sdks/typescript/examples`:

- `sof-app-entrypoint.ts`
- `sof-app-launch-spec.ts`
- `runtime-config-balanced.ts`
- `runtime-config-parse.ts`
- `runtime-extension-manifest.ts`
- `runtime-extension-worker.ts`

Run them with:

```bash
pnpm run check:examples
```

## Choosing An API

- Use `tryDefinePlugin(...)`, `tryDefineApp(...)`, and `tryCreateAppLaunch(...)` for the normal app authoring flow.
- Use `runSelectedPlugin(...)` in your plugin entrypoint when the app should select the plugin to run from its environment.
- Use `parseRuntimeConfig(...)` or `ObserverRuntimeConfig.fromEnvironmentRecord(...)` when you need to validate env input.
- Use `createRuntimeConfigForProfile(...)` for the simplest runtime-profile workflow.
- Use the root `@sof/sdk` import for convenience. Use subpath imports when you want a smaller, more explicit import surface.
