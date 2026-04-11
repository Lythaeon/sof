export enum ValidationErrorKind {
  InvalidRuntimeDeliveryProfile = 1,
}

export interface ValidationError {
  readonly kind: ValidationErrorKind;
  readonly field: string;
  readonly received: string;
  readonly message: string;
  readonly allowedValues?: readonly string[];
}
