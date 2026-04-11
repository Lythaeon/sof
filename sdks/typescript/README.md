# `@sof/sdk`

Unified TypeScript SDK surface for SOF.

## Tooling

- Use `pnpm` for this package.
- `pnpm run build` produces minified ESM library output plus `.d.ts` files.
- `pnpm run format:check` verifies Biome formatting for the SDK TS surface.
- `pnpm run lint` runs the `oxlint` production lint profile.
- `pnpm run check` runs lint, typecheck, tests, and package-shape validation.

## Mental Model

- Prefer the functional runtime-config helpers first: `createRuntimeConfigForProfile(...)`, `serializeRuntimeConfigRecord(...)`, and `parseRuntimeConfig(...)`.
- Use `ObserverRuntimeConfig` when you want an explicit config object with instance methods and class-based presets.
- Prefer `tryCreateRuntimeConfig(...)`, `tryCreateRuntimeConfigForProfile(...)`, and `parseRuntimeConfig(...)` when you want validation errors as `Result` values instead of thrown exceptions.
- Use `createRuntimeConfigForProfile(...)` or `ObserverRuntimeConfig.balanced()` / `.deliveryDisciplined()` when you want one-line profile presets.
- Profile presets in this SDK stamp the profile env plus the derived-state replay retention defaults that SOF applies through env-backed setup.
- Rust still owns host-builder dispatch defaults such as plugin-host and runtime-extension-host queue and timeout wiring. This SDK currently models the env/config surface and ships a TS-side extension worker runtime, not the Rust host builders themselves.

This initial package slice provides:

- checked `Result<T, E>` primitives
- branded/value-object types for domain strings
- enum-backed runtime policy types
- typed SOF runtime config serialization and parsing for:
  - `SOF_RUNTIME_DELIVERY_PROFILE`
  - `SOF_SHRED_TRUST_MODE`
  - `SOF_PROVIDER_STREAM_CAPABILITY_POLICY`
  - `SOF_PROVIDER_STREAM_ALLOW_EOF`
  - `SOF_DERIVED_STATE_CHECKPOINT_INTERVAL_MS`
  - `SOF_DERIVED_STATE_RECOVERY_INTERVAL_MS`
  - `SOF_DERIVED_STATE_REPLAY_BACKEND`
  - `SOF_DERIVED_STATE_REPLAY_DIR`
  - `SOF_DERIVED_STATE_REPLAY_DURABILITY`
  - `SOF_DERIVED_STATE_REPLAY_MAX_ENVELOPES`
  - `SOF_DERIVED_STATE_REPLAY_MAX_SESSIONS`
- nested derived-state runtime config with safe defaults and checkpoint-only replay helper
- typed environment entry helpers instead of only raw string maps
- plain-object nested config construction, so common cases do not require chained `new` calls
- one-line runtime profile presets such as `ObserverRuntimeConfig.balanced()`
- small functional helpers for the common create/serialize/parse path, so most consumers do not need to learn the class API first
- result-return factory and serialization helpers for programmatic validation, so SDK consumers do not need to rely on exceptions for normal invalid-input handling
- typed runtime-extension manifest and worker-authoring primitives for TS-side extension contracts
- a ready-to-run Node stdio worker loop for runtime-extension processes, so the SDK is not only a DTO wrapper
- focused subpath imports when you only want one SDK slice, for example `@sof/sdk/runtime/config`

## Quick Start

```ts
import {
  createRuntimeConfigForProfile,
  DerivedStateReplayBackend,
  DerivedStateReplayDurability,
  parseRuntimeConfig,
  ProviderStreamCapabilityPolicy,
  RuntimeDeliveryProfile,
  serializeRuntimeConfigRecord,
  ShredTrustMode,
} from "@sof/sdk";

const config = createRuntimeConfigForProfile(
  RuntimeDeliveryProfile.Balanced,
  {
    shredTrustMode: ShredTrustMode.TrustedRawShredProvider,
    providerStreamCapabilityPolicy: ProviderStreamCapabilityPolicy.Strict,
    providerStreamAllowEof: true,
    derivedState: {
      replay: {
        backend: DerivedStateReplayBackend.Disk,
        durability: DerivedStateReplayDurability.Fsync,
        maxEnvelopes: 1024,
        maxSessions: 2,
      },
    },
  },
);

const envRecord = serializeRuntimeConfigRecord(config);
// {
//   SOF_RUNTIME_DELIVERY_PROFILE: "balanced",
//   SOF_SHRED_TRUST_MODE: "trusted_raw_shred_provider",
//   SOF_PROVIDER_STREAM_CAPABILITY_POLICY: "strict",
//   SOF_PROVIDER_STREAM_ALLOW_EOF: "true",
//   SOF_DERIVED_STATE_CHECKPOINT_INTERVAL_MS: "60000",
//   SOF_DERIVED_STATE_RECOVERY_INTERVAL_MS: "10000",
//   SOF_DERIVED_STATE_REPLAY_BACKEND: "disk",
//   SOF_DERIVED_STATE_REPLAY_DIR: ".sof-replay",
//   SOF_DERIVED_STATE_REPLAY_DURABILITY: "fsync",
//   SOF_DERIVED_STATE_REPLAY_MAX_ENVELOPES: "1024",
//   SOF_DERIVED_STATE_REPLAY_MAX_SESSIONS: "2",
// }

const parsed = parseRuntimeConfig(envRecord);

config;
parsed;
```

