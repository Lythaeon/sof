import { ValidationErrorKind, type ValidationError } from "../errors.js";
import { err, ok, type Result } from "../result.js";

export enum RuntimeDeliveryProfile {
  LatencyOptimized = 1,
  Balanced = 2,
  DeliveryDisciplined = 3,
}

export const runtimeDeliveryProfileEnvKey = "SOF_RUNTIME_DELIVERY_PROFILE";

export function runtimeDeliveryProfileToEnvValue(
  profile: RuntimeDeliveryProfile,
): string {
  switch (profile) {
    case RuntimeDeliveryProfile.LatencyOptimized:
      return "latency_optimized";
    case RuntimeDeliveryProfile.Balanced:
      return "balanced";
    case RuntimeDeliveryProfile.DeliveryDisciplined:
      return "delivery_disciplined";
  }
}

export function parseRuntimeDeliveryProfile(
  input: string,
): Result<RuntimeDeliveryProfile, ValidationError> {
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
        field: runtimeDeliveryProfileEnvKey,
        received: input,
        message:
          "runtime delivery profile must be latency_optimized, balanced, or delivery_disciplined",
        allowedValues: [
          "latency_optimized",
          "balanced",
          "delivery_disciplined",
        ],
      });
  }
}
