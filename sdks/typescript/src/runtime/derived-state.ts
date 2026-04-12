import { brand, type Brand } from "../brand.js";
import { envVarName } from "../environment.js";
import { ValidationErrorKind, type ValidationError } from "../errors.js";
import { err, isErr, ok, type Result } from "../result.js";

export enum DerivedStateReplayBackend {
  Memory = 1,
  Disk = 2,
}

export enum DerivedStateReplayDurability {
  Flush = 1,
  Fsync = 2,
}

export const defaultDerivedStateCheckpointIntervalMs = 30_000;
export const defaultDerivedStateRecoveryIntervalMs = 5_000;
export const defaultDerivedStateReplayBackend = DerivedStateReplayBackend.Memory;
export const defaultDerivedStateReplayDurability = DerivedStateReplayDurability.Flush;
export const defaultDerivedStateReplayMaxEnvelopes = 8_192;
export const defaultDerivedStateReplayMaxSessions = 4;

export type DerivedStateReplayBackendEnvValue = Brand<string, "DerivedStateReplayBackendEnvValue">;
export type DerivedStateReplayDurabilityEnvValue = Brand<
  string,
  "DerivedStateReplayDurabilityEnvValue"
>;
export type NonNegativeIntegerEnvValue = Brand<string, "NonNegativeIntegerEnvValue">;
export type DerivedStateReplayDirectory = Brand<string, "DerivedStateReplayDirectory">;

function asDerivedStateReplayBackendEnvValue<const Value extends string>(
  value: Value,
): DerivedStateReplayBackendEnvValue {
  return brand<Value, "DerivedStateReplayBackendEnvValue">(value);
}

function asDerivedStateReplayDurabilityEnvValue<const Value extends string>(
  value: Value,
): DerivedStateReplayDurabilityEnvValue {
  return brand<Value, "DerivedStateReplayDurabilityEnvValue">(value);
}

function asNonNegativeIntegerEnvValue<const Value extends string>(
  value: Value,
): NonNegativeIntegerEnvValue {
  return brand<Value, "NonNegativeIntegerEnvValue">(value);
}

function asDerivedStateReplayDirectory<const Value extends string>(
  value: Value,
): DerivedStateReplayDirectory {
  return brand<Value, "DerivedStateReplayDirectory">(value);
}

export const derivedStateCheckpointIntervalEnvVarName = envVarName(
  "SOF_DERIVED_STATE_CHECKPOINT_INTERVAL_MS",
);
export const derivedStateRecoveryIntervalEnvVarName = envVarName(
  "SOF_DERIVED_STATE_RECOVERY_INTERVAL_MS",
);
export const derivedStateReplayBackendEnvVarName = envVarName("SOF_DERIVED_STATE_REPLAY_BACKEND");
export const derivedStateReplayDirEnvVarName = envVarName("SOF_DERIVED_STATE_REPLAY_DIR");
export const derivedStateReplayDurabilityEnvVarName = envVarName(
  "SOF_DERIVED_STATE_REPLAY_DURABILITY",
);
export const derivedStateReplayMaxEnvelopesEnvVarName = envVarName(
  "SOF_DERIVED_STATE_REPLAY_MAX_ENVELOPES",
);
export const derivedStateReplayMaxSessionsEnvVarName = envVarName(
  "SOF_DERIVED_STATE_REPLAY_MAX_SESSIONS",
);

export const derivedStateReplayBackendEnvValues = {
  memory: asDerivedStateReplayBackendEnvValue("memory"),
  disk: asDerivedStateReplayBackendEnvValue("disk"),
} as const;

export const derivedStateReplayDurabilityEnvValues = {
  flush: asDerivedStateReplayDurabilityEnvValue("flush"),
  fsync: asDerivedStateReplayDurabilityEnvValue("fsync"),
} as const;

export const derivedStateReplayBackendAllowedValues: readonly DerivedStateReplayBackendEnvValue[] =
  [derivedStateReplayBackendEnvValues.memory, derivedStateReplayBackendEnvValues.disk];

export const derivedStateReplayDurabilityAllowedValues: readonly DerivedStateReplayDurabilityEnvValue[] =
  [derivedStateReplayDurabilityEnvValues.flush, derivedStateReplayDurabilityEnvValues.fsync];

