import { brand, type Brand } from "./brand.js";

export type EnvVarName = Brand<string, "EnvVarName">;

export interface EnvironmentVariable<
  Name extends EnvVarName = EnvVarName,
  Value extends string = string,
> {
  readonly name: Name;
  readonly value: Value;
}

export type EnvironmentInput =
  | Readonly<Record<string, string | undefined>>
  | readonly EnvironmentVariable[];

function isEnvironmentVariableList(
  input: EnvironmentInput,
): input is readonly EnvironmentVariable[] {
  return Array.isArray(input);
}

export function envVarName<const Name extends string>(value: Name): EnvVarName {
  return brand<Name, "EnvVarName">(value);
}

export function environmentVariable<
  Name extends EnvVarName,
  Value extends string,
>(name: Name, value: Value): EnvironmentVariable<Name, Value> {
  return { name, value };
}

export function environmentVariablesToRecord(
  variables: readonly EnvironmentVariable[],
): Readonly<Record<string, string>> {
  const record = Object.fromEntries(
    variables.map((variable) => [variable.name, variable.value] as const),
  );

  Object.setPrototypeOf(record, null);

  return record;
}

function readEnvironmentVariableFromList(
  variables: readonly EnvironmentVariable[],
  name: EnvVarName,
): string | undefined {
  for (let index = variables.length - 1; index >= 0; index -= 1) {
    const variable = variables[index];
    if (variable?.name === name) {
      return variable.value;
    }
  }

  return undefined;
}

function readEnvironmentVariableFromRecord(
  input: Readonly<Record<string, string | undefined>>,
  name: EnvVarName,
): string | undefined {
  if (!Object.prototype.hasOwnProperty.call(input, name)) {
    return undefined;
  }

  return input[name];
}

export function readEnvironmentVariable(
  input: EnvironmentInput,
  name: EnvVarName,
): string | undefined {
  if (isEnvironmentVariableList(input)) {
    return readEnvironmentVariableFromList(input, name);
  }

  return readEnvironmentVariableFromRecord(input, name);
}
