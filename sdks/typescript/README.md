# `@sof/sdk`

Unified TypeScript SDK surface for SOF.

This initial package slice provides:

- checked `Result<T, E>` primitives
- branded/value-object types for domain strings
- enum-backed runtime delivery profile types
- typed SOF runtime config serialization for `SOF_RUNTIME_DELIVERY_PROFILE`
- typed environment entry helpers instead of only raw string maps

## Example

```ts
import {
  ObserverRuntimeConfig,
  RuntimeDeliveryProfile,
  runtimeDeliveryProfileEnvValues,
  runtimeDeliveryProfileEnvVarName,
} from "@sof/sdk";

const config = new ObserverRuntimeConfig({
  runtimeDeliveryProfile: RuntimeDeliveryProfile.Balanced,
});

const env = config.toEnvironment();
// [{ name: "SOF_RUNTIME_DELIVERY_PROFILE", value: "balanced" }]

const envRecord = config.toEnvironmentRecord();
// { SOF_RUNTIME_DELIVERY_PROFILE: "balanced" }

runtimeDeliveryProfileEnvVarName;
runtimeDeliveryProfileEnvValues.deliveryDisciplined;
```
