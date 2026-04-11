import assert from "node:assert/strict";
import test from "node:test";

test("package exports resolve the documented public entry points", async () => {
  const root = await import("@sof/sdk");
  const runtime = await import("@sof/sdk/runtime");
  const config = await import("@sof/sdk/runtime/config");
  const policy = await import("@sof/sdk/runtime/policy");
  const derivedState = await import("@sof/sdk/runtime/derived-state");
  const deliveryProfile = await import("@sof/sdk/runtime/delivery-profile");

  assert.equal(root.ObserverRuntimeConfig, config.ObserverRuntimeConfig);
  assert.equal(root.observerRuntimeConfig, config.observerRuntimeConfig);
  assert.equal(runtime.ObserverRuntimeConfig, config.ObserverRuntimeConfig);
  assert.equal(runtime.ShredTrustMode, policy.ShredTrustMode);
  assert.equal(
    runtime.DerivedStateReplayConfig,
    derivedState.DerivedStateReplayConfig,
  );
  assert.equal(
    runtime.RuntimeDeliveryProfile,
    deliveryProfile.RuntimeDeliveryProfile,
  );
});