export const defaultDerivedStateReplayDirectory = asDerivedStateReplayDirectory(
  ".sof-derived-state-replay",
);

export function parseDerivedStateReplayDirectory(
  value: string,
): Result<DerivedStateReplayDirectory, ValidationError> {
  if (value.trim() === "") {
    return err({
      kind: ValidationErrorKind.InvalidDerivedStateReplayDirectory,
      field: derivedStateReplayDirEnvVarName,
      received: value,
      message: "replayDirectory must not be empty",
    });
  }
  if (value.includes("\u0000")) {
    return err({
      kind: ValidationErrorKind.InvalidDerivedStateReplayDirectory,
      field: derivedStateReplayDirEnvVarName,
      received: value,
      message: "replayDirectory must not contain NUL bytes",
    });
  }

  return ok(asDerivedStateReplayDirectory(value));
}

export function derivedStateReplayDirectory(value: string): DerivedStateReplayDirectory {
  const result = parseDerivedStateReplayDirectory(value);
  if (!isErr(result)) {
    return result.value;
  }

  throw new RangeError(result.error.message);
}

export function isDerivedStateReplayBackend(
  value: DerivedStateReplayBackend,
): value is DerivedStateReplayBackend {
  switch (value) {
    case DerivedStateReplayBackend.Memory:
    case DerivedStateReplayBackend.Disk:
      return true;
    default:
      return false;
  }
}

export function isDerivedStateReplayDurability(
  value: DerivedStateReplayDurability,
): value is DerivedStateReplayDurability {
  switch (value) {
    case DerivedStateReplayDurability.Flush:
    case DerivedStateReplayDurability.Fsync:
      return true;
    default:
      return false;
  }
}

export function validateDerivedStateReplayBackend(
  value: DerivedStateReplayBackend,
): Result<DerivedStateReplayBackend, ValidationError<DerivedStateReplayBackendEnvValue>> {
  if (!isDerivedStateReplayBackend(value)) {
    return err({
      kind: ValidationErrorKind.InvalidDerivedStateReplayBackend,
      field: derivedStateReplayBackendEnvVarName,
      received: String(value),
      message: "derived-state replay backend must be memory or disk",
      allowedValues: derivedStateReplayBackendAllowedValues,
    });
  }

  return ok(value);
}

export function validateDerivedStateReplayDurability(
  value: DerivedStateReplayDurability,
): Result<DerivedStateReplayDurability, ValidationError<DerivedStateReplayDurabilityEnvValue>> {
  if (!isDerivedStateReplayDurability(value)) {
    return err({
      kind: ValidationErrorKind.InvalidDerivedStateReplayDurability,
      field: derivedStateReplayDurabilityEnvVarName,
      received: String(value),
      message: "derived-state replay durability must be flush or fsync",
      allowedValues: derivedStateReplayDurabilityAllowedValues,
    });
  }

  return ok(value);
}

export function validateNonNegativeIntegerInput(
  value: number,
  field: ReturnType<typeof envVarName>,
  propertyName: string,
): Result<number, ValidationError> {
  if (!Number.isInteger(value) || value < 0) {
    return err({
      kind: ValidationErrorKind.InvalidNonNegativeInteger,
      field,
      received: String(value),
      message: `${propertyName} must be a non-negative integer`,
    });
  }

  return ok(value);
}

function requireDerivedStateReplayBackend(
  value: DerivedStateReplayBackend,
): DerivedStateReplayBackend {
  const validated = validateDerivedStateReplayBackend(value);
  if (!isErr(validated)) {
    return validated.value;
  }

  throw new RangeError(`unknown derived-state replay backend: ${String(value)}`);
}

function requireDerivedStateReplayDurability(
  value: DerivedStateReplayDurability,
): DerivedStateReplayDurability {
  const validated = validateDerivedStateReplayDurability(value);
  if (!isErr(validated)) {
    return validated.value;
  }

  throw new RangeError(`unknown derived-state replay durability: ${String(value)}`);
}

