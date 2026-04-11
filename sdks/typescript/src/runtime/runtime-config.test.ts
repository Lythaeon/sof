import assert from "node:assert/strict";
import test from "node:test";

import { environmentVariable } from "../environment.js";
import { ValidationErrorKind } from "../errors.js";
import { isErr, isOk, ResultTag } from "../result.js";
import {
  ObserverRuntimeConfig,
  RuntimeDeliveryProfile,
  parseRuntimeDeliveryProfile,
  runtimeDeliveryProfileAllowedValues,
  runtimeDeliveryProfileEnvValues,
  runtimeDeliveryProfileEnvVarName,
  runtimeDeliveryProfileToEnvValue,
} from "../runtime.js";

test("result and runtime delivery profile discriminants stay stable", () => {
  assert.equal(ResultTag.Ok, 1);
  assert.equal(ResultTag.Err, 2);
  assert.equal(RuntimeDeliveryProfile.LatencyOptimized, 1);
  assert.equal(RuntimeDeliveryProfile.Balanced, 2);
  assert.equal(RuntimeDeliveryProfile.DeliveryDisciplined, 3);
});

test("runtime delivery profile maps to the documented env values", () => {
  assert.equal(
    runtimeDeliveryProfileToEnvValue(RuntimeDeliveryProfile.LatencyOptimized),
    runtimeDeliveryProfileEnvValues.latencyOptimized,
  );
  assert.equal(
    runtimeDeliveryProfileToEnvValue(RuntimeDeliveryProfile.Balanced),
    runtimeDeliveryProfileEnvValues.balanced,
  );
  assert.equal(
    runtimeDeliveryProfileToEnvValue(RuntimeDeliveryProfile.DeliveryDisciplined),
    runtimeDeliveryProfileEnvValues.deliveryDisciplined,
  );
});

test("runtime delivery profile parser accepts documented aliases", () => {
  const balanced = parseRuntimeDeliveryProfile(" balanced ");
  const disciplined = parseRuntimeDeliveryProfile("delivery-disciplined");

  assert.equal(isOk(balanced), true);
  assert.equal(isOk(disciplined), true);

  if (isOk(balanced)) {
    assert.equal(balanced.value, RuntimeDeliveryProfile.Balanced);
  }
  if (isOk(disciplined)) {
    assert.equal(
      disciplined.value,
      RuntimeDeliveryProfile.DeliveryDisciplined,
    );
  }
});

test("runtime config omits the default profile unless requested", () => {
  const config = new ObserverRuntimeConfig();

  assert.deepEqual(config.toEnvironment(), []);
  assert.deepEqual(config.toEnvironmentRecord({ includeDefaults: true }), {
    [runtimeDeliveryProfileEnvVarName]:
      runtimeDeliveryProfileEnvValues.latencyOptimized,
  });
});

test("runtime config serializes explicit profile selection", () => {
  const config = new ObserverRuntimeConfig({
    runtimeDeliveryProfile: RuntimeDeliveryProfile.Balanced,
  });

  assert.deepEqual(config.toEnvironment(), [
    environmentVariable(
      runtimeDeliveryProfileEnvVarName,
      runtimeDeliveryProfileEnvValues.balanced,
    ),
  ]);
});

test("runtime config parses environment values into typed config", () => {
  const config = ObserverRuntimeConfig.fromEnvironment({
    [runtimeDeliveryProfileEnvVarName]:
      runtimeDeliveryProfileEnvValues.deliveryDisciplined,
  });

  assert.equal(isOk(config), true);
  if (isOk(config)) {
    assert.equal(
      config.value.runtimeDeliveryProfile,
      RuntimeDeliveryProfile.DeliveryDisciplined,
    );
  }
});

test("runtime config parses typed environment variables into typed config", () => {
  const config = ObserverRuntimeConfig.fromEnvironmentVariables([
    environmentVariable(
      runtimeDeliveryProfileEnvVarName,
      runtimeDeliveryProfileEnvValues.balanced,
    ),
  ]);

  assert.equal(isOk(config), true);
  if (isOk(config)) {
    assert.equal(
      config.value.runtimeDeliveryProfile,
      RuntimeDeliveryProfile.Balanced,
    );
  }
});

test("runtime config rejects invalid delivery profile values", () => {
  const config = ObserverRuntimeConfig.fromEnvironment({
    [runtimeDeliveryProfileEnvVarName]: "fastest",
  });

  assert.equal(isErr(config), true);
  if (isErr(config)) {
    assert.equal(
      config.error.kind,
      ValidationErrorKind.InvalidRuntimeDeliveryProfile,
    );
    assert.equal(config.error.field, runtimeDeliveryProfileEnvVarName);
    assert.equal(config.error.received, "fastest");
    assert.deepEqual(
      config.error.allowedValues,
      runtimeDeliveryProfileAllowedValues,
    );
  }
});
