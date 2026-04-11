import type { EnvVarName } from "./environment.js";

export enum ValidationErrorKind {
  InvalidRuntimeDeliveryProfile = 1,
  InvalidShredTrustMode = 2,
  InvalidProviderStreamCapabilityPolicy = 3,
  InvalidProviderStreamAllowEof = 4,
  InvalidDerivedStateReplayBackend = 5,
  InvalidDerivedStateReplayDurability = 6,
  InvalidNonNegativeInteger = 7,
  InvalidDerivedStateReplayDirectory = 8,
}

export interface ValidationError<AllowedValue extends string = string> {
  readonly kind: ValidationErrorKind;
  readonly field: EnvVarName;
  readonly received: string;
  readonly message: string;
  readonly allowedValues?: readonly AllowedValue[];
}