## Result Path

```ts
import {
  isErr,
  RuntimeDeliveryProfile,
  tryCreateRuntimeConfigForProfile,
  trySerializeRuntimeConfigRecord,
} from "@sof/sdk";

const config = tryCreateRuntimeConfigForProfile(
  RuntimeDeliveryProfile.DeliveryDisciplined,
);

if (isErr(config)) {
  throw new Error(config.error.message);
}

const env = trySerializeRuntimeConfigRecord(config.value);

env;
```

## Class API

```ts
import { ObserverRuntimeConfig } from "@sof/sdk";

const config = ObserverRuntimeConfig.deliveryDisciplined();
const env = config.toEnvironmentRecord();

env;
```

## Extension Runtime

The TS SDK now includes a typed extension-worker authoring surface under
`@sof/sdk/runtime/extension` plus a ready-to-run Node worker loop under
`@sof/sdk/runtime/extension-stdio`.

Use it for:

- typed extension manifests
- typed packet-subscription matching
- typed in-memory worker lifecycle/runtime
- newline-delimited JSON stdio worker processes with no custom transport loop
- runnable TS examples for future Rust-host integration

Important boundary:

- Rust still owns the actual runtime, queues, sockets, and packet dispatch.
- The current TS SDK extension surface is the TS-side contract plus a real TS worker runtime.
- It does not yet mean the Rust binary can already spawn TS workers directly.

## Examples

Runnable examples live in `sdks/typescript/examples`:

- `runtime-config-balanced.ts`
- `runtime-config-parse.ts`
- `runtime-extension-manifest.ts`
- `runtime-extension-worker.ts`

Verify them with:

```bash
pnpm run check:examples
```

## Focused Imports

```ts
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

ObserverRuntimeConfig.latencyOptimized();
config;
```

## Stdio Worker Runtime

```ts
import {
  ExtensionCapability,
  RuntimeExtensionWorkerHostMessageTag,
  isErr,
  ok,
  runRuntimeExtensionWorkerStdio,
  runtimeExtensionAck,
  tryCreateRuntimeExtensionWorkerManifest,
  tryDefineRuntimeExtension,
} from "@sof/sdk/runtime/extension-stdio";

const manifest = tryCreateRuntimeExtensionWorkerManifest({
  sdkVersion: "0.1.0",
  extensionName: "demo-worker",
  capabilities: [ExtensionCapability.ObserveObserverIngress],
});

if (isErr(manifest)) {
  throw new Error(manifest.error.message);
}

const worker = tryDefineRuntimeExtension({
  manifest: manifest.value,
  onReady: () => ok(runtimeExtensionAck()),
  onPacketReceived: () => ok(runtimeExtensionAck()),
  onShutdown: () => ok(runtimeExtensionAck()),
});

if (isErr(worker)) {
  throw new Error(worker.error.message);
}

await runRuntimeExtensionWorkerStdio(worker.value);
```

## Choosing An API

- Use `ObserverRuntimeConfig.fromEnvironmentRecord(...)` when you need to validate env from files, CI, or process managers.
- Use `parseRuntimeConfig(...)` for env parsing. It accepts either env records or typed environment-variable lists.
- Use `tryCreateRuntimeConfig(...)`, `tryCreateRuntimeConfigForProfile(...)`, or `trySerializeRuntimeConfigRecord(...)` when invalid programmatic input should stay in `Result` form.
- Use `createRuntimeConfigForProfile(...)` or `serializeRuntimeConfigRecord(...)` for the simplest create-and-emit workflow.
- Use `ObserverRuntimeConfig.balanced(...)` or `observerRuntimeConfigForProfile(...)` when you explicitly want the class-oriented surface.
- Use `derivedStateRuntimeConfig(...)` or `DerivedStateRuntimeConfig.checkpointOnly()` when your main concern is derived-state recovery behavior.
- Use `runRuntimeExtensionWorkerStdio(...)` when you want an actual Node worker process instead of only in-memory runtime objects.
- Use the root `@sof/sdk` import for convenience. Use subpath imports when you want a smaller, more explicit import surface in application code.