export function tryDerivedStateReplayBackendToEnvValue(
  backend: DerivedStateReplayBackend,
): Result<DerivedStateReplayBackendEnvValue, ValidationError<DerivedStateReplayBackendEnvValue>> {
  const validated = validateDerivedStateReplayBackend(backend);
  if (isErr(validated)) {
    return validated;
  }

  switch (validated.value) {
    case DerivedStateReplayBackend.Memory:
      return ok(derivedStateReplayBackendEnvValues.memory);
    case DerivedStateReplayBackend.Disk:
      return ok(derivedStateReplayBackendEnvValues.disk);
  }

  return err({
    kind: ValidationErrorKind.InvalidDerivedStateReplayBackend,
    field: derivedStateReplayBackendEnvVarName,
    received: String(backend),
    message: "derived-state replay backend must be memory or disk",
    allowedValues: derivedStateReplayBackendAllowedValues,
  });
}

export function derivedStateReplayBackendToEnvValue(
  backend: DerivedStateReplayBackend,
): DerivedStateReplayBackendEnvValue {
  const result = tryDerivedStateReplayBackendToEnvValue(backend);
  if (!isErr(result)) {
    return result.value;
  }

  throw new RangeError(`unknown derived-state replay backend: ${String(backend)}`);
}

export function tryDerivedStateReplayDurabilityToEnvValue(
  durability: DerivedStateReplayDurability,
): Result<
  DerivedStateReplayDurabilityEnvValue,
  ValidationError<DerivedStateReplayDurabilityEnvValue>
> {
  const validated = validateDerivedStateReplayDurability(durability);
  if (isErr(validated)) {
    return validated;
  }

  switch (validated.value) {
    case DerivedStateReplayDurability.Flush:
      return ok(derivedStateReplayDurabilityEnvValues.flush);
    case DerivedStateReplayDurability.Fsync:
      return ok(derivedStateReplayDurabilityEnvValues.fsync);
  }

  return err({
    kind: ValidationErrorKind.InvalidDerivedStateReplayDurability,
    field: derivedStateReplayDurabilityEnvVarName,
    received: String(durability),
    message: "derived-state replay durability must be flush or fsync",
    allowedValues: derivedStateReplayDurabilityAllowedValues,
  });
}

export function derivedStateReplayDurabilityToEnvValue(
  durability: DerivedStateReplayDurability,
): DerivedStateReplayDurabilityEnvValue {
  const result = tryDerivedStateReplayDurabilityToEnvValue(durability);
  if (!isErr(result)) {
    return result.value;
  }

  throw new RangeError(`unknown derived-state replay durability: ${String(durability)}`);
}

export function parseDerivedStateReplayBackend(
  input: string,
): Result<DerivedStateReplayBackend, ValidationError<DerivedStateReplayBackendEnvValue>> {
  const normalized = input.trim().toLowerCase();

  switch (normalized) {
    case "memory":
      return ok(DerivedStateReplayBackend.Memory);
    case "disk":
      return ok(DerivedStateReplayBackend.Disk);
    default:
      return err({
        kind: ValidationErrorKind.InvalidDerivedStateReplayBackend,
        field: derivedStateReplayBackendEnvVarName,
        received: input,
        message: "derived-state replay backend must be memory or disk",
        allowedValues: derivedStateReplayBackendAllowedValues,
      });
  }
}

export function parseDerivedStateReplayDurability(
  input: string,
): Result<DerivedStateReplayDurability, ValidationError<DerivedStateReplayDurabilityEnvValue>> {
  const normalized = input.trim().toLowerCase();

  switch (normalized) {
    case "flush":
      return ok(DerivedStateReplayDurability.Flush);
    case "fsync":
      return ok(DerivedStateReplayDurability.Fsync);
    default:
      return err({
        kind: ValidationErrorKind.InvalidDerivedStateReplayDurability,
        field: derivedStateReplayDurabilityEnvVarName,
        received: input,
        message: "derived-state replay durability must be flush or fsync",
        allowedValues: derivedStateReplayDurabilityAllowedValues,
      });
  }
}

