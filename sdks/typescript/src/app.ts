import { brand, type Brand } from "./brand.js";
import {
  envVarName,
  environmentVariablesToRecord,
  readEnvironmentVariable,
  type EnvironmentInput,
} from "./environment.js";
import { err, isErr, ok, type Result } from "./result.js";
import {
  type ExtensionCapability,
  type ExtensionContext,
  type ExtensionResourceSpec,
  type ObserverRuntimeConfigInput,
  type ObserverRuntimeEnvironmentOptions,
  type ObserverRuntimeConfig,
  type PacketSubscription,
  runRuntimeExtensionWorkerStdio,
  type RuntimeExtensionAck,
  type RuntimeExtensionDefinition,
  type RuntimeExtensionError,
  type RuntimePacketEvent,
  type RuntimeExtensionWorkerStdioOptions,
  extensionName,
  tryCreateRuntimeExtensionWorkerManifest,
  tryCreateRuntimeConfig,
  tryDefineRuntimeExtension,
  type ExtensionName,
} from "./runtime.js";

export enum SofApplicationErrorKind {
  ValidationError = 1,
  DuplicateRuntimeExtension = 2,
  MissingRuntimeExtension = 3,
  MissingRuntimeExtensionSelection = 4,
}

export interface SofApplicationError {
  readonly kind: SofApplicationErrorKind;
  readonly field: string;
  readonly message: string;
  readonly received?: string;
  readonly availableExtensionNames?: readonly string[];
}

export type SofApplicationName = Brand<string, "SofApplicationName">;

export interface SofApplicationInit {
  readonly name: string | SofApplicationName;
  readonly runtime?: ObserverRuntimeConfigInput;
  readonly runtimeExtensions?: readonly RuntimeExtensionDefinition[];
  readonly plugins?: readonly SofPluginInput[];
}

export type SofApplicationInput = SofApplication | SofApplicationInit;
export type SofPlugin = RuntimeExtensionDefinition;
export type SofPluginError = RuntimeExtensionError;
export type SofPluginInput = SofPlugin | SofPluginInit;

export interface SofPluginInit {
  readonly name: string | ExtensionName;
  readonly capabilities?: readonly ExtensionCapability[];
  readonly resources?: readonly ExtensionResourceSpec[];
  readonly subscriptions?: readonly PacketSubscription[];
  readonly onStart?: (
    context: ExtensionContext,
  ) =>
    | Promise<Result<RuntimeExtensionAck, SofPluginError>>
    | Result<RuntimeExtensionAck, SofPluginError>;
  readonly onPacket?: (
    event: RuntimePacketEvent,
  ) =>
    | Promise<Result<RuntimeExtensionAck, SofPluginError>>
    | Result<RuntimeExtensionAck, SofPluginError>;
  readonly onStop?: (
    context: ExtensionContext,
  ) =>
    | Promise<Result<RuntimeExtensionAck, SofPluginError>>
    | Result<RuntimeExtensionAck, SofPluginError>;
}

export interface SofNodeLaunchSpecInit {
  readonly workerEntrypoint: string;
  readonly workerCommand?: string;
  readonly workerArgs?: readonly string[];
  readonly workerEnvironment?: Readonly<Record<string, string | undefined>>;
  readonly runtimeEnvironment?: ObserverRuntimeEnvironmentOptions;
}

export interface SofNodeRuntimeExtensionLaunchSpec {
  readonly extensionName: ExtensionName;
  readonly transport: "stdio";
  readonly command: string;
  readonly args: readonly string[];
  readonly environment: Readonly<Record<string, string>>;
}

export type SofPluginLaunchSpec = SofNodeRuntimeExtensionLaunchSpec;

export interface SofNodeLaunchSpec {
  readonly appName: SofApplicationName;
  readonly runtimeEnvironment: Readonly<Record<string, string>>;
  readonly plugins: readonly SofPluginLaunchSpec[];
  readonly runtimeExtensions: readonly SofNodeRuntimeExtensionLaunchSpec[];
}

export type SofApplicationWorkerRunError = SofApplicationError | RuntimeExtensionError;

export const sofApplicationNameEnvVarName = envVarName("SOF_SDK_APPLICATION_NAME");
export const sofRuntimeExtensionNameEnvVarName = envVarName("SOF_SDK_RUNTIME_EXTENSION_NAME");
export const sofTypeScriptSdkVersion = "0.1.0";

const defaultNodeCommand = "node";
const sofApplicationValidatedInitTag = Symbol("SofApplicationValidatedInit");

