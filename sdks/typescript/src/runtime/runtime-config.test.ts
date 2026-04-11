import assert from "node:assert/strict";
import test from "node:test";

import { environmentVariable } from "../environment.js";
import { ValidationErrorKind } from "../errors.js";
import { isErr, isOk, ResultTag } from "../result.js";
import {
  ObserverRuntimeConfig,
  ProviderStreamCapabilityPolicy,
  providerStreamAllowEofEnvVarName,
  providerStreamCapabilityPolicyAllowedValues,
  providerStreamCapabilityPolicyEnvValues,
  providerStreamCapabilityPolicyEnvVarName,
  providerStreamCapabilityPolicyToEnvValue,
  parseProviderStreamCapabilityPolicy,
  parseRuntimeBoolean,
  parseShredTrustMode,
  RuntimeDeliveryProfile,
  runtimeBooleanAllowedValues,
  runtimeBooleanEnvValues,
  parseRuntimeDeliveryProfile,
  shredTrustModeAllowedValues,
  shredTrustModeEnvValues,
  shredTrustModeEnvVarName,
  shredTrustModeToEnvValue,
  ShredTrustMode,
  runtimeDeliveryProfileAllowedValues,
  runtimeDeliveryProfileEnvValues,
  runtimeDeliveryProfileEnvVarName,
  runtimeDeliveryProfileToEnvValue,
} from "../runtime.js";