export function parseNonNegativeInteger(
  input: string,
  field: ReturnType<typeof envVarName>,
): Result<number, ValidationError> {
  if (input.trim() === "") {
    return err({
      kind: ValidationErrorKind.InvalidNonNegativeInteger,
      field,
      received: input,
      message: "numeric env value must be a non-negative integer",
    });
  }

  const parsed = Number(input);
  if (!Number.isInteger(parsed) || parsed < 0) {
    return err({
      kind: ValidationErrorKind.InvalidNonNegativeInteger,
      field,
      received: input,
      message: "numeric env value must be a non-negative integer",
    });
  }

  return ok(parsed);
}

function requireNonNegativeInteger(
  field: ReturnType<typeof envVarName>,
  propertyName: string,
  value: number,
): number {
  const validated = validateNonNegativeIntegerInput(value, field, propertyName);
  if (!isErr(validated)) {
    return validated.value;
  }

  throw new RangeError(validated.error.message);
}

function normalizeReplayDirectory(
  value: DerivedStateReplayDirectory | string | undefined,
): DerivedStateReplayDirectory {
  if (value === undefined) {
    return defaultDerivedStateReplayDirectory;
  }

  return derivedStateReplayDirectory(value);
}

export function tryNonNegativeIntegerToEnvValue(
  value: number,
  field: ReturnType<typeof envVarName> = derivedStateReplayMaxEnvelopesEnvVarName,
  propertyName = "value",
): Result<NonNegativeIntegerEnvValue, ValidationError> {
  const validated = validateNonNegativeIntegerInput(value, field, propertyName);
  if (isErr(validated)) {
    return validated;
  }

  return ok(asNonNegativeIntegerEnvValue(`${validated.value}`));
}

export function nonNegativeIntegerToEnvValue(value: number): NonNegativeIntegerEnvValue {
  const result = tryNonNegativeIntegerToEnvValue(value);
  if (!isErr(result)) {
    return result.value;
  }

  throw new RangeError(result.error.message);
}

export interface DerivedStateReplayConfigInit {
  readonly backend?: DerivedStateReplayBackend;
  readonly replayDirectory?: DerivedStateReplayDirectory | string;
  readonly durability?: DerivedStateReplayDurability;
  readonly maxEnvelopes?: number;
  readonly maxSessions?: number;
}

export type DerivedStateReplayConfigInput = DerivedStateReplayConfig | DerivedStateReplayConfigInit;

export type DerivedStateValidationError =
  | ValidationError<DerivedStateReplayBackendEnvValue>
  | ValidationError<DerivedStateReplayDurabilityEnvValue>
  | ValidationError;

export class DerivedStateReplayConfig {
  readonly backend: DerivedStateReplayBackend;
  readonly replayDirectory: DerivedStateReplayDirectory;
  readonly durability: DerivedStateReplayDurability;
  readonly maxEnvelopes: number;
  readonly maxSessions: number;

  constructor(init: DerivedStateReplayConfigInit = {}) {
    this.backend = requireDerivedStateReplayBackend(
      init.backend ?? defaultDerivedStateReplayBackend,
    );
    this.replayDirectory = normalizeReplayDirectory(init.replayDirectory);
    this.durability = requireDerivedStateReplayDurability(
      init.durability ?? defaultDerivedStateReplayDurability,
    );
    this.maxEnvelopes = requireNonNegativeInteger(
      derivedStateReplayMaxEnvelopesEnvVarName,
      "maxEnvelopes",
      init.maxEnvelopes ?? defaultDerivedStateReplayMaxEnvelopes,
    );
    this.maxSessions = requireNonNegativeInteger(
      derivedStateReplayMaxSessionsEnvVarName,
      "maxSessions",
      init.maxSessions ?? defaultDerivedStateReplayMaxSessions,
    );
  }

  static checkpointOnly(): DerivedStateReplayConfig {
    return new DerivedStateReplayConfig({
      maxEnvelopes: 0,
      maxSessions: 0,
    });
  }

  static create(init: DerivedStateReplayConfigInput = {}): DerivedStateReplayConfig {
    return derivedStateReplayConfig(init);
  }

  static tryCreate(
    init: DerivedStateReplayConfigInput = {},
  ): Result<DerivedStateReplayConfig, DerivedStateValidationError> {
    return tryDerivedStateReplayConfig(init);
  }

