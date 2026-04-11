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
  defaultRuntimeDeliveryProfile,
  isRuntimeDeliveryProfile,
  parseRuntimeDeliveryProfile,
  RuntimeDeliveryProfile,
  type RuntimeDeliveryProfileEnvValue,
  runtimeDeliveryProfileEnvVarName,
  runtimeDeliveryProfileToEnvValue,
} from "./runtime-delivery-profile.js";
import {
  defaultDerivedStateCheckpointIntervalMs,
  defaultDerivedStateRecoveryIntervalMs,
  defaultDerivedStateReplayBackend,
  defaultDerivedStateReplayDirectory,
  defaultDerivedStateReplayDurability,
  defaultDerivedStateReplayMaxEnvelopes,
  defaultDerivedStateReplayMaxSessions,
  derivedStateCheckpointIntervalEnvVarName,
  derivedStateRecoveryIntervalEnvVarName,
  derivedStateReplayBackendEnvVarName,
  derivedStateReplayBackendToEnvValue,
  derivedStateReplayDirEnvVarName,
  derivedStateReplayDurabilityEnvVarName,
  derivedStateReplayDurabilityToEnvValue,
  derivedStateReplayMaxEnvelopesEnvVarName,
  derivedStateReplayMaxSessionsEnvVarName,
  DerivedStateReplayConfig,
  type DerivedStateReplayBackendEnvValue,
  type DerivedStateReplayDirectory,
  type DerivedStateReplayDurabilityEnvValue,
  DerivedStateRuntimeConfig,
  type DerivedStateRuntimeConfigInit,
  type NonNegativeIntegerEnvValue,
  nonNegativeIntegerToEnvValue,
  parseDerivedStateReplayBackend,
  parseDerivedStateReplayDurability,
  parseNonNegativeInteger,
  derivedStateReplayDirectory,
} from "./derived-state.js";
import {
  defaultProviderStreamAllowEof,
  defaultProviderStreamCapabilityPolicy,
  defaultShredTrustMode,
  isProviderStreamCapabilityPolicy,
  isShredTrustMode,
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
  readonly derivedState?: DerivedStateRuntimeConfig | DerivedStateRuntimeConfigInit;
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

function requireBoolean(field: string, value: boolean): boolean {
  if (typeof value !== "boolean") {
    throw new TypeError(`${field} must be a boolean`);
  }

  return value;
}

function requireObserverRuntimeDeliveryProfile(
  value: RuntimeDeliveryProfile,
): RuntimeDeliveryProfile {
  if (!isRuntimeDeliveryProfile(value)) {
    throw new RangeError(`unknown runtime delivery profile: ${String(value)}`);
  }

  return value;
}

function requireObserverShredTrustMode(value: ShredTrustMode): ShredTrustMode {
  if (!isShredTrustMode(value)) {
    throw new RangeError(`unknown shred trust mode: ${String(value)}`);
  }

  return value;
}

function requireObserverProviderStreamCapabilityPolicy(
  value: ProviderStreamCapabilityPolicy,
): ProviderStreamCapabilityPolicy {
  if (!isProviderStreamCapabilityPolicy(value)) {
    throw new RangeError(
      `unknown provider stream capability policy: ${String(value)}`,
    );
  }

  return value;
}

function shouldIncludeValue<T>(
  currentValue: T,
  defaultValue: T,
  options: ObserverRuntimeEnvironmentOptions,
): boolean {
  return options.includeDefaults === true || currentValue !== defaultValue;
}

export class ObserverRuntimeConfig {
  readonly runtimeDeliveryProfile: RuntimeDeliveryProfile;
  readonly shredTrustMode: ShredTrustMode;
  readonly providerStreamCapabilityPolicy: ProviderStreamCapabilityPolicy;
  readonly providerStreamAllowEof: boolean;
  readonly derivedState: DerivedStateRuntimeConfig;

  constructor(init: ObserverRuntimeConfigInit = {}) {
    this.runtimeDeliveryProfile = requireObserverRuntimeDeliveryProfile(
      init.runtimeDeliveryProfile ?? defaultRuntimeDeliveryProfile,
    );
    this.shredTrustMode = requireObserverShredTrustMode(
      init.shredTrustMode ?? defaultShredTrustMode,
    );
    this.providerStreamCapabilityPolicy =
      requireObserverProviderStreamCapabilityPolicy(
        init.providerStreamCapabilityPolicy ??
          defaultProviderStreamCapabilityPolicy,
      );
    this.providerStreamAllowEof = requireBoolean(
      "providerStreamAllowEof",
      init.providerStreamAllowEof ?? defaultProviderStreamAllowEof,
    );
    this.derivedState =
      init.derivedState instanceof DerivedStateRuntimeConfig
        ? init.derivedState
        : new DerivedStateRuntimeConfig(init.derivedState);
  }

  toEnvironment(
    options: ObserverRuntimeEnvironmentOptions = {},
  ): readonly ObserverRuntimeEnvironmentVariable[] {
    const environment: ObserverRuntimeEnvironmentVariable[] = [];

    if (
      shouldIncludeValue(
        this.runtimeDeliveryProfile,
        defaultRuntimeDeliveryProfile,
        options,
      )
    ) {
      environment.push(
        environmentVariable(
          runtimeDeliveryProfileEnvVarName,
          runtimeDeliveryProfileToEnvValue(this.runtimeDeliveryProfile),
        ),
      );
    }

    if (
      shouldIncludeValue(this.shredTrustMode, defaultShredTrustMode, options)
    ) {
      environment.push(
        environmentVariable(
          shredTrustModeEnvVarName,
          shredTrustModeToEnvValue(this.shredTrustMode),
        ),
      );
    }

    if (
      shouldIncludeValue(
        this.providerStreamCapabilityPolicy,
        defaultProviderStreamCapabilityPolicy,
        options,
      )
    ) {
      environment.push(
        environmentVariable(
          providerStreamCapabilityPolicyEnvVarName,
          providerStreamCapabilityPolicyToEnvValue(
            this.providerStreamCapabilityPolicy,
          ),
        ),
      );
    }

    if (
      shouldIncludeValue(
        this.providerStreamAllowEof,
        defaultProviderStreamAllowEof,
        options,
      )
    ) {
      environment.push(
        environmentVariable(
          providerStreamAllowEofEnvVarName,
          runtimeBooleanToEnvValue(this.providerStreamAllowEof),
        ),
      );
    }

    if (
      shouldIncludeValue(
        this.derivedState.checkpointIntervalMs,
        defaultDerivedStateCheckpointIntervalMs,
        options,
      )
    ) {
      environment.push(
        environmentVariable(
          derivedStateCheckpointIntervalEnvVarName,
          nonNegativeIntegerToEnvValue(this.derivedState.checkpointIntervalMs),
        ),
      );
    }

    if (
      shouldIncludeValue(
        this.derivedState.recoveryIntervalMs,
        defaultDerivedStateRecoveryIntervalMs,
        options,
      )
    ) {
      environment.push(
        environmentVariable(
          derivedStateRecoveryIntervalEnvVarName,
          nonNegativeIntegerToEnvValue(this.derivedState.recoveryIntervalMs),
        ),
      );
    }

    if (
      shouldIncludeValue(
        this.derivedState.replay.backend,
        defaultDerivedStateReplayBackend,
        options,
      )
    ) {
      environment.push(
        environmentVariable(
          derivedStateReplayBackendEnvVarName,
          derivedStateReplayBackendToEnvValue(this.derivedState.replay.backend),
        ),
      );
    }

    if (
      shouldIncludeValue(
        this.derivedState.replay.replayDirectory,
        defaultDerivedStateReplayDirectory,
        options,
      )
    ) {
      environment.push(
        environmentVariable(
          derivedStateReplayDirEnvVarName,
          this.derivedState.replay.replayDirectory,
        ),
      );
    }

    if (
      shouldIncludeValue(
        this.derivedState.replay.durability,
        defaultDerivedStateReplayDurability,
        options,
      )
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
      shouldIncludeValue(
        this.derivedState.replay.maxEnvelopes,
        defaultDerivedStateReplayMaxEnvelopes,
        options,
      )
    ) {
      environment.push(
        environmentVariable(
          derivedStateReplayMaxEnvelopesEnvVarName,
          nonNegativeIntegerToEnvValue(this.derivedState.replay.maxEnvelopes),
        ),
      );
    }

    if (
      shouldIncludeValue(
        this.derivedState.replay.maxSessions,
        defaultDerivedStateReplayMaxSessions,
        options,
      )
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

    let parsedRuntimeDeliveryProfile = defaultRuntimeDeliveryProfile;
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

    let parsedShredTrustMode = defaultShredTrustMode;
    if (shredTrustMode !== undefined && shredTrustMode.trim() !== "") {
      const parsed = parseShredTrustMode(shredTrustMode);
      if (isErr(parsed)) {
        return parsed;
      }
      parsedShredTrustMode = parsed.value;
    }

    let parsedProviderStreamCapabilityPolicy =
      defaultProviderStreamCapabilityPolicy;
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

    let parsedProviderStreamAllowEof = defaultProviderStreamAllowEof;
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

    let parsedCheckpointIntervalMs = defaultDerivedStateCheckpointIntervalMs;
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

    let parsedRecoveryIntervalMs = defaultDerivedStateRecoveryIntervalMs;
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

    let parsedDerivedStateReplayBackend = defaultDerivedStateReplayBackend;
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

    let parsedDerivedStateReplayDurability = defaultDerivedStateReplayDurability;
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

    let parsedDerivedStateReplayMaxEnvelopes =
      defaultDerivedStateReplayMaxEnvelopes;
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

    let parsedDerivedStateReplayMaxSessions =
      defaultDerivedStateReplayMaxSessions;
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