test("result and runtime policy discriminants stay stable", () => {
  assert.equal(ResultTag.Ok, 1);
  assert.equal(ResultTag.Err, 2);
  assert.equal(RuntimeDeliveryProfile.LatencyOptimized, 1);
  assert.equal(RuntimeDeliveryProfile.Balanced, 2);
  assert.equal(RuntimeDeliveryProfile.DeliveryDisciplined, 3);
  assert.equal(ShredTrustMode.PublicUntrusted, 1);
  assert.equal(ShredTrustMode.TrustedRawShredProvider, 2);
  assert.equal(ProviderStreamCapabilityPolicy.Warn, 1);
  assert.equal(ProviderStreamCapabilityPolicy.Strict, 2);
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

test("runtime policy enums map to the documented env values", () => {
  assert.equal(
    shredTrustModeToEnvValue(ShredTrustMode.PublicUntrusted),
    shredTrustModeEnvValues.publicUntrusted,
  );
  assert.equal(
    shredTrustModeToEnvValue(ShredTrustMode.TrustedRawShredProvider),
    shredTrustModeEnvValues.trustedRawShredProvider,
  );
  assert.equal(
    providerStreamCapabilityPolicyToEnvValue(
      ProviderStreamCapabilityPolicy.Warn,
    ),
    providerStreamCapabilityPolicyEnvValues.warn,
  );
  assert.equal(
    providerStreamCapabilityPolicyToEnvValue(
      ProviderStreamCapabilityPolicy.Strict,
    ),
    providerStreamCapabilityPolicyEnvValues.strict,
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

test("runtime policy parsers accept documented aliases", () => {
  const trusted = parseShredTrustMode("trusted-raw-shred-provider");
  const strict = parseProviderStreamCapabilityPolicy(" STRICT ");
  const allowEof = parseRuntimeBoolean("YES", providerStreamAllowEofEnvVarName);

  assert.equal(isOk(trusted), true);
  assert.equal(isOk(strict), true);
  assert.equal(isOk(allowEof), true);

  if (isOk(trusted)) {
    assert.equal(trusted.value, ShredTrustMode.TrustedRawShredProvider);
  }
  if (isOk(strict)) {
    assert.equal(strict.value, ProviderStreamCapabilityPolicy.Strict);
  }
  if (isOk(allowEof)) {
    assert.equal(allowEof.value, true);
  }
});

test("runtime config omits default policy values unless requested", () => {
  const config = new ObserverRuntimeConfig();

  assert.deepEqual(config.toEnvironment(), []);
  assert.deepEqual(config.toEnvironmentRecord({ includeDefaults: true }), {
    [runtimeDeliveryProfileEnvVarName]:
      runtimeDeliveryProfileEnvValues.latencyOptimized,
    [shredTrustModeEnvVarName]: shredTrustModeEnvValues.publicUntrusted,
    [providerStreamCapabilityPolicyEnvVarName]:
      providerStreamCapabilityPolicyEnvValues.warn,
    [providerStreamAllowEofEnvVarName]: runtimeBooleanEnvValues.false,
  });
});

test("runtime config serializes explicit runtime policy selection", () => {
  const config = new ObserverRuntimeConfig({
    runtimeDeliveryProfile: RuntimeDeliveryProfile.Balanced,
    shredTrustMode: ShredTrustMode.TrustedRawShredProvider,
    providerStreamCapabilityPolicy: ProviderStreamCapabilityPolicy.Strict,
    providerStreamAllowEof: true,
  });

  assert.deepEqual(config.toEnvironment(), [
    environmentVariable(
      runtimeDeliveryProfileEnvVarName,
      runtimeDeliveryProfileEnvValues.balanced,
    ),
    environmentVariable(
      shredTrustModeEnvVarName,
      shredTrustModeEnvValues.trustedRawShredProvider,
    ),
    environmentVariable(
      providerStreamCapabilityPolicyEnvVarName,
      providerStreamCapabilityPolicyEnvValues.strict,
    ),
    environmentVariable(
      providerStreamAllowEofEnvVarName,
      runtimeBooleanEnvValues.true,
    ),
  ]);
});

test("runtime config parses environment values into typed runtime policy config", () => {
  const config = ObserverRuntimeConfig.fromEnvironment({
    [runtimeDeliveryProfileEnvVarName]:
      runtimeDeliveryProfileEnvValues.deliveryDisciplined,
    [shredTrustModeEnvVarName]: shredTrustModeEnvValues.trustedRawShredProvider,
    [providerStreamCapabilityPolicyEnvVarName]:
      providerStreamCapabilityPolicyEnvValues.strict,
    [providerStreamAllowEofEnvVarName]: "on",
  });

  assert.equal(isOk(config), true);
  if (isOk(config)) {
    assert.equal(
      config.value.runtimeDeliveryProfile,
      RuntimeDeliveryProfile.DeliveryDisciplined,
    );
    assert.equal(
      config.value.shredTrustMode,
      ShredTrustMode.TrustedRawShredProvider,
    );
    assert.equal(
      config.value.providerStreamCapabilityPolicy,
      ProviderStreamCapabilityPolicy.Strict,
    );
    assert.equal(config.value.providerStreamAllowEof, true);
  }
});

test("runtime config parses typed environment variables into typed config", () => {
  const config = ObserverRuntimeConfig.fromEnvironmentVariables([
    environmentVariable(
      runtimeDeliveryProfileEnvVarName,
      runtimeDeliveryProfileEnvValues.balanced,
    ),
    environmentVariable(
      shredTrustModeEnvVarName,
      shredTrustModeEnvValues.publicUntrusted,
    ),
    environmentVariable(
      providerStreamCapabilityPolicyEnvVarName,
      providerStreamCapabilityPolicyEnvValues.warn,
    ),
    environmentVariable(
      providerStreamAllowEofEnvVarName,
      runtimeBooleanEnvValues.false,
    ),
  ]);

  assert.equal(isOk(config), true);
  if (isOk(config)) {
    assert.equal(
      config.value.runtimeDeliveryProfile,
      RuntimeDeliveryProfile.Balanced,
    );
    assert.equal(config.value.shredTrustMode, ShredTrustMode.PublicUntrusted);
    assert.equal(
      config.value.providerStreamCapabilityPolicy,
      ProviderStreamCapabilityPolicy.Warn,
    );
    assert.equal(config.value.providerStreamAllowEof, false);
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

test("runtime config rejects invalid shred trust mode values", () => {
  const config = ObserverRuntimeConfig.fromEnvironment({
    [shredTrustModeEnvVarName]: "trusted",
  });

  assert.equal(isErr(config), true);
  if (isErr(config)) {
    assert.equal(config.error.kind, ValidationErrorKind.InvalidShredTrustMode);
    assert.equal(config.error.field, shredTrustModeEnvVarName);
    assert.deepEqual(config.error.allowedValues, shredTrustModeAllowedValues);
  }
});

test("runtime config rejects invalid provider stream capability policy values", () => {
  const config = ObserverRuntimeConfig.fromEnvironment({
    [providerStreamCapabilityPolicyEnvVarName]: "fail-fast",
  });

  assert.equal(isErr(config), true);
  if (isErr(config)) {
    assert.equal(
      config.error.kind,
      ValidationErrorKind.InvalidProviderStreamCapabilityPolicy,
    );
    assert.equal(config.error.field, providerStreamCapabilityPolicyEnvVarName);
    assert.deepEqual(
      config.error.allowedValues,
      providerStreamCapabilityPolicyAllowedValues,
    );
  }
});

test("runtime config rejects invalid provider stream eof values", () => {
  const config = ObserverRuntimeConfig.fromEnvironment({
    [providerStreamAllowEofEnvVarName]: "sometimes",
  });

  assert.equal(isErr(config), true);
  if (isErr(config)) {
    assert.equal(
      config.error.kind,
      ValidationErrorKind.InvalidProviderStreamAllowEof,
    );
    assert.equal(config.error.field, providerStreamAllowEofEnvVarName);
    assert.deepEqual(config.error.allowedValues, runtimeBooleanAllowedValues);
  }
});
