import {
  environmentVariable,
  environmentVariablesToRecord,
  readEnvironmentVariable,
  type EnvironmentInput,
  type EnvironmentVariable,
} from "../environment.js";
import type { ValidationError } from "../errors.js";
import { isErr, ok, type Result } from "../result.js";
import {
  parseRuntimeDeliveryProfile,
  RuntimeDeliveryProfile,
  type RuntimeDeliveryProfileEnvValue,
  runtimeDeliveryProfileEnvVarName,
  runtimeDeliveryProfileToEnvValue,
} from "./runtime-delivery-profile.js";
import {
  defaultDerivedStateReplayDirectory,
  derivedStateCheckpointIntervalEnvVarName,
  derivedStateRecoveryIntervalEnvVarName,
  DerivedStateReplayBackend,
  derivedStateReplayBackendEnvVarName,
  derivedStateReplayBackendToEnvValue,
  derivedStateReplayDirEnvVarName,
  DerivedStateReplayDurability,
  derivedStateReplayDurabilityEnvVarName,
  derivedStateReplayDurabilityToEnvValue,
  derivedStateReplayMaxEnvelopesEnvVarName,
  derivedStateReplayMaxSessionsEnvVarName,
  DerivedStateReplayConfig,
  type DerivedStateReplayBackendEnvValue,
  type DerivedStateReplayDirectory,
  type DerivedStateReplayDurabilityEnvValue,
  DerivedStateRuntimeConfig,
  type NonNegativeIntegerEnvValue,
  nonNegativeIntegerToEnvValue,
  parseDerivedStateReplayBackend,
  parseDerivedStateReplayDurability,
  parseNonNegativeInteger,
  derivedStateReplayDirectory,
} from "./derived-state.js";
import {
  parseProviderStreamCapabilityPolicy,
  parseRuntimeBoolean,
  parseShredTrustMode,
  ProviderStreamCapabilityPolicy,
  providerStreamAllowEofEnvVarName,
  providerStreamCapabilityPolicyEnvVarName,
  providerStreamCapabilityPolicyToEnvValue,
  type ProviderStreamCapabilityPolicyEnvValue,
  runtimeBooleanToEnvValue,
  type RuntimeBooleanEnvValue,
  ShredTrustMode,
  shredTrustModeEnvVarName,
  shredTrustModeToEnvValue,
  type ShredTrustModeEnvValue,
} from "./runtime-policy.js";

export interface ObserverRuntimeConfigInit {
  readonly runtimeDeliveryProfile?: RuntimeDeliveryProfile;
  readonly shredTrustMode?: ShredTrustMode;
  readonly providerStreamCapabilityPolicy?: ProviderStreamCapabilityPolicy;
  readonly providerStreamAllowEof?: boolean;
  readonly derivedState?: DerivedStateRuntimeConfig;
}

export interface ObserverRuntimeEnvironmentOptions {
  readonly includeDefaults?: boolean;
}

export type ObserverRuntimeEnvironmentVariable =
  | EnvironmentVariable<
      typeof runtimeDeliveryProfileEnvVarName,
      RuntimeDeliveryProfileEnvValue
    >
  | EnvironmentVariable<typeof shredTrustModeEnvVarName, ShredTrustModeEnvValue>
  | EnvironmentVariable<
      typeof providerStreamCapabilityPolicyEnvVarName,
      ProviderStreamCapabilityPolicyEnvValue
    >
  | EnvironmentVariable<
      typeof providerStreamAllowEofEnvVarName,
      RuntimeBooleanEnvValue
    >
  | EnvironmentVariable<
      typeof derivedStateCheckpointIntervalEnvVarName,
      NonNegativeIntegerEnvValue
    >
  | EnvironmentVariable<
      typeof derivedStateRecoveryIntervalEnvVarName,
      NonNegativeIntegerEnvValue
    >
  | EnvironmentVariable<
      typeof derivedStateReplayBackendEnvVarName,
      DerivedStateReplayBackendEnvValue
    >
  | EnvironmentVariable<typeof derivedStateReplayDirEnvVarName, DerivedStateReplayDirectory>
  | EnvironmentVariable<
      typeof derivedStateReplayDurabilityEnvVarName,
      DerivedStateReplayDurabilityEnvValue
    >
  | EnvironmentVariable<
      typeof derivedStateReplayMaxEnvelopesEnvVarName,
      NonNegativeIntegerEnvValue
    >
  | EnvironmentVariable<
      typeof derivedStateReplayMaxSessionsEnvVarName,
      NonNegativeIntegerEnvValue
    >;