  static memory(
    init: Omit<DerivedStateReplayConfigInit, "backend"> = {},
  ): DerivedStateReplayConfig {
    return new DerivedStateReplayConfig({
      ...init,
      backend: DerivedStateReplayBackend.Memory,
    });
  }

  static disk(init: Omit<DerivedStateReplayConfigInit, "backend"> = {}): DerivedStateReplayConfig {
    return new DerivedStateReplayConfig({
      ...init,
      backend: DerivedStateReplayBackend.Disk,
    });
  }

  isEnabled(): boolean {
    return this.maxEnvelopes > 0;
  }

  static tryMemory(
    init: Omit<DerivedStateReplayConfigInit, "backend"> = {},
  ): Result<DerivedStateReplayConfig, DerivedStateValidationError> {
    return tryDerivedStateReplayConfig({
      ...init,
      backend: DerivedStateReplayBackend.Memory,
    });
  }

  static tryDisk(
    init: Omit<DerivedStateReplayConfigInit, "backend"> = {},
  ): Result<DerivedStateReplayConfig, DerivedStateValidationError> {
    return tryDerivedStateReplayConfig({
      ...init,
      backend: DerivedStateReplayBackend.Disk,
    });
  }
}

export interface DerivedStateRuntimeConfigInit {
  readonly checkpointIntervalMs?: number;
  readonly recoveryIntervalMs?: number;
  readonly replay?: DerivedStateReplayConfig | DerivedStateReplayConfigInit;
}

export type DerivedStateRuntimeConfigInput =
  | DerivedStateRuntimeConfig
  | DerivedStateRuntimeConfigInit;

export class DerivedStateRuntimeConfig {
  readonly checkpointIntervalMs: number;
  readonly recoveryIntervalMs: number;
  readonly replay: DerivedStateReplayConfig;

  constructor(init: DerivedStateRuntimeConfigInit = {}) {
    this.checkpointIntervalMs = requireNonNegativeInteger(
      derivedStateCheckpointIntervalEnvVarName,
      "checkpointIntervalMs",
      init.checkpointIntervalMs ?? defaultDerivedStateCheckpointIntervalMs,
    );
    this.recoveryIntervalMs = requireNonNegativeInteger(
      derivedStateRecoveryIntervalEnvVarName,
      "recoveryIntervalMs",
      init.recoveryIntervalMs ?? defaultDerivedStateRecoveryIntervalMs,
    );
    this.replay =
      init.replay instanceof DerivedStateReplayConfig
        ? init.replay
        : new DerivedStateReplayConfig(init.replay);
  }

  static create(init: DerivedStateRuntimeConfigInput = {}): DerivedStateRuntimeConfig {
    return derivedStateRuntimeConfig(init);
  }

  static tryCreate(
    init: DerivedStateRuntimeConfigInput = {},
  ): Result<DerivedStateRuntimeConfig, DerivedStateValidationError> {
    return tryDerivedStateRuntimeConfig(init);
  }

  static checkpointOnly(
    init: Omit<DerivedStateRuntimeConfigInit, "replay"> = {},
  ): DerivedStateRuntimeConfig {
    return new DerivedStateRuntimeConfig({
      ...init,
      replay: DerivedStateReplayConfig.checkpointOnly(),
    });
  }

  static tryCheckpointOnly(
    init: Omit<DerivedStateRuntimeConfigInit, "replay"> = {},
  ): Result<DerivedStateRuntimeConfig, DerivedStateValidationError> {
    return tryDerivedStateRuntimeConfig({
      ...init,
      replay: DerivedStateReplayConfig.checkpointOnly(),
    });
  }
}

