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
  parseRuntimeDeliveryProfile,
  RuntimeDeliveryProfile,
  tryRuntimeDeliveryProfileEnvDefaults,
  validateRuntimeDeliveryProfile,
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
  parseDerivedStateReplayDirectory,
  derivedStateReplayDurabilityEnvVarName,
  derivedStateReplayDurabilityToEnvValue,
  derivedStateReplayMaxEnvelopesEnvVarName,
  derivedStateReplayMaxSessionsEnvVarName,
  type DerivedStateValidationError,
  DerivedStateReplayConfig,
  type DerivedStateReplayBackendEnvValue,
  type DerivedStateReplayDirectory,
  type DerivedStateReplayDurabilityEnvValue,
  DerivedStateRuntimeConfig,
  derivedStateRuntimeConfig,
  type DerivedStateRuntimeConfigInit,
  type NonNegativeIntegerEnvValue,
  nonNegativeIntegerToEnvValue,
  parseDerivedStateReplayBackend,
  parseDerivedStateReplayDurability,
  parseNonNegativeInteger,
  tryDerivedStateRuntimeConfig,
} from "./derived-state.js";
import {
  defaultProviderStreamAllowEof,
  defaultProviderStreamCapabilityPolicy,
  defaultShredTrustMode,
  parseProviderStreamCapabilityPolicy,
  parseRuntimeBoolean,
  parseShredTrustMode,
  type ProviderStreamCapabilityPolicy,
  providerStreamAllowEofEnvVarName,
  providerStreamCapabilityPolicyEnvVarName,
  providerStreamCapabilityPolicyToEnvValue,
  type ProviderStreamCapabilityPolicyEnvValue,
  runtimeBooleanToEnvValue,
  type RuntimeBooleanEnvValue,
  type ShredTrustMode,
  validateProviderStreamCapabilityPolicy,
  validateRuntimeBooleanInput,
  validateShredTrustMode,
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

export interface ObserverRuntimeProfileInit
  extends Omit<ObserverRuntimeConfigInit, "runtimeDeliveryProfile"> {}

export type ObserverRuntimeConfigInput =
  | ObserverRuntimeConfig
  | ObserverRuntimeConfigInit;

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
  | DerivedStateValidationError;

function throwObserverRuntimeValidationError(
  error: ObserverRuntimeValidationError,
): never {
  throw new RangeError(error.message);
}

function requireBoolean(field: string, value: boolean): boolean {
  const validated = validateRuntimeBooleanInput(
    value,
    providerStreamAllowEofEnvVarName,
    field,
  );
  if (isErr(validated)) {
    throw new TypeError(validated.error.message);
  }

  return validated.value;
}

function requireObserverRuntimeDeliveryProfile(
  value: RuntimeDeliveryProfile,
): RuntimeDeliveryProfile {
  const validated = validateRuntimeDeliveryProfile(value);
  if (isErr(validated)) {
    throw new RangeError(validated.error.message);
  }

  return validated.value;
}

function requireObserverShredTrustMode(value: ShredTrustMode): ShredTrustMode {
  const validated = validateShredTrustMode(value);
  if (isErr(validated)) {
    throw new RangeError(validated.error.message);
  }

  return validated.value;
}

function requireObserverProviderStreamCapabilityPolicy(
  value: ProviderStreamCapabilityPolicy,
): ProviderStreamCapabilityPolicy {
  const validated = validateProviderStreamCapabilityPolicy(value);
  if (isErr(validated)) {
    throw new RangeError(validated.error.message);
  }

  return validated.value;
}

function shouldIncludeValue<T>(
  currentValue: T,
  defaultValue: T,
  options: ObserverRuntimeEnvironmentOptions,
): boolean {
  return options.includeDefaults === true || currentValue !== defaultValue;
}

function applyRuntimeProfileDerivedStateDefaults(
  profile: RuntimeDeliveryProfile,
  init: ObserverRuntimeProfileInit,
): Result<
  ObserverRuntimeConfigInit,
  ValidationError<RuntimeDeliveryProfileEnvValue>
> {
  const derivedStateDefaults = tryRuntimeDeliveryProfileEnvDefaults(profile);
  if (isErr(derivedStateDefaults)) {
    return derivedStateDefaults;
  }

  const derivedState = init.derivedState;
  const replay =
    derivedState instanceof DerivedStateRuntimeConfig
      ? derivedState.replay
      : derivedState?.replay;

  return ok({
    ...init,
    runtimeDeliveryProfile: profile,
    derivedState:
      derivedState instanceof DerivedStateRuntimeConfig
        ? derivedState
        : {
            ...derivedState,
            replay:
              replay instanceof DerivedStateReplayConfig
                ? replay
                : {
                    ...replay,
                    maxEnvelopes:
                      replay?.maxEnvelopes ??
                      derivedStateDefaults.value.derivedStateReplayMaxEnvelopes,
                    maxSessions:
                      replay?.maxSessions ??
                      derivedStateDefaults.value.derivedStateReplayMaxSessions,
                  },
          },
  });
}

export function tryObserverRuntimeConfig(
  init: ObserverRuntimeConfigInput = {},
): Result<ObserverRuntimeConfig, ObserverRuntimeValidationError> {
  if (init instanceof ObserverRuntimeConfig) {
    return ok(init);
  }

  const runtimeDeliveryProfile = validateRuntimeDeliveryProfile(
    init.runtimeDeliveryProfile ?? defaultRuntimeDeliveryProfile,
  );
  if (isErr(runtimeDeliveryProfile)) {
    return runtimeDeliveryProfile;
  }

  const shredTrustMode = validateShredTrustMode(
    init.shredTrustMode ?? defaultShredTrustMode,
  );
  if (isErr(shredTrustMode)) {
    return shredTrustMode;
  }

  const providerStreamCapabilityPolicy =
    validateProviderStreamCapabilityPolicy(
      init.providerStreamCapabilityPolicy ??
        defaultProviderStreamCapabilityPolicy,
    );
  if (isErr(providerStreamCapabilityPolicy)) {
    return providerStreamCapabilityPolicy;
  }

  const providerStreamAllowEof = validateRuntimeBooleanInput(
    init.providerStreamAllowEof ?? defaultProviderStreamAllowEof,
    providerStreamAllowEofEnvVarName,
    "providerStreamAllowEof",
  );
  if (isErr(providerStreamAllowEof)) {
    return providerStreamAllowEof;
  }

  const derivedState = tryDerivedStateRuntimeConfig(init.derivedState);
  if (isErr(derivedState)) {
    return derivedState;
  }

  return ok(
    new ObserverRuntimeConfig({
      runtimeDeliveryProfile: runtimeDeliveryProfile.value,
      shredTrustMode: shredTrustMode.value,
      providerStreamCapabilityPolicy: providerStreamCapabilityPolicy.value,
      providerStreamAllowEof: providerStreamAllowEof.value,
      derivedState: derivedState.value,
    }),
  );
}

export function tryObserverRuntimeConfigForProfile(
  profile: RuntimeDeliveryProfile,
  init: ObserverRuntimeProfileInit = {},
): Result<ObserverRuntimeConfig, ObserverRuntimeValidationError> {
  const withProfileDefaults = applyRuntimeProfileDerivedStateDefaults(
    profile,
    init,
  );
  if (isErr(withProfileDefaults)) {
    return withProfileDefaults;
  }

  return tryObserverRuntimeConfig(withProfileDefaults.value);
}

export function observerRuntimeConfig(
  init: ObserverRuntimeConfigInput = {},
): ObserverRuntimeConfig {
  const result = tryObserverRuntimeConfig(init);
  if (isErr(result)) {
    throwObserverRuntimeValidationError(result.error);
  }

  return result.value;
}

export function observerRuntimeConfigForProfile(
  profile: RuntimeDeliveryProfile,
  init: ObserverRuntimeProfileInit = {},
): ObserverRuntimeConfig {
  const result = tryObserverRuntimeConfigForProfile(profile, init);
  if (isErr(result)) {
    throwObserverRuntimeValidationError(result.error);
  }

  return result.value;
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
    this.derivedState = derivedStateRuntimeConfig(init.derivedState);
  }

  static create(init: ObserverRuntimeConfigInput = {}): ObserverRuntimeConfig {
    return observerRuntimeConfig(init);
  }

  static tryCreate(
    init: ObserverRuntimeConfigInput = {},
  ): Result<ObserverRuntimeConfig, ObserverRuntimeValidationError> {
    return tryObserverRuntimeConfig(init);
  }

  static forProfile(
    profile: RuntimeDeliveryProfile,
    init: ObserverRuntimeProfileInit = {},
  ): ObserverRuntimeConfig {
    return observerRuntimeConfigForProfile(profile, init);
  }

  static tryForProfile(
    profile: RuntimeDeliveryProfile,
    init: ObserverRuntimeProfileInit = {},
  ): Result<ObserverRuntimeConfig, ObserverRuntimeValidationError> {
    return tryObserverRuntimeConfigForProfile(profile, init);
  }

  static latencyOptimized(
    init: ObserverRuntimeProfileInit = {},
  ): ObserverRuntimeConfig {
    return observerRuntimeConfigForProfile(
      RuntimeDeliveryProfile.LatencyOptimized,
      init,
    );
  }

  static balanced(init: ObserverRuntimeProfileInit = {}): ObserverRuntimeConfig {
    return observerRuntimeConfigForProfile(RuntimeDeliveryProfile.Balanced, init);
  }

  static deliveryDisciplined(
    init: ObserverRuntimeProfileInit = {},
  ): ObserverRuntimeConfig {
    return observerRuntimeConfigForProfile(
      RuntimeDeliveryProfile.DeliveryDisciplined,
      init,
    );
  }

  static tryLatencyOptimized(
    init: ObserverRuntimeProfileInit = {},
  ): Result<ObserverRuntimeConfig, ObserverRuntimeValidationError> {
    return tryObserverRuntimeConfigForProfile(
      RuntimeDeliveryProfile.LatencyOptimized,
      init,
    );
  }

  static tryBalanced(
    init: ObserverRuntimeProfileInit = {},
  ): Result<ObserverRuntimeConfig, ObserverRuntimeValidationError> {
    return tryObserverRuntimeConfigForProfile(
      RuntimeDeliveryProfile.Balanced,
      init,
    );
  }

  static tryDeliveryDisciplined(
    init: ObserverRuntimeProfileInit = {},
  ): Result<ObserverRuntimeConfig, ObserverRuntimeValidationError> {
    return tryObserverRuntimeConfigForProfile(
      RuntimeDeliveryProfile.DeliveryDisciplined,
      init,
    );
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
      const parsed = parseDerivedStateReplayDirectory(
        derivedStateReplayDir,
      );
      if (isErr(parsed)) {
        return parsed;
      }
      parsedDerivedStateReplayDirectory = parsed.value;
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

    return tryObserverRuntimeConfig({
      runtimeDeliveryProfile: parsedRuntimeDeliveryProfile,
      shredTrustMode: parsedShredTrustMode,
      providerStreamCapabilityPolicy: parsedProviderStreamCapabilityPolicy,
      providerStreamAllowEof: parsedProviderStreamAllowEof,
      derivedState: {
        checkpointIntervalMs: parsedCheckpointIntervalMs,
        recoveryIntervalMs: parsedRecoveryIntervalMs,
        replay: {
          backend: parsedDerivedStateReplayBackend,
          replayDirectory: parsedDerivedStateReplayDirectory,
          durability: parsedDerivedStateReplayDurability,
          maxEnvelopes: parsedDerivedStateReplayMaxEnvelopes,
          maxSessions: parsedDerivedStateReplayMaxSessions,
        },
      },
    });
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