export type ObserverRuntimeValidationError =
  | ValidationError<RuntimeDeliveryProfileEnvValue>
  | ValidationError<ShredTrustModeEnvValue>
  | ValidationError<ProviderStreamCapabilityPolicyEnvValue>
  | ValidationError<RuntimeBooleanEnvValue>
  | ValidationError<DerivedStateReplayBackendEnvValue>
  | ValidationError<DerivedStateReplayDurabilityEnvValue>
  | ValidationError;

export class ObserverRuntimeConfig {
  readonly runtimeDeliveryProfile: RuntimeDeliveryProfile;
  readonly shredTrustMode: ShredTrustMode;
  readonly providerStreamCapabilityPolicy: ProviderStreamCapabilityPolicy;
  readonly providerStreamAllowEof: boolean;
  readonly derivedState: DerivedStateRuntimeConfig;

  constructor(init: ObserverRuntimeConfigInit = {}) {
    this.runtimeDeliveryProfile =
      init.runtimeDeliveryProfile ?? RuntimeDeliveryProfile.LatencyOptimized;
    this.shredTrustMode = init.shredTrustMode ?? ShredTrustMode.PublicUntrusted;
    this.providerStreamCapabilityPolicy =
      init.providerStreamCapabilityPolicy ?? ProviderStreamCapabilityPolicy.Warn;
    this.providerStreamAllowEof = init.providerStreamAllowEof ?? false;
    this.derivedState = init.derivedState ?? new DerivedStateRuntimeConfig();
  }

  toEnvironment(
    options: ObserverRuntimeEnvironmentOptions = {},
  ): readonly ObserverRuntimeEnvironmentVariable[] {
    const environment: ObserverRuntimeEnvironmentVariable[] = [];

    if (
      options.includeDefaults !== true &&
      this.runtimeDeliveryProfile === RuntimeDeliveryProfile.LatencyOptimized
    ) {} else {
      environment.push(
        environmentVariable(
          runtimeDeliveryProfileEnvVarName,
          runtimeDeliveryProfileToEnvValue(this.runtimeDeliveryProfile),
        ),
      );
    }

    if (
      options.includeDefaults !== true &&
      this.shredTrustMode === ShredTrustMode.PublicUntrusted
    ) {} else {
      environment.push(
        environmentVariable(
          shredTrustModeEnvVarName,
          shredTrustModeToEnvValue(this.shredTrustMode),
        ),
      );
    }

    if (
      options.includeDefaults !== true &&
      this.providerStreamCapabilityPolicy === ProviderStreamCapabilityPolicy.Warn
    ) {} else {
      environment.push(
        environmentVariable(
          providerStreamCapabilityPolicyEnvVarName,
          providerStreamCapabilityPolicyToEnvValue(
            this.providerStreamCapabilityPolicy,
          ),
        ),
      );
    }

    if (options.includeDefaults === true || this.providerStreamAllowEof) {
      environment.push(
        environmentVariable(
          providerStreamAllowEofEnvVarName,
          runtimeBooleanToEnvValue(this.providerStreamAllowEof),
        ),
      );
    }

    if (
      options.includeDefaults === true ||
      this.derivedState.checkpointIntervalMs !== 30_000
    ) {
      environment.push(
        environmentVariable(
          derivedStateCheckpointIntervalEnvVarName,
          nonNegativeIntegerToEnvValue(this.derivedState.checkpointIntervalMs),
        ),
      );
    }

    if (
      options.includeDefaults === true ||
      this.derivedState.recoveryIntervalMs !== 5_000
    ) {
      environment.push(
        environmentVariable(
          derivedStateRecoveryIntervalEnvVarName,
          nonNegativeIntegerToEnvValue(this.derivedState.recoveryIntervalMs),
        ),
      );
    }

    if (
      options.includeDefaults === true ||
      this.derivedState.replay.backend !== DerivedStateReplayBackend.Memory
    ) {
      environment.push(
        environmentVariable(
          derivedStateReplayBackendEnvVarName,
          derivedStateReplayBackendToEnvValue(this.derivedState.replay.backend),
        ),
      );
    }

    if (
      options.includeDefaults === true ||
      this.derivedState.replay.replayDirectory !==
        defaultDerivedStateReplayDirectory
    ) {
      environment.push(
        environmentVariable(
          derivedStateReplayDirEnvVarName,
          this.derivedState.replay.replayDirectory,
        ),
      );
    }

    if (
      options.includeDefaults === true ||
      this.derivedState.replay.durability !== DerivedStateReplayDurability.Flush
    ) {
      environment.push(
        environmentVariable(
          derivedStateReplayDurabilityEnvVarName,
          derivedStateReplayDurabilityToEnvValue(
            this.derivedState.replay.durability,
          ),
        ),
      );
    }

    if (
      options.includeDefaults === true ||
      this.derivedState.replay.maxEnvelopes !== 8_192
    ) {
      environment.push(
        environmentVariable(
          derivedStateReplayMaxEnvelopesEnvVarName,
          nonNegativeIntegerToEnvValue(this.derivedState.replay.maxEnvelopes),
        ),
      );
    }

    if (
      options.includeDefaults === true ||
      this.derivedState.replay.maxSessions !== 4
    ) {
      environment.push(
        environmentVariable(
          derivedStateReplayMaxSessionsEnvVarName,
          nonNegativeIntegerToEnvValue(this.derivedState.replay.maxSessions),
        ),
      );
    }

    return environment;
  }

