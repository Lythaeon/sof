# `@sof/sdk`

Unified TypeScript SDK surface for SOF.

## Mental Model

- Use `ObserverRuntimeConfig` when you want to build or parse the SOF env surface safely.
- Use `ObserverRuntimeConfig.balanced()` / `.deliveryDisciplined()` when you want one-line profile presets.
- Profile presets in this SDK stamp the profile env plus the derived-state replay retention defaults that SOF applies through env-backed setup.
- Rust still owns host-builder dispatch defaults such as plugin-host and runtime-extension-host queue and timeout wiring. This SDK currently models the env/config surface, not those in-process host builders.

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
- focused subpath imports when you only want one SDK slice, for example `@sof/sdk/runtime/config`

## Example

```ts
import {
  DerivedStateReplayBackend,
  DerivedStateReplayConfig,
  DerivedStateReplayDurability,
  ObserverRuntimeConfig,
  ProviderStreamCapabilityPolicy,
  RuntimeDeliveryProfile,
  ShredTrustMode,
  providerStreamAllowEofEnvVarName,
  providerStreamCapabilityPolicyEnvVarName,
  runtimeDeliveryProfileEnvValues,
  runtimeDeliveryProfileEnvVarName,
  shredTrustModeEnvVarName,
} from "@sof/sdk";

const config = ObserverRuntimeConfig.balanced({
  shredTrustMode: ShredTrustMode.TrustedRawShredProvider,
  providerStreamCapabilityPolicy: ProviderStreamCapabilityPolicy.Strict,
  providerStreamAllowEof: true,
  derivedState: {
    checkpointIntervalMs: 60_000,
    recoveryIntervalMs: 10_000,
    replay: {
      backend: DerivedStateReplayBackend.Disk,
      replayDirectory: ".sof-replay",
      durability: DerivedStateReplayDurability.Fsync,
      maxEnvelopes: 1024,
      maxSessions: 2,
    },
  },
});

const env = config.toEnvironment();
// [
//   { name: "SOF_RUNTIME_DELIVERY_PROFILE", value: "balanced" },
//   { name: "SOF_SHRED_TRUST_MODE", value: "trusted_raw_shred_provider" },
//   { name: "SOF_PROVIDER_STREAM_CAPABILITY_POLICY", value: "strict" },
//   { name: "SOF_PROVIDER_STREAM_ALLOW_EOF", value: "true" },
//   { name: "SOF_DERIVED_STATE_CHECKPOINT_INTERVAL_MS", value: "60000" },
//   { name: "SOF_DERIVED_STATE_RECOVERY_INTERVAL_MS", value: "10000" },
//   { name: "SOF_DERIVED_STATE_REPLAY_BACKEND", value: "disk" },
//   { name: "SOF_DERIVED_STATE_REPLAY_DIR", value: ".sof-replay" },
//   { name: "SOF_DERIVED_STATE_REPLAY_DURABILITY", value: "fsync" },
//   { name: "SOF_DERIVED_STATE_REPLAY_MAX_ENVELOPES", value: "1024" },
//   { name: "SOF_DERIVED_STATE_REPLAY_MAX_SESSIONS", value: "2" },
// ]

const envRecord = config.toEnvironmentRecord();
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

const parsed = ObserverRuntimeConfig.fromEnvironmentRecord(envRecord);
const checkpointOnly = DerivedStateReplayConfig.checkpointOnly();

runtimeDeliveryProfileEnvVarName;
runtimeDeliveryProfileEnvValues.deliveryDisciplined;
shredTrustModeEnvVarName;
providerStreamCapabilityPolicyEnvVarName;
providerStreamAllowEofEnvVarName;
parsed;
checkpointOnly;
```

## Quick Start

```ts
import { ObserverRuntimeConfig } from "@sof/sdk";

const config = ObserverRuntimeConfig.deliveryDisciplined();
const env = config.toEnvironmentRecord();

// {
//   SOF_RUNTIME_DELIVERY_PROFILE: "delivery_disciplined",
//   SOF_DERIVED_STATE_REPLAY_MAX_ENVELOPES: "32768",
//   SOF_DERIVED_STATE_REPLAY_MAX_SESSIONS: "8",
// }

env;
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

## Choosing An API

- Use `ObserverRuntimeConfig.fromEnvironmentRecord(...)` when you need to validate env from files, CI, or process managers.
- Use `ObserverRuntimeConfig.balanced(...)` or `observerRuntimeConfigForProfile(...)` when you want profile-first setup.
- Use `derivedStateRuntimeConfig(...)` or `DerivedStateRuntimeConfig.checkpointOnly()` when your main concern is derived-state recovery behavior.
- Use the root `@sof/sdk` import for convenience. Use subpath imports when you want a smaller, more explicit import surface in application code.