interface SofApplicationValidatedInit {
  readonly [sofApplicationValidatedInitTag]: true;
  readonly name: SofApplicationName;
  readonly runtime: ObserverRuntimeConfig;
  readonly plugins: readonly SofPlugin[];
  readonly pluginsByName: ReadonlyMap<string, SofPlugin>;
  readonly runtimeExtensions: readonly RuntimeExtensionDefinition[];
  readonly runtimeExtensionsByName: ReadonlyMap<string, RuntimeExtensionDefinition>;
}

function sofApplicationError(
  kind: SofApplicationErrorKind,
  field: string,
  message: string,
  received?: string,
  availableExtensionNames?: readonly string[],
): SofApplicationError {
  const error: SofApplicationError = {
    kind,
    field,
    message,
  };

  if (received !== undefined) {
    return {
      ...error,
      received,
      ...(availableExtensionNames === undefined ? {} : { availableExtensionNames }),
    };
  }

  if (availableExtensionNames !== undefined) {
    return {
      ...error,
      availableExtensionNames,
    };
  }

  return error;
}

function parseNonEmptyAppValue<T>(
  value: string,
  field: string,
  wrap: (normalized: string) => T,
): Result<T, SofApplicationError> {
  const normalized = value.trim();
  if (normalized === "") {
    return err(
      sofApplicationError(
        SofApplicationErrorKind.ValidationError,
        field,
        `${field} must not be empty`,
        value,
      ),
    );
  }
  if (normalized.includes("\u0000")) {
    return err(
      sofApplicationError(
        SofApplicationErrorKind.ValidationError,
        field,
        `${field} must not contain NUL bytes`,
        value,
      ),
    );
  }

  return ok(wrap(normalized));
}

function asSofApplicationName<const Value extends string>(value: Value): SofApplicationName {
  return brand<Value, "SofApplicationName">(value);
}

function parseSofApplicationName(value: string): Result<SofApplicationName, SofApplicationError> {
  return parseNonEmptyAppValue(value, "name", asSofApplicationName);
}

function mergeEnvironmentRecords(
  base: Readonly<Record<string, string | undefined>> = {},
  overlay: Readonly<Record<string, string | undefined>> = {},
): Readonly<Record<string, string>> {
  const variables: Array<{ readonly name: string; readonly value: string }> = [];

  for (const source of [base, overlay]) {
    for (const [key, value] of Object.entries(source)) {
      if (value !== undefined) {
        variables.push({
          name: key,
          value,
        });
      }
    }
  }

  return environmentVariablesToRecord(
    variables.map((variable) => ({
      name: envVarName(variable.name),
      value: variable.value,
    })),
  );
}

function throwSofApplicationError(error: SofApplicationError): never {
  throw new RangeError(error.message);
}

function toRuntimeExtensionNameKey(value: ExtensionName): string {
  return value;
}

export class SofApplication {
  readonly name!: SofApplicationName;
  readonly runtime!: ObserverRuntimeConfig;
  readonly plugins!: readonly SofPlugin[];
  readonly pluginsByName!: ReadonlyMap<string, SofPlugin>;
  readonly runtimeExtensions!: readonly RuntimeExtensionDefinition[];
  readonly runtimeExtensionsByName!: ReadonlyMap<string, RuntimeExtensionDefinition>;

  constructor(init: SofApplicationInit | SofApplicationValidatedInit) {
    if (sofApplicationValidatedInitTag in init) {
      this.name = init.name;
      this.runtime = init.runtime;
      this.plugins = init.plugins;
      this.pluginsByName = init.pluginsByName;
      this.runtimeExtensions = init.runtimeExtensions;
      this.runtimeExtensionsByName = init.runtimeExtensionsByName;
      return;
    }

    const result = tryDefineSofApplication(init);
    if (isErr(result)) {
      throwSofApplicationError(result.error);
    }

    this.name = result.value.name;
    this.runtime = result.value.runtime;
    this.plugins = result.value.plugins;
    this.pluginsByName = result.value.pluginsByName;
    this.runtimeExtensions = result.value.runtimeExtensions;
    this.runtimeExtensionsByName = result.value.runtimeExtensionsByName;
  }

  toRuntimeEnvironmentRecord(
    options: ObserverRuntimeEnvironmentOptions = {},
  ): Readonly<Record<string, string>> {
    return this.runtime.toEnvironmentRecord(options);
  }

