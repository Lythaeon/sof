import { brand, type Brand } from "../brand.js";
import { envVarName } from "../environment.js";
import { ValidationErrorKind, type ValidationError } from "../errors.js";
import { err, ok, type Result } from "../result.js";

export enum DerivedStateReplayBackend {
  Memory = 1,
  Disk = 2,
}

export enum DerivedStateReplayDurability {
  Flush = 1,
  Fsync = 2,
}

export type DerivedStateReplayBackendEnvValue = Brand<
  string,
  "DerivedStateReplayBackendEnvValue"
>;
export type DerivedStateReplayDurabilityEnvValue = Brand<
  string,
  "DerivedStateReplayDurabilityEnvValue"
>;
export type NonNegativeIntegerEnvValue = Brand<
  string,
  "NonNegativeIntegerEnvValue"
>;
export type DerivedStateReplayDirectory = Brand<
  string,
  "DerivedStateReplayDirectory"
>;

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
export const derivedStateReplayBackendEnvVarName = envVarName(
  "SOF_DERIVED_STATE_REPLAY_BACKEND",
);
export const derivedStateReplayDirEnvVarName = envVarName(
  "SOF_DERIVED_STATE_REPLAY_DIR",
);
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
  [
    derivedStateReplayBackendEnvValues.memory,
    derivedStateReplayBackendEnvValues.disk,
  ];

export const derivedStateReplayDurabilityAllowedValues: readonly DerivedStateReplayDurabilityEnvValue[] =
  [
    derivedStateReplayDurabilityEnvValues.flush,
    derivedStateReplayDurabilityEnvValues.fsync,
  ];

export const defaultDerivedStateReplayDirectory = asDerivedStateReplayDirectory(
  ".sof-derived-state-replay",
);

export function derivedStateReplayDirectory(
  value: string,
): DerivedStateReplayDirectory {
  return asDerivedStateReplayDirectory(value);
}

export function derivedStateReplayBackendToEnvValue(
  backend: DerivedStateReplayBackend,
): DerivedStateReplayBackendEnvValue {
  switch (backend) {
    case DerivedStateReplayBackend.Memory:
      return derivedStateReplayBackendEnvValues.memory;
    case DerivedStateReplayBackend.Disk:
      return derivedStateReplayBackendEnvValues.disk;
  }
}

export function derivedStateReplayDurabilityToEnvValue(
  durability: DerivedStateReplayDurability,
): DerivedStateReplayDurabilityEnvValue {
  switch (durability) {
    case DerivedStateReplayDurability.Flush:
      return derivedStateReplayDurabilityEnvValues.flush;
    case DerivedStateReplayDurability.Fsync:
      return derivedStateReplayDurabilityEnvValues.fsync;
  }
}

export function parseDerivedStateReplayBackend(
  input: string,
): Result<
  DerivedStateReplayBackend,
  ValidationError<DerivedStateReplayBackendEnvValue>
> {
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
): Result<
  DerivedStateReplayDurability,
  ValidationError<DerivedStateReplayDurabilityEnvValue>
> {
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

function requireNonNegativeInteger(field: string, value: number): number {
  if (!Number.isInteger(value) || value < 0) {
    throw new RangeError(`${field} must be a non-negative integer`);
  }

  return value;
}

export function nonNegativeIntegerToEnvValue(
  value: number,
): NonNegativeIntegerEnvValue {
  return asNonNegativeIntegerEnvValue(`${requireNonNegativeInteger("value", value)}`);
}

export interface DerivedStateReplayConfigInit {
  readonly backend?: DerivedStateReplayBackend;
  readonly replayDirectory?: DerivedStateReplayDirectory;
  readonly durability?: DerivedStateReplayDurability;
  readonly maxEnvelopes?: number;
  readonly maxSessions?: number;
}

export class DerivedStateReplayConfig {
  readonly backend: DerivedStateReplayBackend;
  readonly replayDirectory: DerivedStateReplayDirectory;
  readonly durability: DerivedStateReplayDurability;
  readonly maxEnvelopes: number;
  readonly maxSessions: number;

  constructor(init: DerivedStateReplayConfigInit = {}) {
    this.backend = init.backend ?? DerivedStateReplayBackend.Memory;
    this.replayDirectory =
      init.replayDirectory ?? defaultDerivedStateReplayDirectory;
    this.durability = init.durability ?? DerivedStateReplayDurability.Flush;
    this.maxEnvelopes = requireNonNegativeInteger(
      "maxEnvelopes",
      init.maxEnvelopes ?? 8_192,
    );
    this.maxSessions = requireNonNegativeInteger(
      "maxSessions",
      init.maxSessions ?? 4,
    );
  }

  static checkpointOnly(): DerivedStateReplayConfig {
    return new DerivedStateReplayConfig({
      maxEnvelopes: 0,
      maxSessions: 0,
    });
  }

  isEnabled(): boolean {
    return this.maxEnvelopes > 0;
  }
}

export interface DerivedStateRuntimeConfigInit {
  readonly checkpointIntervalMs?: number;
  readonly recoveryIntervalMs?: number;
  readonly replay?: DerivedStateReplayConfig;
}

export class DerivedStateRuntimeConfig {
  readonly checkpointIntervalMs: number;
  readonly recoveryIntervalMs: number;
  readonly replay: DerivedStateReplayConfig;

  constructor(init: DerivedStateRuntimeConfigInit = {}) {
    this.checkpointIntervalMs = requireNonNegativeInteger(
      "checkpointIntervalMs",
      init.checkpointIntervalMs ?? 30_000,
    );
    this.recoveryIntervalMs = requireNonNegativeInteger(
      "recoveryIntervalMs",
      init.recoveryIntervalMs ?? 5_000,
    );
    this.replay = init.replay ?? new DerivedStateReplayConfig();
  }
}
