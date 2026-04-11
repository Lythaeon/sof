# `@sof/sdk`

Unified TypeScript SDK surface for SOF.

This initial package slice provides:

- checked `Result<T, E>` primitives
- branded/value-object types for domain strings
- enum-backed runtime policy types
- typed SOF runtime config serialization for:
  - `SOF_RUNTIME_DELIVERY_PROFILE`
  - `SOF_SHRED_TRUST_MODE`
  - `SOF_PROVIDER_STREAM_CAPABILITY_POLICY`
  - `SOF_PROVIDER_STREAM_ALLOW_EOF`
- typed environment entry helpers instead of only raw string maps

## Example

```ts
import {
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

const config = new ObserverRuntimeConfig({
  runtimeDeliveryProfile: RuntimeDeliveryProfile.Balanced,
  shredTrustMode: ShredTrustMode.TrustedRawShredProvider,
  providerStreamCapabilityPolicy: ProviderStreamCapabilityPolicy.Strict,
  providerStreamAllowEof: true,
});

const env = config.toEnvironment();
// [
//   { name: "SOF_RUNTIME_DELIVERY_PROFILE", value: "balanced" },
//   { name: "SOF_SHRED_TRUST_MODE", value: "trusted_raw_shred_provider" },
//   { name: "SOF_PROVIDER_STREAM_CAPABILITY_POLICY", value: "strict" },
//   { name: "SOF_PROVIDER_STREAM_ALLOW_EOF", value: "true" },
// ]

const envRecord = config.toEnvironmentRecord();
// {
//   SOF_RUNTIME_DELIVERY_PROFILE: "balanced",
//   SOF_SHRED_TRUST_MODE: "trusted_raw_shred_provider",
//   SOF_PROVIDER_STREAM_CAPABILITY_POLICY: "strict",
//   SOF_PROVIDER_STREAM_ALLOW_EOF: "true",
// }

runtimeDeliveryProfileEnvVarName;
runtimeDeliveryProfileEnvValues.deliveryDisciplined;
shredTrustModeEnvVarName;
providerStreamCapabilityPolicyEnvVarName;
providerStreamAllowEofEnvVarName;
```
