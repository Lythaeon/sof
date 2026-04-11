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

export interface ObserverRuntimeConfigInit {
  readonly runtimeDeliveryProfile?: RuntimeDeliveryProfile;
}

export interface ObserverRuntimeEnvironmentOptions {
  readonly includeDefaults?: boolean;
}

export type ObserverRuntimeEnvironmentVariable = EnvironmentVariable<
  typeof runtimeDeliveryProfileEnvVarName,
  RuntimeDeliveryProfileEnvValue
>;

export class ObserverRuntimeConfig {
  readonly runtimeDeliveryProfile: RuntimeDeliveryProfile;

  constructor(init: ObserverRuntimeConfigInit = {}) {
    this.runtimeDeliveryProfile =
      init.runtimeDeliveryProfile ?? RuntimeDeliveryProfile.LatencyOptimized;
  }

  toEnvironment(
    options: ObserverRuntimeEnvironmentOptions = {},
  ): readonly ObserverRuntimeEnvironmentVariable[] {
    if (
      options.includeDefaults !== true &&
      this.runtimeDeliveryProfile === RuntimeDeliveryProfile.LatencyOptimized
    ) {
      return [];
    }

    return [
      environmentVariable(
        runtimeDeliveryProfileEnvVarName,
        runtimeDeliveryProfileToEnvValue(
          this.runtimeDeliveryProfile,
        ),
      ),
    ];
  }

  toEnvironmentRecord(
    options: ObserverRuntimeEnvironmentOptions = {},
  ): Readonly<Record<string, string>> {
    return environmentVariablesToRecord(this.toEnvironment(options));
  }

  static fromEnvironment(
    env: EnvironmentInput,
  ): Result<ObserverRuntimeConfig, ValidationError<RuntimeDeliveryProfileEnvValue>> {
    const runtimeDeliveryProfile = readEnvironmentVariable(
      env,
      runtimeDeliveryProfileEnvVarName,
    );

    if (
      runtimeDeliveryProfile === undefined ||
      runtimeDeliveryProfile.trim() === ""
    ) {
      return ok(new ObserverRuntimeConfig());
    }

    const parsedRuntimeDeliveryProfile = parseRuntimeDeliveryProfile(
      runtimeDeliveryProfile,
    );
    if (isErr(parsedRuntimeDeliveryProfile)) {
      return parsedRuntimeDeliveryProfile;
    }

    return ok(
      new ObserverRuntimeConfig({
        runtimeDeliveryProfile: parsedRuntimeDeliveryProfile.value,
      }),
    );
  }

  static fromEnvironmentRecord(
    env: Readonly<Record<string, string | undefined>>,
  ): Result<ObserverRuntimeConfig, ValidationError<RuntimeDeliveryProfileEnvValue>> {
    return ObserverRuntimeConfig.fromEnvironment(env);
  }

  static fromEnvironmentVariables(
    env: readonly ObserverRuntimeEnvironmentVariable[],
  ): Result<ObserverRuntimeConfig, ValidationError<RuntimeDeliveryProfileEnvValue>> {
    return ObserverRuntimeConfig.fromEnvironment(env);
  }
}
