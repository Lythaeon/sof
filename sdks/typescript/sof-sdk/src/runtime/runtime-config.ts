import type { ValidationError } from "../errors.js";
import { isErr, ok, type Result } from "../result.js";
import {
  parseRuntimeDeliveryProfile,
  RuntimeDeliveryProfile,
  runtimeDeliveryProfileEnvKey,
  runtimeDeliveryProfileToEnvValue,
} from "./runtime-delivery-profile.js";

export interface ObserverRuntimeConfigInit {
  readonly runtimeDeliveryProfile?: RuntimeDeliveryProfile;
}

export interface ObserverRuntimeEnvironmentOptions {
  readonly includeDefaults?: boolean;
}

export class ObserverRuntimeConfig {
  readonly runtimeDeliveryProfile: RuntimeDeliveryProfile;

  constructor(init: ObserverRuntimeConfigInit = {}) {
    this.runtimeDeliveryProfile =
      init.runtimeDeliveryProfile ?? RuntimeDeliveryProfile.LatencyOptimized;
  }

  toEnvironment(
    options: ObserverRuntimeEnvironmentOptions = {},
  ): Readonly<Record<string, string>> {
    if (
      options.includeDefaults !== true &&
      this.runtimeDeliveryProfile === RuntimeDeliveryProfile.LatencyOptimized
    ) {
      return {};
    }

    return {
      [runtimeDeliveryProfileEnvKey]: runtimeDeliveryProfileToEnvValue(
        this.runtimeDeliveryProfile,
      ),
    };
  }

  static fromEnvironment(
    env: Readonly<Record<string, string | undefined>>,
  ): Result<ObserverRuntimeConfig, ValidationError> {
    const runtimeDeliveryProfile = env[runtimeDeliveryProfileEnvKey];

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
}
