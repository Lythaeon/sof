import assert from "node:assert/strict";
import test from "node:test";

function importPackageEntry(moduleName: string): Promise<unknown> {
  return import(moduleName);
}

test("package exports resolve the documented public entry points", async () => {
  const root = await importPackageEntry("@sof/sdk");
  const runtime = await importPackageEntry("@sof/sdk/runtime");
  const config = await importPackageEntry("@sof/sdk/runtime/config");
  const policy = await importPackageEntry("@sof/sdk/runtime/policy");
  const derivedState = await importPackageEntry("@sof/sdk/runtime/derived-state");
  const deliveryProfile = await importPackageEntry("@sof/sdk/runtime/delivery-profile");
  const extension = await importPackageEntry("@sof/sdk/runtime/extension");

  assert.equal(
    (root as { ObserverRuntimeConfig: unknown }).ObserverRuntimeConfig,
    (config as { ObserverRuntimeConfig: unknown }).ObserverRuntimeConfig,
  );
  assert.equal(
    (root as { createRuntimeConfig: unknown }).createRuntimeConfig,
    (config as { createRuntimeConfig: unknown }).createRuntimeConfig,
  );
  assert.equal(
    (root as { observerRuntimeConfig: unknown }).observerRuntimeConfig,
    (config as { observerRuntimeConfig: unknown }).observerRuntimeConfig,
  );
  assert.equal(
    (root as { tryObserverRuntimeConfig: unknown }).tryObserverRuntimeConfig,
    (config as { tryObserverRuntimeConfig: unknown }).tryObserverRuntimeConfig,
  );
  assert.equal(
    (root as { tryCreateRuntimeConfig: unknown }).tryCreateRuntimeConfig,
    (config as { tryCreateRuntimeConfig: unknown }).tryCreateRuntimeConfig,
  );
  assert.equal(
    (runtime as { ObserverRuntimeConfig: unknown }).ObserverRuntimeConfig,
    (config as { ObserverRuntimeConfig: unknown }).ObserverRuntimeConfig,
  );
  assert.equal(
    (runtime as { ShredTrustMode: unknown }).ShredTrustMode,
    (policy as { ShredTrustMode: unknown }).ShredTrustMode,
  );
  assert.equal(
    (runtime as { DerivedStateReplayConfig: unknown }).DerivedStateReplayConfig,
    (derivedState as { DerivedStateReplayConfig: unknown }).DerivedStateReplayConfig,
  );
  assert.equal(
    (runtime as { RuntimeDeliveryProfile: unknown }).RuntimeDeliveryProfile,
    (deliveryProfile as { RuntimeDeliveryProfile: unknown }).RuntimeDeliveryProfile,
  );
  assert.equal(
    (runtime as { createRuntimeExtensionWorkerManifest: unknown })
      .createRuntimeExtensionWorkerManifest,
    (extension as { createRuntimeExtensionWorkerManifest: unknown })
      .createRuntimeExtensionWorkerManifest,
  );
});
