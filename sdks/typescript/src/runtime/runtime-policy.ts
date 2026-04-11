import { brand, type Brand } from "../brand.js";
import { envVarName, type EnvVarName } from "../environment.js";
import { ValidationErrorKind, type ValidationError } from "../errors.js";
import { err, ok, type Result } from "../result.js";

export enum ShredTrustMode {
  PublicUntrusted = 1,
  TrustedRawShredProvider = 2,
}

export enum ProviderStreamCapabilityPolicy {
  Warn = 1,
  Strict = 2,
}

export const defaultShredTrustMode = ShredTrustMode.PublicUntrusted;
export const defaultProviderStreamCapabilityPolicy =
  ProviderStreamCapabilityPolicy.Warn;
export const defaultProviderStreamAllowEof = false;

export type ShredTrustModeEnvValue = Brand<string, "ShredTrustModeEnvValue">;
export type ProviderStreamCapabilityPolicyEnvValue = Brand<
  string,
  "ProviderStreamCapabilityPolicyEnvValue"
>;
export type RuntimeBooleanEnvValue = Brand<string, "RuntimeBooleanEnvValue">;

function asShredTrustModeEnvValue<const Value extends string>(
  value: Value,
): ShredTrustModeEnvValue {
  return brand<Value, "ShredTrustModeEnvValue">(value);
}

function asProviderStreamCapabilityPolicyEnvValue<const Value extends string>(
  value: Value,
): ProviderStreamCapabilityPolicyEnvValue {
  return brand<Value, "ProviderStreamCapabilityPolicyEnvValue">(value);
}

function asRuntimeBooleanEnvValue<const Value extends string>(
  value: Value,
): RuntimeBooleanEnvValue {
  return brand<Value, "RuntimeBooleanEnvValue">(value);
}

export const shredTrustModeEnvVarName = envVarName("SOF_SHRED_TRUST_MODE");
export const providerStreamCapabilityPolicyEnvVarName = envVarName(
  "SOF_PROVIDER_STREAM_CAPABILITY_POLICY",
);
export const providerStreamAllowEofEnvVarName = envVarName(
  "SOF_PROVIDER_STREAM_ALLOW_EOF",
);

export const shredTrustModeEnvValues = {
  publicUntrusted: asShredTrustModeEnvValue("public_untrusted"),
  trustedRawShredProvider: asShredTrustModeEnvValue(
    "trusted_raw_shred_provider",
  ),
} as const;

export const providerStreamCapabilityPolicyEnvValues = {
  warn: asProviderStreamCapabilityPolicyEnvValue("warn"),
  strict: asProviderStreamCapabilityPolicyEnvValue("strict"),
} as const;

export const runtimeBooleanEnvValues = {
  true: asRuntimeBooleanEnvValue("true"),
  false: asRuntimeBooleanEnvValue("false"),
} as const;

export const shredTrustModeAllowedValues: readonly ShredTrustModeEnvValue[] = [
  shredTrustModeEnvValues.publicUntrusted,
  shredTrustModeEnvValues.trustedRawShredProvider,
];

export const providerStreamCapabilityPolicyAllowedValues: readonly ProviderStreamCapabilityPolicyEnvValue[] =
  [
    providerStreamCapabilityPolicyEnvValues.warn,
    providerStreamCapabilityPolicyEnvValues.strict,
  ];

export const runtimeBooleanAllowedValues: readonly RuntimeBooleanEnvValue[] = [
  runtimeBooleanEnvValues.true,
  runtimeBooleanEnvValues.false,
];

export function isShredTrustMode(value: ShredTrustMode): value is ShredTrustMode {
  switch (value) {
    case ShredTrustMode.PublicUntrusted:
    case ShredTrustMode.TrustedRawShredProvider:
      return true;
    default:
      return false;
  }
}

export function isProviderStreamCapabilityPolicy(
  value: ProviderStreamCapabilityPolicy,
): value is ProviderStreamCapabilityPolicy {
  switch (value) {
    case ProviderStreamCapabilityPolicy.Warn:
    case ProviderStreamCapabilityPolicy.Strict:
      return true;
    default:
      return false;
  }
}