  getRuntimeExtension(
    name: string | ExtensionName,
  ): Result<RuntimeExtensionDefinition, SofApplicationError> {
    const parsedName = typeof name === "string" ? extensionName(name) : ok(name);
    if (isErr(parsedName)) {
      return err(
        sofApplicationError(
          SofApplicationErrorKind.ValidationError,
          "runtimeExtensionName",
          parsedName.error.message,
          typeof name === "string" ? name : String(name),
        ),
      );
    }

    const found = this.runtimeExtensionsByName.get(toRuntimeExtensionNameKey(parsedName.value));
    if (found !== undefined) {
      return ok(found);
    }

    return err(
      sofApplicationError(
        SofApplicationErrorKind.MissingRuntimeExtension,
        "runtimeExtensionName",
        `runtime extension ${String(parsedName.value)} is not registered in application ${String(this.name)}`,
        String(parsedName.value),
        this.runtimeExtensions.map((definition) => String(definition.manifest.extensionName)),
      ),
    );
  }

  getPlugin(name: string | ExtensionName): Result<SofPlugin, SofApplicationError> {
    return this.getRuntimeExtension(name);
  }

  toNodeLaunchSpec(init: SofNodeLaunchSpecInit): Result<SofNodeLaunchSpec, SofApplicationError> {
    const workerEntrypoint = parseNonEmptyAppValue(
      init.workerEntrypoint,
      "workerEntrypoint",
      (value) => value,
    );
    if (isErr(workerEntrypoint)) {
      return workerEntrypoint;
    }

    const command = parseNonEmptyAppValue(
      init.workerCommand ?? defaultNodeCommand,
      "workerCommand",
      (value) => value,
    );
    if (isErr(command)) {
      return command;
    }

    const runtimeEnvironment = this.runtime.toEnvironmentRecord(init.runtimeEnvironment);
    const runtimeExtensions = this.runtimeExtensions.map((definition) => ({
      extensionName: definition.manifest.extensionName,
      transport: "stdio" as const,
      command: command.value,
      args: [...(init.workerArgs ?? []), workerEntrypoint.value],
      environment: mergeEnvironmentRecords(init.workerEnvironment, {
        [sofApplicationNameEnvVarName]: this.name,
        [sofRuntimeExtensionNameEnvVarName]: definition.manifest.extensionName,
      }),
    }));

    return ok({
      appName: this.name,
      runtimeEnvironment,
      plugins: runtimeExtensions,
      runtimeExtensions,
    });
  }

  static create(init: SofApplicationInit): SofApplication {
    return defineSofApplication(init);
  }

  static tryCreate(init: SofApplicationInit): Result<SofApplication, SofApplicationError> {
    return tryDefineSofApplication(init);
  }
}

function createValidatedSofApplication(
  init: Omit<SofApplicationValidatedInit, typeof sofApplicationValidatedInitTag>,
): SofApplication {
  return new SofApplication({
    [sofApplicationValidatedInitTag]: true,
    ...init,
  });
}

export function tryDefineSofApplication(
  init: SofApplicationInput,
): Result<SofApplication, SofApplicationError> {
  if (init instanceof SofApplication) {
    return ok(init);
  }

  const name = typeof init.name === "string" ? parseSofApplicationName(init.name) : ok(init.name);
  if (isErr(name)) {
    return name;
  }

  const runtime = tryCreateRuntimeConfig(init.runtime);
  if (isErr(runtime)) {
    return err(
      sofApplicationError(
        SofApplicationErrorKind.ValidationError,
        "runtime",
        runtime.error.message,
      ),
    );
  }

  const validatedRuntimeExtensions: RuntimeExtensionDefinition[] = [];
  const runtimeExtensionsByName = new Map<string, RuntimeExtensionDefinition>();
  const pluginDefinitions: RuntimeExtensionDefinition[] = [];
  for (const plugin of init.plugins ?? []) {
    const validatedPlugin = tryDefineSofPlugin(plugin);
    if (isErr(validatedPlugin)) {
      return err(
        sofApplicationError(
          SofApplicationErrorKind.ValidationError,
          "plugins",
          validatedPlugin.error.message,
        ),
      );
    }

    pluginDefinitions.push(validatedPlugin.value);
  }

  for (const runtimeExtension of [...(init.runtimeExtensions ?? []), ...pluginDefinitions]) {
    const validatedRuntimeExtension = tryDefineRuntimeExtension(runtimeExtension);
    if (isErr(validatedRuntimeExtension)) {
      return err(
        sofApplicationError(
          SofApplicationErrorKind.ValidationError,
          "runtimeExtensions",
          validatedRuntimeExtension.error.message,
        ),
      );
    }

    const runtimeExtensionNameKey = toRuntimeExtensionNameKey(
      validatedRuntimeExtension.value.manifest.extensionName,
    );
    if (runtimeExtensionsByName.has(runtimeExtensionNameKey)) {
      return err(
        sofApplicationError(
          SofApplicationErrorKind.DuplicateRuntimeExtension,
          "runtimeExtensions",
          `runtime extension ${runtimeExtensionNameKey} is registered more than once`,
          runtimeExtensionNameKey,
        ),
      );
    }

    validatedRuntimeExtensions.push(validatedRuntimeExtension.value);
    runtimeExtensionsByName.set(runtimeExtensionNameKey, validatedRuntimeExtension.value);
  }

  return ok(
    createValidatedSofApplication({
      name: name.value,
      runtime: runtime.value,
      plugins: validatedRuntimeExtensions,
      pluginsByName: runtimeExtensionsByName,
      runtimeExtensions: validatedRuntimeExtensions,
      runtimeExtensionsByName,
    }),
  );
}

