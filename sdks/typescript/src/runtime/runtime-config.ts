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
    >;

export type ObserverRuntimeValidationError =
  | ValidationError<RuntimeDeliveryProfileEnvValue>
  | ValidationError<ShredTrustModeEnvValue>
  | ValidationError<ProviderStreamCapabilityPolicyEnvValue>
  | ValidationError<RuntimeBooleanEnvValue>;

export class ObserverRuntimeConfig {
  readonly runtimeDeliveryProfile: RuntimeDeliveryProfile;
  readonly shredTrustMode: ShredTrustMode;
  readonly providerStreamCapabilityPolicy: ProviderStreamCapabilityPolicy;
  readonly providerStreamAllowEof: boolean;

  constructor(init: ObserverRuntimeConfigInit = {}) {
    this.runtimeDeliveryProfile =
      init.runtimeDeliveryProfile ?? RuntimeDeliveryProfile.LatencyOptimized;
    this.shredTrustMode = init.shredTrustMode ?? ShredTrustMode.PublicUntrusted;
    this.providerStreamCapabilityPolicy =
      init.providerStreamCapabilityPolicy ?? ProviderStreamCapabilityPolicy.Warn;
    this.providerStreamAllowEof = init.providerStreamAllowEof ?? false;
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

    return ok(
      new ObserverRuntimeConfig({
        runtimeDeliveryProfile: parsedRuntimeDeliveryProfile,
        shredTrustMode: parsedShredTrustMode,
        providerStreamCapabilityPolicy: parsedProviderStreamCapabilityPolicy,
        providerStreamAllowEof: parsedProviderStreamAllowEof,
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