function validateDerivedStateReplayConfigInit(
  init: DerivedStateReplayConfigInit,
): Result<Required<DerivedStateReplayConfigInit>, DerivedStateValidationError> {
  const backend = validateDerivedStateReplayBackend(
    init.backend ?? defaultDerivedStateReplayBackend,
  );
  if (isErr(backend)) {
    return backend;
  }

  const replayDirectory =
    init.replayDirectory === undefined
      ? ok(defaultDerivedStateReplayDirectory)
      : parseDerivedStateReplayDirectory(init.replayDirectory);
  if (isErr(replayDirectory)) {
    return replayDirectory;
  }

  const durability = validateDerivedStateReplayDurability(
    init.durability ?? defaultDerivedStateReplayDurability,
  );
  if (isErr(durability)) {
    return durability;
  }

  const maxEnvelopes = validateNonNegativeIntegerInput(
    init.maxEnvelopes ?? defaultDerivedStateReplayMaxEnvelopes,
    derivedStateReplayMaxEnvelopesEnvVarName,
    "maxEnvelopes",
  );
  if (isErr(maxEnvelopes)) {
    return maxEnvelopes;
  }

  const maxSessions = validateNonNegativeIntegerInput(
    init.maxSessions ?? defaultDerivedStateReplayMaxSessions,
    derivedStateReplayMaxSessionsEnvVarName,
    "maxSessions",
  );
  if (isErr(maxSessions)) {
    return maxSessions;
  }

  return ok({
    backend: backend.value,
    replayDirectory: replayDirectory.value,
    durability: durability.value,
    maxEnvelopes: maxEnvelopes.value,
    maxSessions: maxSessions.value,
  });
}

function validateDerivedStateRuntimeConfigInit(init: DerivedStateRuntimeConfigInit): Result<
  {
    readonly checkpointIntervalMs: number;
    readonly recoveryIntervalMs: number;
    readonly replay: DerivedStateReplayConfig;
  },
  DerivedStateValidationError
> {
  const checkpointIntervalMs = validateNonNegativeIntegerInput(
    init.checkpointIntervalMs ?? defaultDerivedStateCheckpointIntervalMs,
    derivedStateCheckpointIntervalEnvVarName,
    "checkpointIntervalMs",
  );
  if (isErr(checkpointIntervalMs)) {
    return checkpointIntervalMs;
  }

  const recoveryIntervalMs = validateNonNegativeIntegerInput(
    init.recoveryIntervalMs ?? defaultDerivedStateRecoveryIntervalMs,
    derivedStateRecoveryIntervalEnvVarName,
    "recoveryIntervalMs",
  );
  if (isErr(recoveryIntervalMs)) {
    return recoveryIntervalMs;
  }

  const replay =
    init.replay instanceof DerivedStateReplayConfig
      ? ok(init.replay)
      : tryDerivedStateReplayConfig(init.replay);
  if (isErr(replay)) {
    return replay;
  }

  return ok({
    checkpointIntervalMs: checkpointIntervalMs.value,
    recoveryIntervalMs: recoveryIntervalMs.value,
    replay: replay.value,
  });
}

export function derivedStateReplayConfig(
  init?: DerivedStateReplayConfigInput,
): DerivedStateReplayConfig {
  if (init === undefined) {
    return new DerivedStateReplayConfig();
  }

  return init instanceof DerivedStateReplayConfig ? init : new DerivedStateReplayConfig(init);
}

export function tryDerivedStateReplayConfig(
  init?: DerivedStateReplayConfigInput,
): Result<DerivedStateReplayConfig, DerivedStateValidationError> {
  if (init === undefined) {
    return ok(new DerivedStateReplayConfig());
  }

  if (init instanceof DerivedStateReplayConfig) {
    return ok(init);
  }

  const validated = validateDerivedStateReplayConfigInit(init);
  if (isErr(validated)) {
    return validated;
  }

  return ok(new DerivedStateReplayConfig(validated.value));
}

export function derivedStateRuntimeConfig(
  init?: DerivedStateRuntimeConfigInput,
): DerivedStateRuntimeConfig {
  if (init === undefined) {
    return new DerivedStateRuntimeConfig();
  }

  return init instanceof DerivedStateRuntimeConfig ? init : new DerivedStateRuntimeConfig(init);
}

export function tryDerivedStateRuntimeConfig(
  init?: DerivedStateRuntimeConfigInput,
): Result<DerivedStateRuntimeConfig, DerivedStateValidationError> {
  if (init === undefined) {
    return ok(new DerivedStateRuntimeConfig());
  }

  if (init instanceof DerivedStateRuntimeConfig) {
    return ok(init);
  }

  const validated = validateDerivedStateRuntimeConfigInit(init);
  if (isErr(validated)) {
    return validated;
  }

  return ok(new DerivedStateRuntimeConfig(validated.value));
}
