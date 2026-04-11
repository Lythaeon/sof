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
  return Object.fromEntries(
    variables.map((variable) => [variable.name, variable.value]),
  );
}

export function readEnvironmentVariable(
  input: EnvironmentInput,
  name: EnvVarName,
): string | undefined {
  if (Array.isArray(input)) {
    for (let index = input.length - 1; index >= 0; index -= 1) {
      const variable = input[index];
      if (variable?.name === name) {
        return variable.value;
      }
    }
    return undefined;
  }

  const record = input as Readonly<Record<string, string | undefined>>;
  return record[name];
}