  toEnvironmentRecord(
    options: ObserverRuntimeEnvironmentOptions = {},
  ): Readonly<Record<string, string>> {
    return environmentVariablesToRecord(this.toEnvironment(options));
  }

  static fromEnvironment(
    env: EnvironmentInput,
  ): Result<ObserverRuntimeConfig, ObserverRuntimeValidationError> {
    const runtimeDeliveryProfile = readEnvironmentVariable(
      env,
      runtimeDeliveryProfileEnvVarName,
    );
    const shredTrustMode = readEnvironmentVariable(env, shredTrustModeEnvVarName);
    const providerStreamCapabilityPolicy = readEnvironmentVariable(
      env,
      providerStreamCapabilityPolicyEnvVarName,
    );
    const providerStreamAllowEof = readEnvironmentVariable(
      env,
      providerStreamAllowEofEnvVarName,
    );
    const derivedStateCheckpointInterval = readEnvironmentVariable(
      env,
      derivedStateCheckpointIntervalEnvVarName,
    );
    const derivedStateRecoveryInterval = readEnvironmentVariable(
      env,
      derivedStateRecoveryIntervalEnvVarName,
    );
    const derivedStateReplayBackend = readEnvironmentVariable(
      env,
      derivedStateReplayBackendEnvVarName,
    );
    const derivedStateReplayDir = readEnvironmentVariable(
      env,
      derivedStateReplayDirEnvVarName,
    );
    const derivedStateReplayDurability = readEnvironmentVariable(
      env,
      derivedStateReplayDurabilityEnvVarName,
    );
    const derivedStateReplayMaxEnvelopes = readEnvironmentVariable(
      env,
      derivedStateReplayMaxEnvelopesEnvVarName,
    );
    const derivedStateReplayMaxSessions = readEnvironmentVariable(
      env,
      derivedStateReplayMaxSessionsEnvVarName,
    );

    let parsedRuntimeDeliveryProfile = RuntimeDeliveryProfile.LatencyOptimized;
    if (
      runtimeDeliveryProfile !== undefined &&
      runtimeDeliveryProfile.trim() !== ""
    ) {
      const parsed = parseRuntimeDeliveryProfile(runtimeDeliveryProfile);
      if (isErr(parsed)) {
        return parsed;
      }
      parsedRuntimeDeliveryProfile = parsed.value;
    }

    let parsedShredTrustMode = ShredTrustMode.PublicUntrusted;
    if (shredTrustMode !== undefined && shredTrustMode.trim() !== "") {
      const parsed = parseShredTrustMode(shredTrustMode);
      if (isErr(parsed)) {
        return parsed;
      }
      parsedShredTrustMode = parsed.value;
    }

    let parsedProviderStreamCapabilityPolicy =
      ProviderStreamCapabilityPolicy.Warn;
    if (
      providerStreamCapabilityPolicy !== undefined &&
      providerStreamCapabilityPolicy.trim() !== ""
    ) {
      const parsed = parseProviderStreamCapabilityPolicy(
        providerStreamCapabilityPolicy,
      );
      if (isErr(parsed)) {
        return parsed;
      }
      parsedProviderStreamCapabilityPolicy = parsed.value;
    }

    let parsedProviderStreamAllowEof = false;
    if (
      providerStreamAllowEof !== undefined &&
      providerStreamAllowEof.trim() !== ""
    ) {
      const parsed = parseRuntimeBoolean(
        providerStreamAllowEof,
        providerStreamAllowEofEnvVarName,
      );
      if (isErr(parsed)) {
        return parsed;
      }
      parsedProviderStreamAllowEof = parsed.value;
    }

    let parsedCheckpointIntervalMs = 30_000;
    if (
      derivedStateCheckpointInterval !== undefined &&
      derivedStateCheckpointInterval.trim() !== ""
    ) {
      const parsed = parseNonNegativeInteger(
        derivedStateCheckpointInterval,
        derivedStateCheckpointIntervalEnvVarName,
      );
      if (isErr(parsed)) {
        return parsed;
      }
      parsedCheckpointIntervalMs = parsed.value;
    }

    let parsedRecoveryIntervalMs = 5_000;
    if (
      derivedStateRecoveryInterval !== undefined &&
      derivedStateRecoveryInterval.trim() !== ""
    ) {
      const parsed = parseNonNegativeInteger(
        derivedStateRecoveryInterval,
        derivedStateRecoveryIntervalEnvVarName,
      );
      if (isErr(parsed)) {
        return parsed;
      }
      parsedRecoveryIntervalMs = parsed.value;
    }

    let parsedDerivedStateReplayBackend = DerivedStateReplayBackend.Memory;
    if (
      derivedStateReplayBackend !== undefined &&
      derivedStateReplayBackend.trim() !== ""
    ) {
      const parsed = parseDerivedStateReplayBackend(derivedStateReplayBackend);
      if (isErr(parsed)) {
        return parsed;
      }
      parsedDerivedStateReplayBackend = parsed.value;
    }

    let parsedDerivedStateReplayDirectory = defaultDerivedStateReplayDirectory;
    if (derivedStateReplayDir !== undefined && derivedStateReplayDir.trim() !== "") {
      parsedDerivedStateReplayDirectory = derivedStateReplayDirectory(
        derivedStateReplayDir,
      );
    }

    let parsedDerivedStateReplayDurability = DerivedStateReplayDurability.Flush;
    if (
      derivedStateReplayDurability !== undefined &&
      derivedStateReplayDurability.trim() !== ""
    ) {
      const parsed = parseDerivedStateReplayDurability(
        derivedStateReplayDurability,
      );
      if (isErr(parsed)) {
        return parsed;
      }
      parsedDerivedStateReplayDurability = parsed.value;
    }

    let parsedDerivedStateReplayMaxEnvelopes = 8_192;
    if (
      derivedStateReplayMaxEnvelopes !== undefined &&
      derivedStateReplayMaxEnvelopes.trim() !== ""
    ) {
      const parsed = parseNonNegativeInteger(
        derivedStateReplayMaxEnvelopes,
        derivedStateReplayMaxEnvelopesEnvVarName,
      );
      if (isErr(parsed)) {
        return parsed;
      }
      parsedDerivedStateReplayMaxEnvelopes = parsed.value;
    }

    let parsedDerivedStateReplayMaxSessions = 4;
    if (
      derivedStateReplayMaxSessions !== undefined &&
      derivedStateReplayMaxSessions.trim() !== ""
    ) {
      const parsed = parseNonNegativeInteger(
        derivedStateReplayMaxSessions,
        derivedStateReplayMaxSessionsEnvVarName,
      );
      if (isErr(parsed)) {
        return parsed;
      }
      parsedDerivedStateReplayMaxSessions = parsed.value;
    }

    return ok(
      new ObserverRuntimeConfig({
        runtimeDeliveryProfile: parsedRuntimeDeliveryProfile,
        shredTrustMode: parsedShredTrustMode,
        providerStreamCapabilityPolicy: parsedProviderStreamCapabilityPolicy,
        providerStreamAllowEof: parsedProviderStreamAllowEof,
        derivedState: new DerivedStateRuntimeConfig({
          checkpointIntervalMs: parsedCheckpointIntervalMs,
          recoveryIntervalMs: parsedRecoveryIntervalMs,
          replay: new DerivedStateReplayConfig({
            backend: parsedDerivedStateReplayBackend,
            replayDirectory: parsedDerivedStateReplayDirectory,
            durability: parsedDerivedStateReplayDurability,
            maxEnvelopes: parsedDerivedStateReplayMaxEnvelopes,
            maxSessions: parsedDerivedStateReplayMaxSessions,
          }),
        }),
      }),
    );
  }

  static fromEnvironmentRecord(
    env: Readonly<Record<string, string | undefined>>,
  ): Result<ObserverRuntimeConfig, ObserverRuntimeValidationError> {
    return ObserverRuntimeConfig.fromEnvironment(env);
  }

  static fromEnvironmentVariables(
    env: readonly ObserverRuntimeEnvironmentVariable[],
  ): Result<ObserverRuntimeConfig, ObserverRuntimeValidationError> {
    return ObserverRuntimeConfig.fromEnvironment(env);
  }
}
