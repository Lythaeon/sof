import type { EnvVarName } from "./environment.js";

export enum ValidationErrorKind {
  InvalidRuntimeDeliveryProfile = 1,
}

export interface ValidationError<AllowedValue extends string = string> {
  readonly kind: ValidationErrorKind;
  readonly field: EnvVarName;
  readonly received: string;
  readonly message: string;
  readonly allowedValues?: readonly AllowedValue[];
}
