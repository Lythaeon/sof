# `@sof/sdk`

Unified TypeScript SDK surface for SOF.

This initial package slice provides:

- checked `Result<T, E>` primitives
- enum-backed runtime delivery profile types
- typed SOF runtime config serialization for `SOF_RUNTIME_DELIVERY_PROFILE`

## Example

```ts
import {
  ObserverRuntimeConfig,
  RuntimeDeliveryProfile,
  runtimeDeliveryProfileToEnvValue,
} from "@sof/sdk";

const config = new ObserverRuntimeConfig({
  runtimeDeliveryProfile: RuntimeDeliveryProfile.Balanced,
});

const env = config.toEnvironment();
// { SOF_RUNTIME_DELIVERY_PROFILE: "balanced" }

runtimeDeliveryProfileToEnvValue(RuntimeDeliveryProfile.DeliveryDisciplined);
// "delivery_disciplined"
```
