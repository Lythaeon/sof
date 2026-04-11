import assert from "node:assert/strict";
import test from "node:test";

import {
  environmentVariable,
  environmentVariablesToRecord,
} from "../environment.js";
import { ValidationErrorKind } from "../errors.js";
import { isErr, isOk, ResultTag } from "../result.js";
import {
  defaultDerivedStateReplayDirectory,
  derivedStateCheckpointIntervalEnvVarName,
  DerivedStateReplayBackend,
  derivedStateReplayBackendAllowedValues,
  derivedStateReplayBackendEnvValues,
  derivedStateReplayBackendEnvVarName,
  derivedStateReplayBackendToEnvValue,
  derivedStateReplayDirectory,
  derivedStateReplayDirEnvVarName,
  derivedStateRecoveryIntervalEnvVarName,
  DerivedStateReplayConfig,
  DerivedStateReplayDurability,
  derivedStateReplayDurabilityAllowedValues,
  derivedStateReplayDurabilityEnvValues,
  derivedStateReplayDurabilityEnvVarName,
  derivedStateReplayDurabilityToEnvValue,
  derivedStateReplayMaxEnvelopesEnvVarName,
  derivedStateReplayMaxSessionsEnvVarName,
  DerivedStateRuntimeConfig,
  derivedStateReplayConfig,
  derivedStateRuntimeConfig,
  nonNegativeIntegerToEnvValue,
  observerRuntimeConfig,
  observerRuntimeConfigForProfile,
  parseDerivedStateReplayBackend,
  parseDerivedStateReplayDurability,
  parseNonNegativeInteger,
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
  runtimeDeliveryProfileEnvDefaults,
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
  assert.equal(DerivedStateReplayBackend.Memory, 1);
  assert.equal(DerivedStateReplayBackend.Disk, 2);
  assert.equal(DerivedStateReplayDurability.Flush, 1);
  assert.equal(DerivedStateReplayDurability.Fsync, 2);
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

test("runtime delivery profiles expose the expected env-backed defaults", () => {
  assert.deepEqual(
    runtimeDeliveryProfileEnvDefaults(RuntimeDeliveryProfile.LatencyOptimized),
    {
      derivedStateReplayMaxEnvelopes: 8192,
      derivedStateReplayMaxSessions: 4,
    },
  );
  assert.deepEqual(
    runtimeDeliveryProfileEnvDefaults(RuntimeDeliveryProfile.Balanced),
    {
      derivedStateReplayMaxEnvelopes: 16384,
      derivedStateReplayMaxSessions: 6,
    },
  );
  assert.deepEqual(
    runtimeDeliveryProfileEnvDefaults(
      RuntimeDeliveryProfile.DeliveryDisciplined,
    ),
    {
      derivedStateReplayMaxEnvelopes: 32768,
      derivedStateReplayMaxSessions: 8,
    },
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
  assert.equal(
    derivedStateReplayBackendToEnvValue(DerivedStateReplayBackend.Memory),
    derivedStateReplayBackendEnvValues.memory,
  );
  assert.equal(
    derivedStateReplayBackendToEnvValue(DerivedStateReplayBackend.Disk),
    derivedStateReplayBackendEnvValues.disk,
  );
  assert.equal(
    derivedStateReplayDurabilityToEnvValue(DerivedStateReplayDurability.Flush),
    derivedStateReplayDurabilityEnvValues.flush,
  );
  assert.equal(
    derivedStateReplayDurabilityToEnvValue(DerivedStateReplayDurability.Fsync),
    derivedStateReplayDurabilityEnvValues.fsync,
  );
  assert.equal(nonNegativeIntegerToEnvValue(8192), "8192");
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
  const replayBackend = parseDerivedStateReplayBackend("DISK");
  const replayDurability = parseDerivedStateReplayDurability(" flush ");
  const nonNegativeInteger = parseNonNegativeInteger(
    "8192",
    derivedStateReplayMaxEnvelopesEnvVarName,
  );

  assert.equal(isOk(trusted), true);
  assert.equal(isOk(strict), true);
  assert.equal(isOk(allowEof), true);
  assert.equal(isOk(replayBackend), true);
  assert.equal(isOk(replayDurability), true);
  assert.equal(isOk(nonNegativeInteger), true);

  if (isOk(trusted)) {
    assert.equal(trusted.value, ShredTrustMode.TrustedRawShredProvider);
  }
  if (isOk(strict)) {
    assert.equal(strict.value, ProviderStreamCapabilityPolicy.Strict);
  }
  if (isOk(allowEof)) {
    assert.equal(allowEof.value, true);
  }
  if (isOk(replayBackend)) {
    assert.equal(replayBackend.value, DerivedStateReplayBackend.Disk);
  }
  if (isOk(replayDurability)) {
    assert.equal(replayDurability.value, DerivedStateReplayDurability.Flush);
  }
  if (isOk(nonNegativeInteger)) {
    assert.equal(nonNegativeInteger.value, 8192);
  }
});

test("runtime config omits default policy values unless requested", () => {
  const config = new ObserverRuntimeConfig();
  const envRecord = config.toEnvironmentRecord({ includeDefaults: true });

  assert.deepEqual(config.toEnvironment(), []);
  assert.equal(Object.getPrototypeOf(envRecord), null);
  assert.deepEqual({ ...envRecord }, {
    [runtimeDeliveryProfileEnvVarName]:
      runtimeDeliveryProfileEnvValues.latencyOptimized,
    [shredTrustModeEnvVarName]: shredTrustModeEnvValues.publicUntrusted,
    [providerStreamCapabilityPolicyEnvVarName]:
      providerStreamCapabilityPolicyEnvValues.warn,
    [providerStreamAllowEofEnvVarName]: runtimeBooleanEnvValues.false,
    [derivedStateCheckpointIntervalEnvVarName]: "30000",
    [derivedStateRecoveryIntervalEnvVarName]: "5000",
    [derivedStateReplayBackendEnvVarName]:
      derivedStateReplayBackendEnvValues.memory,
    [derivedStateReplayDirEnvVarName]: defaultDerivedStateReplayDirectory,
    [derivedStateReplayDurabilityEnvVarName]:
      derivedStateReplayDurabilityEnvValues.flush,
    [derivedStateReplayMaxEnvelopesEnvVarName]: "8192",
    [derivedStateReplayMaxSessionsEnvVarName]: "4",
  });
});

test("runtime config serializes explicit runtime policy selection", () => {
  const config = new ObserverRuntimeConfig({
    runtimeDeliveryProfile: RuntimeDeliveryProfile.Balanced,
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
    environmentVariable(derivedStateCheckpointIntervalEnvVarName, "60000"),
    environmentVariable(derivedStateRecoveryIntervalEnvVarName, "10000"),
    environmentVariable(
      derivedStateReplayBackendEnvVarName,
      derivedStateReplayBackendEnvValues.disk,
    ),
    environmentVariable(
      derivedStateReplayDirEnvVarName,
      derivedStateReplayDirectory(".sof-replay"),
    ),
    environmentVariable(
      derivedStateReplayDurabilityEnvVarName,
      derivedStateReplayDurabilityEnvValues.fsync,
    ),
    environmentVariable(derivedStateReplayMaxEnvelopesEnvVarName, "1024"),
    environmentVariable(derivedStateReplayMaxSessionsEnvVarName, "2"),
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
    [derivedStateCheckpointIntervalEnvVarName]: "45000",
    [derivedStateRecoveryIntervalEnvVarName]: "7000",
    [derivedStateReplayBackendEnvVarName]: "disk",
    [derivedStateReplayDirEnvVarName]: ".sof-disk-tail",
    [derivedStateReplayDurabilityEnvVarName]: "fsync",
    [derivedStateReplayMaxEnvelopesEnvVarName]: "2048",
    [derivedStateReplayMaxSessionsEnvVarName]: "6",
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
    assert.equal(config.value.derivedState.checkpointIntervalMs, 45_000);
    assert.equal(config.value.derivedState.recoveryIntervalMs, 7_000);
    assert.equal(
      config.value.derivedState.replay.backend,
      DerivedStateReplayBackend.Disk,
    );
    assert.equal(
      config.value.derivedState.replay.replayDirectory,
      derivedStateReplayDirectory(".sof-disk-tail"),
    );
    assert.equal(
      config.value.derivedState.replay.durability,
      DerivedStateReplayDurability.Fsync,
    );
    assert.equal(config.value.derivedState.replay.maxEnvelopes, 2048);
    assert.equal(config.value.derivedState.replay.maxSessions, 6);
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
    environmentVariable(
      derivedStateCheckpointIntervalEnvVarName,
      nonNegativeIntegerToEnvValue(30_000),
    ),
    environmentVariable(
      derivedStateRecoveryIntervalEnvVarName,
      nonNegativeIntegerToEnvValue(5_000),
    ),
    environmentVariable(
      derivedStateReplayBackendEnvVarName,
      derivedStateReplayBackendEnvValues.memory,
    ),
    environmentVariable(
      derivedStateReplayDirEnvVarName,
      defaultDerivedStateReplayDirectory,
    ),
    environmentVariable(
      derivedStateReplayDurabilityEnvVarName,
      derivedStateReplayDurabilityEnvValues.flush,
    ),
    environmentVariable(
      derivedStateReplayMaxEnvelopesEnvVarName,
      nonNegativeIntegerToEnvValue(8_192),
    ),
    environmentVariable(
      derivedStateReplayMaxSessionsEnvVarName,
      nonNegativeIntegerToEnvValue(4),
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
    assert.equal(
      config.value.derivedState.replay.backend,
      DerivedStateReplayBackend.Memory,
    );
  }
});

test("runtime config preset helpers create one-line common profiles", () => {
  const latency = ObserverRuntimeConfig.latencyOptimized();
  const balanced = ObserverRuntimeConfig.balanced({
    providerStreamAllowEof: true,
  });
  const disciplined = ObserverRuntimeConfig.deliveryDisciplined({
    shredTrustMode: ShredTrustMode.TrustedRawShredProvider,
  });
  const explicit = ObserverRuntimeConfig.forProfile(
    RuntimeDeliveryProfile.Balanced,
    {
      providerStreamCapabilityPolicy: ProviderStreamCapabilityPolicy.Strict,
    },
  );
  const functionPreset = observerRuntimeConfigForProfile(
    RuntimeDeliveryProfile.DeliveryDisciplined,
  );

  assert.equal(
    latency.runtimeDeliveryProfile,
    RuntimeDeliveryProfile.LatencyOptimized,
  );
  assert.equal(balanced.runtimeDeliveryProfile, RuntimeDeliveryProfile.Balanced);
  assert.equal(balanced.providerStreamAllowEof, true);
  assert.equal(
    disciplined.runtimeDeliveryProfile,
    RuntimeDeliveryProfile.DeliveryDisciplined,
  );
  assert.equal(
    disciplined.shredTrustMode,
    ShredTrustMode.TrustedRawShredProvider,
  );
  assert.equal(explicit.runtimeDeliveryProfile, RuntimeDeliveryProfile.Balanced);
  assert.equal(
    explicit.providerStreamCapabilityPolicy,
    ProviderStreamCapabilityPolicy.Strict,
  );
  assert.equal(
    functionPreset.runtimeDeliveryProfile,
    RuntimeDeliveryProfile.DeliveryDisciplined,
  );
  assert.equal(balanced.derivedState.replay.maxEnvelopes, 16_384);
  assert.equal(balanced.derivedState.replay.maxSessions, 6);
  assert.equal(functionPreset.derivedState.replay.maxEnvelopes, 32_768);
  assert.equal(functionPreset.derivedState.replay.maxSessions, 8);
});

test("runtime profile helpers preserve explicit derived-state replay overrides", () => {
  const config = ObserverRuntimeConfig.deliveryDisciplined({
    derivedState: {
      replay: {
        maxEnvelopes: 512,
      },
    },
  });

  assert.equal(
    config.derivedState.replay.maxEnvelopes,
    512,
  );
  assert.equal(
    config.derivedState.replay.maxSessions,
    8,
  );
});

test("runtime config supports nested plain-object construction", () => {
  const config = new ObserverRuntimeConfig({
    runtimeDeliveryProfile: RuntimeDeliveryProfile.Balanced,
    derivedState: {
      checkpointIntervalMs: 15_000,
      replay: {
        backend: DerivedStateReplayBackend.Disk,
        replayDirectory: ".sof-plain-object",
        durability: DerivedStateReplayDurability.Fsync,
        maxEnvelopes: 256,
        maxSessions: 3,
      },
    },
  });

  assert.equal(config.derivedState.checkpointIntervalMs, 15_000);
  assert.equal(config.derivedState.replay.backend, DerivedStateReplayBackend.Disk);
  assert.equal(
    config.derivedState.replay.replayDirectory,
    derivedStateReplayDirectory(".sof-plain-object"),
  );
  assert.equal(config.derivedState.replay.maxEnvelopes, 256);
  assert.equal(config.derivedState.replay.maxSessions, 3);
});

test("derived-state factory helpers reduce nested constructor ceremony", () => {
  const replay = DerivedStateReplayConfig.disk({
    replayDirectory: ".sof-disk",
    durability: DerivedStateReplayDurability.Fsync,
    maxEnvelopes: 512,
  });
  const runtime = DerivedStateRuntimeConfig.checkpointOnly({
    checkpointIntervalMs: 20_000,
  });
  const replayFromFunction = derivedStateReplayConfig({
    backend: DerivedStateReplayBackend.Memory,
    maxEnvelopes: 128,
  });
  const runtimeFromFunction = derivedStateRuntimeConfig({
    replay: {
      backend: DerivedStateReplayBackend.Disk,
      replayDirectory: ".sof-function",
    },
  });
  const observer = observerRuntimeConfig({
    derivedState: runtimeFromFunction,
  });

  assert.equal(replay.backend, DerivedStateReplayBackend.Disk);
  assert.equal(replay.replayDirectory, derivedStateReplayDirectory(".sof-disk"));
  assert.equal(replay.maxEnvelopes, 512);
  assert.equal(runtime.replay.isEnabled(), false);
  assert.equal(runtime.checkpointIntervalMs, 20_000);
  assert.equal(replayFromFunction.maxEnvelopes, 128);
  assert.equal(
    runtimeFromFunction.replay.replayDirectory,
    derivedStateReplayDirectory(".sof-function"),
  );
  assert.equal(
    observer.derivedState.replay.replayDirectory,
    derivedStateReplayDirectory(".sof-function"),
  );
});

test("environment helpers ignore inherited env values and use a null-prototype record", () => {
  const inherited = Object.create({
    [runtimeDeliveryProfileEnvVarName]: runtimeDeliveryProfileEnvValues.balanced,
  }) as Record<string, string | undefined>;

  const parsed = ObserverRuntimeConfig.fromEnvironmentRecord(inherited);

  assert.equal(isOk(parsed), true);
  if (isOk(parsed)) {
    assert.equal(
      parsed.value.runtimeDeliveryProfile,
      RuntimeDeliveryProfile.LatencyOptimized,
    );
  }

  const record = environmentVariablesToRecord([
    environmentVariable(
      runtimeDeliveryProfileEnvVarName,
      runtimeDeliveryProfileEnvValues.balanced,
    ),
  ]);

  assert.equal(Object.getPrototypeOf(record), null);
  assert.equal(record[runtimeDeliveryProfileEnvVarName], "balanced");
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

test("runtime config rejects invalid derived-state replay backend values", () => {
  const config = ObserverRuntimeConfig.fromEnvironment({
    [derivedStateReplayBackendEnvVarName]: "remote",
  });

  assert.equal(isErr(config), true);
  if (isErr(config)) {
    assert.equal(
      config.error.kind,
      ValidationErrorKind.InvalidDerivedStateReplayBackend,
    );
    assert.equal(config.error.field, derivedStateReplayBackendEnvVarName);
    assert.deepEqual(
      config.error.allowedValues,
      derivedStateReplayBackendAllowedValues,
    );
  }
});

test("runtime config rejects invalid derived-state replay durability values", () => {
  const config = ObserverRuntimeConfig.fromEnvironment({
    [derivedStateReplayDurabilityEnvVarName]: "sync",
  });

  assert.equal(isErr(config), true);
  if (isErr(config)) {
    assert.equal(
      config.error.kind,
      ValidationErrorKind.InvalidDerivedStateReplayDurability,
    );
    assert.equal(config.error.field, derivedStateReplayDurabilityEnvVarName);
    assert.deepEqual(
      config.error.allowedValues,
      derivedStateReplayDurabilityAllowedValues,
    );
  }
});

test("runtime config rejects invalid derived-state numeric values", () => {
  const config = ObserverRuntimeConfig.fromEnvironment({
    [derivedStateReplayMaxEnvelopesEnvVarName]: "-1",
  });

  assert.equal(isErr(config), true);
  if (isErr(config)) {
    assert.equal(config.error.kind, ValidationErrorKind.InvalidNonNegativeInteger);
    assert.equal(config.error.field, derivedStateReplayMaxEnvelopesEnvVarName);
  }
});

test("derived-state replay config exposes checkpoint-only helper", () => {
  const replay = DerivedStateReplayConfig.checkpointOnly();

  assert.equal(replay.maxEnvelopes, 0);
  assert.equal(replay.maxSessions, 0);
  assert.equal(replay.isEnabled(), false);
});

test("derived-state configs reject invalid programmatic numeric values", () => {
  assert.throws(
    () =>
      new DerivedStateRuntimeConfig({
        checkpointIntervalMs: -1,
      }),
    /checkpointIntervalMs must be a non-negative integer/,
  );
  assert.throws(
    () =>
      new DerivedStateReplayConfig({
        maxSessions: -1,
      }),
    /maxSessions must be a non-negative integer/,
  );
  assert.throws(
    () => nonNegativeIntegerToEnvValue(-1),
    /value must be a non-negative integer/,
  );
});

test("runtime config helpers reject invalid programmatic enum and path values", () => {
  assert.throws(
    () => runtimeDeliveryProfileToEnvValue(99 as RuntimeDeliveryProfile),
    /unknown runtime delivery profile/,
  );
  assert.throws(
    () => shredTrustModeToEnvValue(99 as ShredTrustMode),
    /unknown shred trust mode/,
  );
  assert.throws(
    () =>
      providerStreamCapabilityPolicyToEnvValue(
        99 as ProviderStreamCapabilityPolicy,
      ),
    /unknown provider stream capability policy/,
  );
  assert.throws(
    () =>
      derivedStateReplayBackendToEnvValue(99 as DerivedStateReplayBackend),
    /unknown derived-state replay backend/,
  );
  assert.throws(
    () =>
      derivedStateReplayDurabilityToEnvValue(
        99 as DerivedStateReplayDurability,
      ),
    /unknown derived-state replay durability/,
  );
  assert.throws(
    () => derivedStateReplayDirectory("   "),
    /replayDirectory must not be empty/,
  );
  assert.throws(
    () =>
      new ObserverRuntimeConfig({
        providerStreamAllowEof: "true" as unknown as boolean,
      }),
    /providerStreamAllowEof must be a boolean/,
  );
});