export function tryDefineSofPlugin(init: SofPluginInput): Result<SofPlugin, SofPluginError> {
  if ("manifest" in init) {
    return tryDefineRuntimeExtension(init);
  }

  const manifest = tryCreateRuntimeExtensionWorkerManifest({
    sdkVersion: sofTypeScriptSdkVersion,
    extensionName: init.name,
    ...(init.capabilities === undefined ? {} : { capabilities: init.capabilities }),
    ...(init.resources === undefined ? {} : { resources: init.resources }),
    ...(init.subscriptions === undefined ? {} : { subscriptions: init.subscriptions }),
  });
  if (isErr(manifest)) {
    return manifest;
  }

  return tryDefineRuntimeExtension({
    manifest: manifest.value,
    ...(init.onStart === undefined ? {} : { onReady: init.onStart }),
    ...(init.onPacket === undefined ? {} : { onPacketReceived: init.onPacket }),
    ...(init.onStop === undefined ? {} : { onShutdown: init.onStop }),
  });
}

export function defineSofPlugin(init: SofPluginInput): SofPlugin {
  const result = tryDefineSofPlugin(init);
  if (isErr(result)) {
    throw new RangeError(result.error.message);
  }

  return result.value;
}

export function defineSofApplication(init: SofApplicationInit): SofApplication {
  const result = tryDefineSofApplication(init);
  if (isErr(result)) {
    throwSofApplicationError(result.error);
  }

  return result.value;
}

export function tryCreateSofNodeLaunchSpec(
  app: SofApplicationInput,
  init: SofNodeLaunchSpecInit,
): Result<SofNodeLaunchSpec, SofApplicationError> {
  const application = tryDefineSofApplication(app);
  if (isErr(application)) {
    return application;
  }

  return application.value.toNodeLaunchSpec(init);
}

export function createSofNodeLaunchSpec(
  app: SofApplicationInput,
  init: SofNodeLaunchSpecInit,
): SofNodeLaunchSpec {
  const result = tryCreateSofNodeLaunchSpec(app, init);
  if (isErr(result)) {
    throwSofApplicationError(result.error);
  }

  return result.value;
}

export function tryRunSofApplicationRuntimeExtensionWorker(
  app: SofApplicationInput,
  name: string | ExtensionName,
  options: RuntimeExtensionWorkerStdioOptions = {},
): Promise<Result<RuntimeExtensionAck, SofApplicationWorkerRunError>> {
  const application = tryDefineSofApplication(app);
  if (isErr(application)) {
    return Promise.resolve(application);
  }

  const runtimeExtension = application.value.getRuntimeExtension(name);
  if (isErr(runtimeExtension)) {
    return Promise.resolve(runtimeExtension);
  }

  return runRuntimeExtensionWorkerStdio(runtimeExtension.value, options);
}

export async function runSofApplicationRuntimeExtensionWorker(
  app: SofApplicationInput,
  name: string | ExtensionName,
  options: RuntimeExtensionWorkerStdioOptions = {},
): Promise<RuntimeExtensionAck> {
  const result = await tryRunSofApplicationRuntimeExtensionWorker(app, name, options);
  if (isErr(result)) {
    throw new RangeError(result.error.message);
  }

  return result.value;
}

