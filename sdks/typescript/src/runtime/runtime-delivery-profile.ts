import { brand, type Brand } from "../brand.js";
import { envVarName } from "../environment.js";
import { ValidationErrorKind, type ValidationError } from "../errors.js";
import { err, ok, type Result } from "../result.js";
import {
  defaultDerivedStateReplayMaxEnvelopes,
  defaultDerivedStateReplayMaxSessions,
} from "./derived-state.js";

export enum RuntimeDeliveryProfile {
  LatencyOptimized = 1,
  Balanced = 2,
  DeliveryDisciplined = 3,
}

export const defaultRuntimeDeliveryProfile =
  RuntimeDeliveryProfile.LatencyOptimized;

export type RuntimeDeliveryProfileEnvValue = Brand<
  string,
  "RuntimeDeliveryProfileEnvValue"
>;

function asRuntimeDeliveryProfileEnvValue<const Value extends string>(
  value: Value,
): RuntimeDeliveryProfileEnvValue {
  return brand<Value, "RuntimeDeliveryProfileEnvValue">(value);
}

export const runtimeDeliveryProfileEnvVarName = envVarName(
  "SOF_RUNTIME_DELIVERY_PROFILE",
);

export const runtimeDeliveryProfileEnvValues = {
  latencyOptimized: asRuntimeDeliveryProfileEnvValue("latency_optimized"),
  balanced: asRuntimeDeliveryProfileEnvValue("balanced"),
  deliveryDisciplined: asRuntimeDeliveryProfileEnvValue(
    "delivery_disciplined",
  ),
} as const;

export const runtimeDeliveryProfileAllowedValues: readonly RuntimeDeliveryProfileEnvValue[] =
  [
    runtimeDeliveryProfileEnvValues.latencyOptimized,
    runtimeDeliveryProfileEnvValues.balanced,
    runtimeDeliveryProfileEnvValues.deliveryDisciplined,
  ];

export interface RuntimeDeliveryProfileEnvDefaults {
  readonly derivedStateReplayMaxEnvelopes: number;
  readonly derivedStateReplayMaxSessions: number;
}

export function isRuntimeDeliveryProfile(
  value: RuntimeDeliveryProfile,
): value is RuntimeDeliveryProfile {
  switch (value) {
    case RuntimeDeliveryProfile.LatencyOptimized:
    case RuntimeDeliveryProfile.Balanced:
    case RuntimeDeliveryProfile.DeliveryDisciplined:
      return true;
    default:
      return false;
  }
}

function requireRuntimeDeliveryProfile(
  value: RuntimeDeliveryProfile,
): RuntimeDeliveryProfile {
  if (!isRuntimeDeliveryProfile(value)) {
    throw new RangeError(`unknown runtime delivery profile: ${String(value)}`);
  }

  return value;
}

export function runtimeDeliveryProfileToEnvValue(
  profile: RuntimeDeliveryProfile,
): RuntimeDeliveryProfileEnvValue {
  switch (requireRuntimeDeliveryProfile(profile)) {
    case RuntimeDeliveryProfile.LatencyOptimized:
      return runtimeDeliveryProfileEnvValues.latencyOptimized;
    case RuntimeDeliveryProfile.Balanced:
      return runtimeDeliveryProfileEnvValues.balanced;
    case RuntimeDeliveryProfile.DeliveryDisciplined:
      return runtimeDeliveryProfileEnvValues.deliveryDisciplined;
  }
}

export function runtimeDeliveryProfileEnvDefaults(
  profile: RuntimeDeliveryProfile,
): RuntimeDeliveryProfileEnvDefaults {
  switch (requireRuntimeDeliveryProfile(profile)) {
    case RuntimeDeliveryProfile.LatencyOptimized:
      return {
        derivedStateReplayMaxEnvelopes: defaultDerivedStateReplayMaxEnvelopes,
        derivedStateReplayMaxSessions: defaultDerivedStateReplayMaxSessions,
      };
    case RuntimeDeliveryProfile.Balanced:
      return {
        derivedStateReplayMaxEnvelopes:
          defaultDerivedStateReplayMaxEnvelopes * 2,
        derivedStateReplayMaxSessions:
          defaultDerivedStateReplayMaxSessions + 2,
      };
    case RuntimeDeliveryProfile.DeliveryDisciplined:
      return {
        derivedStateReplayMaxEnvelopes:
          defaultDerivedStateReplayMaxEnvelopes * 4,
        derivedStateReplayMaxSessions:
          defaultDerivedStateReplayMaxSessions * 2,
      };
  }
}

export function parseRuntimeDeliveryProfile(
  input: string,
): Result<
  RuntimeDeliveryProfile,
  ValidationError<RuntimeDeliveryProfileEnvValue>
> {
  const normalized = input.trim().toLowerCase().replaceAll("-", "_");

  switch (normalized) {
    case "latency_optimized":
      return ok(RuntimeDeliveryProfile.LatencyOptimized);
    case "balanced":
      return ok(RuntimeDeliveryProfile.Balanced);
    case "delivery_disciplined":
      return ok(RuntimeDeliveryProfile.DeliveryDisciplined);
    default:
      return err({
        kind: ValidationErrorKind.InvalidRuntimeDeliveryProfile,
        field: runtimeDeliveryProfileEnvVarName,
        received: input,
        message:
          "runtime delivery profile must be latency_optimized, balanced, or delivery_disciplined",
        allowedValues: runtimeDeliveryProfileAllowedValues,
      });
  }
}