function requireShredTrustMode(value: ShredTrustMode): ShredTrustMode {
  if (!isShredTrustMode(value)) {
    throw new RangeError(`unknown shred trust mode: ${String(value)}`);
  }

  return value;
}

function requireProviderStreamCapabilityPolicy(
  value: ProviderStreamCapabilityPolicy,
): ProviderStreamCapabilityPolicy {
  if (!isProviderStreamCapabilityPolicy(value)) {
    throw new RangeError(
      `unknown provider stream capability policy: ${String(value)}`,
    );
  }

  return value;
}

export function shredTrustModeToEnvValue(
  mode: ShredTrustMode,
): ShredTrustModeEnvValue {
  switch (requireShredTrustMode(mode)) {
    case ShredTrustMode.PublicUntrusted:
      return shredTrustModeEnvValues.publicUntrusted;
    case ShredTrustMode.TrustedRawShredProvider:
      return shredTrustModeEnvValues.trustedRawShredProvider;
  }
}

export function providerStreamCapabilityPolicyToEnvValue(
  policy: ProviderStreamCapabilityPolicy,
): ProviderStreamCapabilityPolicyEnvValue {
  switch (requireProviderStreamCapabilityPolicy(policy)) {
    case ProviderStreamCapabilityPolicy.Warn:
      return providerStreamCapabilityPolicyEnvValues.warn;
    case ProviderStreamCapabilityPolicy.Strict:
      return providerStreamCapabilityPolicyEnvValues.strict;
  }
}

export function runtimeBooleanToEnvValue(value: boolean): RuntimeBooleanEnvValue {
  return value ? runtimeBooleanEnvValues.true : runtimeBooleanEnvValues.false;
}

export function parseShredTrustMode(
  input: string,
): Result<ShredTrustMode, ValidationError<ShredTrustModeEnvValue>> {
  const normalized = input.trim().toLowerCase().replaceAll("-", "_");

  switch (normalized) {
    case "public_untrusted":
      return ok(ShredTrustMode.PublicUntrusted);
    case "trusted_raw_shred_provider":
      return ok(ShredTrustMode.TrustedRawShredProvider);
    default:
      return err({
        kind: ValidationErrorKind.InvalidShredTrustMode,
        field: shredTrustModeEnvVarName,
        received: input,
        message:
          "shred trust mode must be public_untrusted or trusted_raw_shred_provider",
        allowedValues: shredTrustModeAllowedValues,
      });
  }
}

export function parseProviderStreamCapabilityPolicy(
  input: string,
): Result<
  ProviderStreamCapabilityPolicy,
  ValidationError<ProviderStreamCapabilityPolicyEnvValue>
> {
  const normalized = input.trim().toLowerCase();

  switch (normalized) {
    case "warn":
      return ok(ProviderStreamCapabilityPolicy.Warn);
    case "strict":
      return ok(ProviderStreamCapabilityPolicy.Strict);
    default:
      return err({
        kind: ValidationErrorKind.InvalidProviderStreamCapabilityPolicy,
        field: providerStreamCapabilityPolicyEnvVarName,
        received: input,
        message: "provider stream capability policy must be warn or strict",
        allowedValues: providerStreamCapabilityPolicyAllowedValues,
      });
  }
}

export function parseRuntimeBoolean(
  input: string,
  field: EnvVarName,
): Result<boolean, ValidationError<RuntimeBooleanEnvValue>> {
  const normalized = input.trim().toLowerCase();

  switch (normalized) {
    case "1":
    case "true":
    case "yes":
    case "on":
      return ok(true);
    case "0":
    case "false":
    case "no":
    case "off":
      return ok(false);
    default:
      return err({
        kind: ValidationErrorKind.InvalidProviderStreamAllowEof,
        field,
        received: input,
        message: "boolean env value must be true or false",
        allowedValues: runtimeBooleanAllowedValues,
      });
  }
}