export function tryRunSofApplicationRuntimeExtensionWorkerFromEnvironment(
  app: SofApplicationInput,
  env: EnvironmentInput = process.env,
  options: RuntimeExtensionWorkerStdioOptions = {},
): Promise<Result<RuntimeExtensionAck, SofApplicationWorkerRunError>> {
  const selectedRuntimeExtensionName = readEnvironmentVariable(
    env,
    sofRuntimeExtensionNameEnvVarName,
  );
  if (selectedRuntimeExtensionName === undefined || selectedRuntimeExtensionName.trim() === "") {
    const application = tryDefineSofApplication(app);
    const availableExtensionNames = isErr(application)
      ? undefined
      : application.value.runtimeExtensions.map((definition) =>
          String(definition.manifest.extensionName),
        );

    return Promise.resolve(
      err(
        sofApplicationError(
          SofApplicationErrorKind.MissingRuntimeExtensionSelection,
          String(sofRuntimeExtensionNameEnvVarName),
          `${String(sofRuntimeExtensionNameEnvVarName)} must be set to select one runtime extension worker`,
          selectedRuntimeExtensionName,
          availableExtensionNames,
        ),
      ),
    );
  }

  return tryRunSofApplicationRuntimeExtensionWorker(app, selectedRuntimeExtensionName, options);
}

export async function runSofApplicationRuntimeExtensionWorkerFromEnvironment(
  app: SofApplicationInput,
  env: EnvironmentInput = process.env,
  options: RuntimeExtensionWorkerStdioOptions = {},
): Promise<RuntimeExtensionAck> {
  const result = await tryRunSofApplicationRuntimeExtensionWorkerFromEnvironment(app, env, options);
  if (isErr(result)) {
    throw new RangeError(result.error.message);
  }

  return result.value;
}

export type SofApp = SofApplication;
export type SofAppError = SofApplicationError;
export type SofAppInit = SofApplicationInit;
export type SofAppInput = SofApplicationInput;
export type SofAppLaunchSpec = SofNodeLaunchSpec;
export type SofAppLaunchSpecInit = SofNodeLaunchSpecInit;
export type SofPluginName = ExtensionName;

export function tryDefineSofApp(init: SofAppInput): Result<SofApp, SofAppError> {
  return tryDefineSofApplication(init);
}

export function defineSofApp(init: SofAppInit): SofApp {
  return defineSofApplication(init);
}

export function tryCreateSofAppLaunch(
  app: SofAppInput,
  init: SofAppLaunchSpecInit,
): Result<SofAppLaunchSpec, SofAppError> {
  return tryCreateSofNodeLaunchSpec(app, init);
}

export function createSofAppLaunch(app: SofAppInput, init: SofAppLaunchSpecInit): SofAppLaunchSpec {
  return createSofNodeLaunchSpec(app, init);
}

export function tryRunSofPlugin(
  app: SofAppInput,
  name: string | SofPluginName,
  options: RuntimeExtensionWorkerStdioOptions = {},
): Promise<Result<RuntimeExtensionAck, SofApplicationWorkerRunError>> {
  return tryRunSofApplicationRuntimeExtensionWorker(app, name, options);
}

export function runSofPlugin(
  app: SofAppInput,
  name: string | SofPluginName,
  options: RuntimeExtensionWorkerStdioOptions = {},
): Promise<RuntimeExtensionAck> {
  return runSofApplicationRuntimeExtensionWorker(app, name, options);
}

export function tryRunSelectedSofPlugin(
  app: SofAppInput,
  env: EnvironmentInput = process.env,
  options: RuntimeExtensionWorkerStdioOptions = {},
): Promise<Result<RuntimeExtensionAck, SofApplicationWorkerRunError>> {
  return tryRunSofApplicationRuntimeExtensionWorkerFromEnvironment(app, env, options);
}

export function runSelectedSofPlugin(
  app: SofAppInput,
  env: EnvironmentInput = process.env,
  options: RuntimeExtensionWorkerStdioOptions = {},
): Promise<RuntimeExtensionAck> {
  return runSofApplicationRuntimeExtensionWorkerFromEnvironment(app, env, options);
}

export const defineApp = defineSofApp;
export const definePlugin = defineSofPlugin;
export const tryDefineApp = tryDefineSofApp;
export const tryDefinePlugin = tryDefineSofPlugin;
export const createAppLaunch = createSofAppLaunch;
export const tryCreateAppLaunch = tryCreateSofAppLaunch;
export const runPlugin = runSofPlugin;
export const tryRunPlugin = tryRunSofPlugin;
export const runSelectedPlugin = runSelectedSofPlugin;
export const tryRunSelectedPlugin = tryRunSelectedSofPlugin;
