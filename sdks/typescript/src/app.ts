import { spawn, type ChildProcess } from "node:child_process";
import { chmodSync, existsSync } from "node:fs";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { brand, type Brand } from "./brand.js";
import { err, isErr, ok, type Result } from "./result.js";
import {
  ExtensionCapability,
  type ExtensionContext,
  type ExtensionManifest,
  type ExtensionName,
  type ExtensionResourceSpec,
  extensionName,
  ObserverRuntimeConfig,
  observerRuntimeConfig,
  type ObserverRuntimeConfigInput,
  type PacketSubscription,
  type RuntimeExtensionAck,
  runtimeExtensionAck,
  type RuntimeExtensionError,
  RuntimePacketSourceKind,
  type RuntimePacketEvent,
  type RuntimeProviderEvent,
  RuntimeDeliveryProfile,
  type ShredTrustMode,
  socketAddress,
  webSocketUrl,
} from "./runtime.js";
import {
  type RuntimeExtensionDefinition,
  type RuntimeExtensionWorkerManifest,
  runRuntimeExtensionWorkerStdio,
  tryCreateRuntimeConfig,
  tryCreateRuntimeExtensionWorkerManifest,
  tryDefineRuntimeExtension,
} from "./runtime/app-internal.js";

export enum AppErrorKind {
  ValidationError = 1,
  DuplicatePlugin = 2,
  MissingPlugin = 3,
}

export interface AppError {
  readonly kind: AppErrorKind;
  readonly field: string;
  readonly message: string;
  readonly received?: string;
  readonly availablePluginNames?: readonly string[];
}

export type AppName = Brand<string, "AppName">;
export type PluginError = RuntimeExtensionError;
export type PluginName = ExtensionName;

export interface PluginHandler {
  readonly onStart?: (
    context: ExtensionContext,
  ) => Promise<Result<RuntimeExtensionAck, PluginError>> | Result<RuntimeExtensionAck, PluginError>;
  readonly onPacket?: (
    event: RuntimePacketEvent,
  ) => Promise<Result<RuntimeExtensionAck, PluginError>> | Result<RuntimeExtensionAck, PluginError>;
  readonly onProviderEvent?: (
    event: RuntimeProviderEvent,
  ) => Promise<Result<RuntimeExtensionAck, PluginError>> | Result<RuntimeExtensionAck, PluginError>;
  readonly onStop?: (
    context: ExtensionContext,
  ) => Promise<Result<RuntimeExtensionAck, PluginError>> | Result<RuntimeExtensionAck, PluginError>;
}

export interface PluginInit extends PluginHandler {
  readonly name?: string | ExtensionName;
  readonly capabilities?: readonly ExtensionCapability[];
  readonly resources?: readonly ExtensionResourceSpec[];
  readonly subscriptions?: readonly PacketSubscription[];
  readonly logPackets?:
    | boolean
    | {
        readonly output?: NodeJS.WritableStream;
        readonly formatter?: (event: RuntimePacketEvent) => string;
      };
}

export enum IngressKind {
  WebSocket = 1,
  Grpc = 2,
  Gossip = 3,
  DirectShreds = 4,
}

export enum GrpcIngressStream {
  Transactions = 1,
  TransactionStatus = 2,
  Accounts = 3,
  BlockMeta = 4,
  Slots = 5,
}

export enum WebSocketIngressStream {
  Transactions = 1,
  Logs = 2,
  Account = 3,
  Program = 4,
}

export enum WebSocketLogsFilterKind {
  All = 1,
  AllWithVotes = 2,
  Mentions = 3,
}

export enum ProviderCommitment {
  Processed = 1,
  Confirmed = 2,
  Finalized = 3,
}

export enum ProviderIngressReadiness {
  Required = 1,
  Optional = 2,
}

export enum ProviderIngressRole {
  Primary = 1,
  Secondary = 2,
  Fallback = 3,
  ConfirmOnly = 4,
}

export enum GossipRuntimeMode {
  Full = 1,
  BootstrapOnly = 2,
  ControlPlaneOnly = 3,
}

export interface WebSocketIngressInit {
  readonly kind: IngressKind.WebSocket;
  readonly name?: string;
  readonly url: string;
  readonly stream?: WebSocketIngressStream;
  readonly commitment?: ProviderCommitment;
  readonly vote?: boolean;
  readonly failed?: boolean;
  readonly signature?: string;
  readonly accountInclude?: readonly string[];
  readonly accountExclude?: readonly string[];
  readonly accountRequired?: readonly string[];
  readonly account?: string;
  readonly programId?: string;
  readonly logsFilter?: WebSocketLogsFilterKind;
  readonly mentions?: string;
  readonly readiness?: ProviderIngressReadiness;
  readonly role?: ProviderIngressRole;
  readonly priority?: number;
}

export interface GrpcIngressInit {
  readonly kind: IngressKind.Grpc;
  readonly name?: string;
  readonly endpoint: string;
  readonly tls?: boolean;
  readonly stream?: GrpcIngressStream;
  readonly xToken?: string;
  readonly commitment?: ProviderCommitment;
  readonly vote?: boolean;
  readonly failed?: boolean;
  readonly signature?: string;
  readonly accountInclude?: readonly string[];
  readonly accountExclude?: readonly string[];
  readonly accountRequired?: readonly string[];
  readonly accounts?: readonly string[];
  readonly owners?: readonly string[];
  readonly requireTransactionSignature?: boolean;
  readonly readiness?: ProviderIngressReadiness;
  readonly role?: ProviderIngressRole;
  readonly priority?: number;
}

export interface GossipIngressInit {
  readonly kind: IngressKind.Gossip;
  readonly name?: string;
  readonly bindAddress?: string;
  readonly entrypoints?: readonly string[];
  readonly runtimeMode?: GossipRuntimeMode;
  readonly entrypointPinned?: boolean;
}

export interface KernelBypassInit {
  readonly interface: string;
  readonly queueId?: number;
  readonly batchSize?: number;
  readonly umemFrameCount?: number;
  readonly ringDepth?: number;
  readonly pollTimeoutMs?: number;
}

export interface DirectShredsIngressInit {
  readonly kind: IngressKind.DirectShreds;
  readonly name?: string;
  readonly bindAddress: string;
  readonly trustMode?: ShredTrustMode;
  readonly kernelBypass?: KernelBypassInit;
}

export type IngressInit =
  | WebSocketIngressInit
  | GrpcIngressInit
  | GossipIngressInit
  | DirectShredsIngressInit;

export enum FanInStrategy {
  EmitAll = 1,
  FirstSeen = 2,
  FirstSeenThenPromote = 3,
}

export interface FanInInit {
  readonly strategy?: FanInStrategy;
}

export interface WebSocketIngress {
  readonly kind: IngressKind.WebSocket;
  readonly name: string;
  readonly url: string;
  readonly stream: WebSocketIngressStream;
  readonly commitment?: ProviderCommitment;
  readonly vote?: boolean;
  readonly failed?: boolean;
  readonly signature?: string;
  readonly accountInclude?: readonly string[];
  readonly accountExclude?: readonly string[];
  readonly accountRequired?: readonly string[];
  readonly account?: string;
  readonly programId?: string;
  readonly logsFilter?: WebSocketLogsFilterKind;
  readonly mentions?: string;
  readonly readiness: ProviderIngressReadiness;
  readonly role: ProviderIngressRole;
  readonly priority?: number;
}

export interface GrpcIngress {
  readonly kind: IngressKind.Grpc;
  readonly name: string;
  readonly endpoint: string;
  readonly tls: boolean;
  readonly stream: GrpcIngressStream;
  readonly xToken?: string;
  readonly commitment?: ProviderCommitment;
  readonly vote?: boolean;
  readonly failed?: boolean;
  readonly signature?: string;
  readonly accountInclude: readonly string[];
  readonly accountExclude: readonly string[];
  readonly accountRequired: readonly string[];
  readonly accounts: readonly string[];
  readonly owners: readonly string[];
  readonly requireTransactionSignature: boolean;
  readonly readiness: ProviderIngressReadiness;
  readonly role: ProviderIngressRole;
  readonly priority?: number;
}

export interface GossipIngress {
  readonly kind: IngressKind.Gossip;
  readonly name: string;
  readonly bindAddress?: string;
  readonly entrypoints: readonly string[];
  readonly runtimeMode?: GossipRuntimeMode;
  readonly entrypointPinned?: boolean;
}

export interface DirectShredsIngress {
  readonly kind: IngressKind.DirectShreds;
  readonly name: string;
  readonly bindAddress: string;
  readonly trustMode?: ShredTrustMode;
  readonly kernelBypass?: KernelBypassConfig;
}

export interface KernelBypassConfig {
  readonly interface: string;
  readonly queueId: number;
  readonly batchSize: number;
  readonly umemFrameCount: number;
  readonly ringDepth: number;
  readonly pollTimeoutMs: number;
}

export type Ingress = WebSocketIngress | GrpcIngress | GossipIngress | DirectShredsIngress;

export interface FanIn {
  readonly strategy: FanInStrategy;
}

export interface AppInit {
  readonly name?: string | AppName;
  readonly runtime?: ObserverRuntimeConfigInput;
  readonly ingress?: readonly IngressInit[];
  readonly fanIn?: FanInInit;
  readonly plugins?: readonly Plugin[];
  readonly extensions?: readonly Plugin[];
}

export interface AppRunOptions {
  readonly signal?: AbortSignal;
}

export type AppRunError = AppError | RuntimeExtensionError;
export type ExtensionHandler = PluginHandler;
export type ExtensionError = PluginError;
export type ExtensionInit = PluginInit;
export const typeScriptSdkVersion = "0.1.1";

const require = createRequire(import.meta.url);

const defaultAppName = "app";
const autoPluginNamePrefix = "plugin";
const internalPluginWorkerEnvVarName = "SOF_SDK_INTERNAL_PLUGIN_WORKER";
const internalPluginWorkerModeEnvVarName = "SOF_SDK_INTERNAL_PLUGIN_WORKER_MODE";
const internalPluginWorkerModeEnvValue = "1";
const runtimeHostBinaryEnvVarName = "SOF_SDK_RUNTIME_HOST_BINARY";
const runtimeHostBinaryBaseName = "sof_ts_runtime_host";
const grpcIngressStreams = [
  GrpcIngressStream.Transactions,
  GrpcIngressStream.TransactionStatus,
  GrpcIngressStream.Accounts,
  GrpcIngressStream.BlockMeta,
  GrpcIngressStream.Slots,
] as const satisfies readonly GrpcIngressStream[];
const webSocketIngressStreams = [
  WebSocketIngressStream.Transactions,
  WebSocketIngressStream.Logs,
  WebSocketIngressStream.Account,
  WebSocketIngressStream.Program,
] as const satisfies readonly WebSocketIngressStream[];
const webSocketLogsFilters = [
  WebSocketLogsFilterKind.All,
  WebSocketLogsFilterKind.AllWithVotes,
  WebSocketLogsFilterKind.Mentions,
] as const satisfies readonly WebSocketLogsFilterKind[];
const providerCommitments = [
  ProviderCommitment.Processed,
  ProviderCommitment.Confirmed,
  ProviderCommitment.Finalized,
] as const satisfies readonly ProviderCommitment[];
const providerIngressReadiness = [
  ProviderIngressReadiness.Required,
  ProviderIngressReadiness.Optional,
] as const satisfies readonly ProviderIngressReadiness[];
const providerIngressRoles = [
  ProviderIngressRole.Primary,
  ProviderIngressRole.Secondary,
  ProviderIngressRole.Fallback,
  ProviderIngressRole.ConfirmOnly,
] as const satisfies readonly ProviderIngressRole[];
const gossipRuntimeModes = [
  GossipRuntimeMode.Full,
  GossipRuntimeMode.BootstrapOnly,
  GossipRuntimeMode.ControlPlaneOnly,
] as const satisfies readonly GossipRuntimeMode[];
const defaultKernelBypassQueueId = 0;
const defaultKernelBypassBatchSize = 64;
const defaultKernelBypassUmemFrameCount = 4_096;
const defaultKernelBypassRingDepth = 2_048;
const defaultKernelBypassPollTimeoutMs = 100;

let nextAutoPluginOrdinal = 1;
const pluginDefinitions = new WeakMap<Plugin, RuntimeExtensionDefinition>();

function appError(
  kind: AppErrorKind,
  field: string,
  message: string,
  received?: string,
  availablePluginNames?: readonly string[],
): AppError {
  const base: AppError = {
    kind,
    field,
    message,
  };

  if (received !== undefined) {
    return {
      ...base,
      received,
      ...(availablePluginNames === undefined ? {} : { availablePluginNames }),
    };
  }

  if (availablePluginNames !== undefined) {
    return {
      ...base,
      availablePluginNames,
    };
  }

  return base;
}

function throwAppError(error: AppError): never {
  throw new RangeError(error.message);
}

function parseNonEmptyValue<T>(
  value: string,
  field: string,
  wrap: (normalized: string) => T,
): Result<T, AppError> {
  const normalized = value.trim();
  if (normalized === "") {
    return err(appError(AppErrorKind.ValidationError, field, `${field} must not be empty`, value));
  }
  if (normalized.includes("\u0000")) {
    return err(
      appError(AppErrorKind.ValidationError, field, `${field} must not contain NUL bytes`, value),
    );
  }

  return ok(wrap(normalized));
}

function asAppName<const Value extends string>(value: Value): AppName {
  return brand<Value, "AppName">(value);
}

function parseAppName(value: string): Result<AppName, AppError> {
  return parseNonEmptyValue(value, "name", asAppName);
}

function nextAutoPluginName(): string {
  const ordinal = nextAutoPluginOrdinal;
  nextAutoPluginOrdinal += 1;
  return `${autoPluginNamePrefix}-${ordinal}`;
}

function defaultAppNameFromPlugins(plugins: readonly Plugin[]): string {
  const firstPlugin = plugins[0];
  if (firstPlugin !== undefined) {
    return `${String(firstPlugin.name)}-app`;
  }
  return defaultAppName;
}

function mergePluginHandlers(base: PluginHandler, overlay: PluginHandler): PluginHandler {
  return {
    ...(base.onStart === undefined ? {} : { onStart: base.onStart }),
    ...(overlay.onStart === undefined ? {} : { onStart: overlay.onStart }),
    ...(base.onPacket === undefined ? {} : { onPacket: base.onPacket }),
    ...(overlay.onPacket === undefined ? {} : { onPacket: overlay.onPacket }),
    ...(base.onProviderEvent === undefined ? {} : { onProviderEvent: base.onProviderEvent }),
    ...(overlay.onProviderEvent === undefined ? {} : { onProviderEvent: overlay.onProviderEvent }),
    ...(base.onStop === undefined ? {} : { onStop: base.onStop }),
    ...(overlay.onStop === undefined ? {} : { onStop: overlay.onStop }),
  };
}

function createPacketLoggerHandler(
  output: NodeJS.WritableStream,
  formatter?: (event: RuntimePacketEvent) => string,
): PluginHandler {
  return {
    onPacket: (event) => {
      const line =
        formatter?.(event) ??
        JSON.stringify(
          {
            observedUnixMs: event.observedUnixMs,
            source: event.source,
            bytes: Array.from(event.bytes),
          },
          undefined,
          2,
        );
      output.write(`${line}\n`);
      return ok(runtimeExtensionAck());
    },
  };
}

function normalizePluginName(
  value: string | ExtensionName | undefined,
): Result<ExtensionName, PluginError> {
  if (value === undefined) {
    return extensionName(nextAutoPluginName());
  }

  return typeof value === "string" ? extensionName(value) : ok(value);
}

function defaultPacketCapabilities(handlers: PluginHandler): readonly ExtensionCapability[] {
  return handlers.onPacket === undefined ? [] : [ExtensionCapability.ObserveObserverIngress];
}

function defaultPacketSubscriptions(handlers: PluginHandler): readonly PacketSubscription[] {
  return handlers.onPacket === undefined
    ? []
    : [
        {
          sourceKind: RuntimePacketSourceKind.ObserverIngress,
        },
      ];
}

function createPluginDefinition(init: PluginInit): Result<RuntimeExtensionDefinition, PluginError> {
  const name = normalizePluginName(init.name);
  if (isErr(name)) {
    return name;
  }

  const logger =
    init.logPackets === true
      ? { output: process.stdout }
      : init.logPackets === undefined || init.logPackets === false
        ? undefined
        : {
            output: init.logPackets.output ?? process.stdout,
            ...(init.logPackets.formatter === undefined
              ? {}
              : { formatter: init.logPackets.formatter }),
          };
  const handlers =
    logger === undefined
      ? init
      : mergePluginHandlers(createPacketLoggerHandler(logger.output, logger.formatter), init);

  const manifest = tryCreateRuntimeExtensionWorkerManifest({
    sdkVersion: typeScriptSdkVersion,
    extensionName: name.value,
    capabilities: init.capabilities ?? defaultPacketCapabilities(handlers),
    ...(init.resources === undefined ? {} : { resources: init.resources }),
    subscriptions: init.subscriptions ?? defaultPacketSubscriptions(handlers),
  });
  if (isErr(manifest)) {
    return manifest;
  }

  return tryDefineRuntimeExtension({
    manifest: manifest.value,
    ...(handlers.onStart === undefined ? {} : { onReady: handlers.onStart }),
    ...(handlers.onPacket === undefined ? {} : { onPacketReceived: handlers.onPacket }),
    ...(handlers.onProviderEvent === undefined
      ? {}
      : { onProviderEvent: handlers.onProviderEvent }),
    ...(handlers.onStop === undefined ? {} : { onShutdown: handlers.onStop }),
  });
}

function setPluginDefinition(plugin: Plugin, definition: RuntimeExtensionDefinition): void {
  pluginDefinitions.set(plugin, definition);
}

function pluginDefinition(plugin: Plugin): RuntimeExtensionDefinition {
  const definition = pluginDefinitions.get(plugin);
  if (definition === undefined) {
    throw new RangeError("plugin definition is not initialized");
  }

  return definition;
}

function isRuntimeExtensionDefinition(value: unknown): value is RuntimeExtensionDefinition {
  return typeof value === "object" && value !== null && "manifest" in value;
}

function parseIngressName(value: string, field: string): Result<string, AppError> {
  return parseNonEmptyValue(value, field, (normalized) => normalized);
}

function validateWebSocketIngress(
  ingress: WebSocketIngressInit,
  index: number,
): Result<WebSocketIngress, AppError> {
  const name = parseIngressName(ingress.name ?? `websocket-${index + 1}`, "ingress.name");
  if (isErr(name)) {
    return name;
  }

  const url = webSocketUrl(ingress.url);
  if (isErr(url)) {
    return err(
      appError(AppErrorKind.ValidationError, "ingress.url", url.error.message, ingress.url),
    );
  }

  const stream = ingress.stream ?? WebSocketIngressStream.Transactions;
  if (!webSocketIngressStreams.includes(stream)) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "ingress.stream",
        "ingress.stream must be a supported websocket stream",
        String(stream),
      ),
    );
  }
  if (ingress.commitment !== undefined && !providerCommitments.includes(ingress.commitment)) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "ingress.commitment",
        "ingress.commitment must be a supported provider commitment",
        String(ingress.commitment),
      ),
    );
  }
  if (ingress.readiness !== undefined && !providerIngressReadiness.includes(ingress.readiness)) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "ingress.readiness",
        "ingress.readiness must be a supported provider readiness policy",
        String(ingress.readiness),
      ),
    );
  }
  if (ingress.role !== undefined && !providerIngressRoles.includes(ingress.role)) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "ingress.role",
        "ingress.role must be a supported provider ingress role",
        String(ingress.role),
      ),
    );
  }
  if (
    ingress.priority !== undefined &&
    (!Number.isInteger(ingress.priority) || ingress.priority < 0 || ingress.priority > 65_535)
  ) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "ingress.priority",
        "ingress.priority must be an integer between 0 and 65535",
        String(ingress.priority),
      ),
    );
  }

  const parseOptionalField = (
    value: string | undefined,
    field: string,
  ): Result<string | undefined, AppError> => {
    if (value === undefined) {
      return ok(value);
    }

    const parsed = parseNonEmptyValue(value, field, (normalized) => normalized);
    if (isErr(parsed)) {
      return parsed;
    }

    return ok(parsed.value);
  };
  const signature = parseOptionalField(ingress.signature, "ingress.signature");
  if (isErr(signature)) {
    return signature;
  }
  const account = parseOptionalField(ingress.account, "ingress.account");
  if (isErr(account)) {
    return account;
  }
  const programId = parseOptionalField(ingress.programId, "ingress.programId");
  if (isErr(programId)) {
    return programId;
  }
  const mentions = parseOptionalField(ingress.mentions, "ingress.mentions");
  if (isErr(mentions)) {
    return mentions;
  }

  const parseFilterList = (
    values: readonly string[] | undefined,
    field: string,
  ): Result<readonly string[], AppError> => {
    const normalized: string[] = [];
    for (const value of values ?? []) {
      const parsed = parseNonEmptyValue(value, field, (normalizedValue) => normalizedValue);
      if (isErr(parsed)) {
        return parsed;
      }
      normalized.push(parsed.value);
    }

    return ok(normalized);
  };
  const accountInclude = parseFilterList(ingress.accountInclude, "ingress.accountInclude");
  if (isErr(accountInclude)) {
    return accountInclude;
  }
  const accountExclude = parseFilterList(ingress.accountExclude, "ingress.accountExclude");
  if (isErr(accountExclude)) {
    return accountExclude;
  }
  const accountRequired = parseFilterList(ingress.accountRequired, "ingress.accountRequired");
  if (isErr(accountRequired)) {
    return accountRequired;
  }

  const base = {
    kind: IngressKind.WebSocket,
    name: name.value,
    url: url.value,
    stream,
    ...(ingress.commitment === undefined ? {} : { commitment: ingress.commitment }),
    readiness: ingress.readiness ?? ProviderIngressReadiness.Required,
    role: ingress.role ?? ProviderIngressRole.Primary,
    ...(ingress.priority === undefined ? {} : { priority: ingress.priority }),
  } as const;

  const rejectUnsupportedField = (field: string, message: string, received?: string) =>
    err(appError(AppErrorKind.ValidationError, field, message, received));

  switch (stream) {
    case WebSocketIngressStream.Transactions:
      if (account.value !== undefined) {
        return rejectUnsupportedField(
          "ingress.account",
          "ingress.account is only supported for websocket account streams",
          account.value,
        );
      }
      if (programId.value !== undefined) {
        return rejectUnsupportedField(
          "ingress.programId",
          "ingress.programId is only supported for websocket program streams",
          programId.value,
        );
      }
      if (mentions.value !== undefined) {
        return rejectUnsupportedField(
          "ingress.mentions",
          "ingress.mentions is only supported for websocket logs mention filters",
          mentions.value,
        );
      }
      if (ingress.logsFilter !== undefined) {
        return rejectUnsupportedField(
          "ingress.logsFilter",
          "ingress.logsFilter is only supported for websocket logs streams",
          String(ingress.logsFilter),
        );
      }

      return ok({
        ...base,
        ...(ingress.vote === undefined ? {} : { vote: ingress.vote }),
        ...(ingress.failed === undefined ? {} : { failed: ingress.failed }),
        ...(signature.value === undefined ? {} : { signature: signature.value }),
        accountInclude: accountInclude.value,
        accountExclude: accountExclude.value,
        accountRequired: accountRequired.value,
      });
    case WebSocketIngressStream.Logs: {
      if (account.value !== undefined) {
        return rejectUnsupportedField(
          "ingress.account",
          "ingress.account is only supported for websocket account streams",
          account.value,
        );
      }
      if (programId.value !== undefined) {
        return rejectUnsupportedField(
          "ingress.programId",
          "ingress.programId is only supported for websocket program streams",
          programId.value,
        );
      }
      if (ingress.vote !== undefined) {
        return rejectUnsupportedField(
          "ingress.vote",
          "ingress.vote is only supported for websocket transaction streams",
          String(ingress.vote),
        );
      }
      if (ingress.failed !== undefined) {
        return rejectUnsupportedField(
          "ingress.failed",
          "ingress.failed is only supported for websocket transaction streams",
          String(ingress.failed),
        );
      }
      if (signature.value !== undefined) {
        return rejectUnsupportedField(
          "ingress.signature",
          "ingress.signature is only supported for websocket transaction streams",
          signature.value,
        );
      }
      if (
        accountInclude.value.length > 0 ||
        accountExclude.value.length > 0 ||
        accountRequired.value.length > 0
      ) {
        return rejectUnsupportedField(
          "ingress.accountInclude",
          "transaction account filters are only supported for websocket transaction streams",
        );
      }

      const logsFilter = ingress.logsFilter ?? WebSocketLogsFilterKind.All;
      if (!webSocketLogsFilters.includes(logsFilter)) {
        return rejectUnsupportedField(
          "ingress.logsFilter",
          "ingress.logsFilter must be a supported websocket logs filter",
          String(logsFilter),
        );
      }
      if (logsFilter === WebSocketLogsFilterKind.Mentions && mentions.value === undefined) {
        return rejectUnsupportedField(
          "ingress.mentions",
          "ingress.mentions is required when ingress.logsFilter is Mentions",
        );
      }
      if (logsFilter !== WebSocketLogsFilterKind.Mentions && mentions.value !== undefined) {
        return rejectUnsupportedField(
          "ingress.mentions",
          "ingress.mentions is only supported when ingress.logsFilter is Mentions",
          mentions.value,
        );
      }

      return ok({
        ...base,
        logsFilter,
        ...(mentions.value === undefined ? {} : { mentions: mentions.value }),
      });
    }
    case WebSocketIngressStream.Account:
      if (account.value === undefined) {
        return rejectUnsupportedField(
          "ingress.account",
          "ingress.account is required for websocket account streams",
        );
      }
      if (programId.value !== undefined) {
        return rejectUnsupportedField(
          "ingress.programId",
          "ingress.programId is only supported for websocket program streams",
          programId.value,
        );
      }
      if (mentions.value !== undefined || ingress.logsFilter !== undefined) {
        return rejectUnsupportedField(
          "ingress.logsFilter",
          "websocket logs filters are only supported for websocket logs streams",
        );
      }
      if (
        ingress.vote !== undefined ||
        ingress.failed !== undefined ||
        signature.value !== undefined ||
        accountInclude.value.length > 0 ||
        accountExclude.value.length > 0 ||
        accountRequired.value.length > 0
      ) {
        return rejectUnsupportedField(
          "ingress.vote",
          "transaction-only websocket filters are not supported for websocket account streams",
        );
      }

      return ok({
        ...base,
        account: account.value,
      });
    case WebSocketIngressStream.Program:
      if (programId.value === undefined) {
        return rejectUnsupportedField(
          "ingress.programId",
          "ingress.programId is required for websocket program streams",
        );
      }
      if (account.value !== undefined) {
        return rejectUnsupportedField(
          "ingress.account",
          "ingress.account is only supported for websocket account streams",
          account.value,
        );
      }
      if (mentions.value !== undefined || ingress.logsFilter !== undefined) {
        return rejectUnsupportedField(
          "ingress.logsFilter",
          "websocket logs filters are only supported for websocket logs streams",
        );
      }
      if (
        ingress.vote !== undefined ||
        ingress.failed !== undefined ||
        signature.value !== undefined ||
        accountInclude.value.length > 0 ||
        accountExclude.value.length > 0 ||
        accountRequired.value.length > 0
      ) {
        return rejectUnsupportedField(
          "ingress.vote",
          "transaction-only websocket filters are not supported for websocket program streams",
        );
      }

      return ok({
        ...base,
        programId: programId.value,
      });
    default:
      return rejectUnsupportedField(
        "ingress.stream",
        "ingress.stream must be a supported websocket stream",
        String(stream),
      );
  }
}

function validateGrpcIngress(
  ingress: GrpcIngressInit,
  index: number,
): Result<GrpcIngress, AppError> {
  const name = parseIngressName(ingress.name ?? `grpc-${index + 1}`, "ingress.name");
  if (isErr(name)) {
    return name;
  }

  const endpoint = parseNonEmptyValue(ingress.endpoint, "ingress.endpoint", (value) => value);
  if (isErr(endpoint)) {
    return endpoint;
  }

  const stream = ingress.stream ?? GrpcIngressStream.Transactions;
  if (!grpcIngressStreams.includes(stream)) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "ingress.stream",
        "ingress.stream must be a supported gRPC stream",
        String(stream),
      ),
    );
  }
  if (ingress.commitment !== undefined && !providerCommitments.includes(ingress.commitment)) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "ingress.commitment",
        "ingress.commitment must be a supported provider commitment",
        String(ingress.commitment),
      ),
    );
  }
  if (ingress.readiness !== undefined && !providerIngressReadiness.includes(ingress.readiness)) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "ingress.readiness",
        "ingress.readiness must be a supported provider readiness policy",
        String(ingress.readiness),
      ),
    );
  }
  if (ingress.role !== undefined && !providerIngressRoles.includes(ingress.role)) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "ingress.role",
        "ingress.role must be a supported provider ingress role",
        String(ingress.role),
      ),
    );
  }
  if (
    ingress.priority !== undefined &&
    (!Number.isInteger(ingress.priority) || ingress.priority < 0 || ingress.priority > 65_535)
  ) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "ingress.priority",
        "ingress.priority must be an integer between 0 and 65535",
        String(ingress.priority),
      ),
    );
  }

  const xToken =
    ingress.xToken === undefined
      ? undefined
      : parseNonEmptyValue(ingress.xToken, "ingress.xToken", (value) => value);
  if (xToken !== undefined && isErr(xToken)) {
    return xToken;
  }

  const signature =
    ingress.signature === undefined
      ? undefined
      : parseNonEmptyValue(ingress.signature, "ingress.signature", (value) => value);
  if (signature !== undefined && isErr(signature)) {
    return signature;
  }

  const parseFilterList = (
    values: readonly string[] | undefined,
    field: string,
  ): Result<readonly string[], AppError> => {
    const normalized: string[] = [];
    for (const value of values ?? []) {
      const parsed = parseNonEmptyValue(value, field, (normalizedValue) => normalizedValue);
      if (isErr(parsed)) {
        return parsed;
      }
      normalized.push(parsed.value);
    }

    return ok(normalized);
  };
  const accountInclude = parseFilterList(ingress.accountInclude, "ingress.accountInclude");
  if (isErr(accountInclude)) {
    return accountInclude;
  }
  const accountExclude = parseFilterList(ingress.accountExclude, "ingress.accountExclude");
  if (isErr(accountExclude)) {
    return accountExclude;
  }
  const accountRequired = parseFilterList(ingress.accountRequired, "ingress.accountRequired");
  if (isErr(accountRequired)) {
    return accountRequired;
  }
  const accounts = parseFilterList(ingress.accounts, "ingress.accounts");
  if (isErr(accounts)) {
    return accounts;
  }
  const owners = parseFilterList(ingress.owners, "ingress.owners");
  if (isErr(owners)) {
    return owners;
  }

  return ok({
    kind: IngressKind.Grpc,
    name: name.value,
    endpoint: endpoint.value,
    tls: ingress.tls ?? true,
    stream,
    ...(xToken === undefined ? {} : { xToken: xToken.value }),
    ...(ingress.commitment === undefined ? {} : { commitment: ingress.commitment }),
    ...(ingress.vote === undefined ? {} : { vote: ingress.vote }),
    ...(ingress.failed === undefined ? {} : { failed: ingress.failed }),
    ...(signature === undefined ? {} : { signature: signature.value }),
    accountInclude: accountInclude.value,
    accountExclude: accountExclude.value,
    accountRequired: accountRequired.value,
    accounts: accounts.value,
    owners: owners.value,
    requireTransactionSignature: ingress.requireTransactionSignature ?? false,
    readiness: ingress.readiness ?? ProviderIngressReadiness.Required,
    role: ingress.role ?? ProviderIngressRole.Primary,
    ...(ingress.priority === undefined ? {} : { priority: ingress.priority }),
  });
}

function validateGossipIngress(
  ingress: GossipIngressInit,
  index: number,
): Result<GossipIngress, AppError> {
  const name = parseIngressName(ingress.name ?? `gossip-${index + 1}`, "ingress.name");
  if (isErr(name)) {
    return name;
  }

  const bindAddress =
    ingress.bindAddress === undefined ? undefined : socketAddress(ingress.bindAddress);
  if (bindAddress !== undefined && isErr(bindAddress)) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "ingress.bindAddress",
        bindAddress.error.message,
        ingress.bindAddress,
      ),
    );
  }

  if (ingress.runtimeMode !== undefined && !gossipRuntimeModes.includes(ingress.runtimeMode)) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "ingress.runtimeMode",
        "ingress.runtimeMode must be a supported gossip runtime mode",
        String(ingress.runtimeMode),
      ),
    );
  }

  const entrypoints: string[] = [];
  for (const value of ingress.entrypoints ?? []) {
    const parsed = parseNonEmptyValue(value, "ingress.entrypoints", (normalized) => normalized);
    if (isErr(parsed)) {
      return parsed;
    }
    entrypoints.push(parsed.value);
  }

  return ok({
    kind: IngressKind.Gossip,
    name: name.value,
    ...(bindAddress === undefined ? {} : { bindAddress: bindAddress.value }),
    entrypoints,
    ...(ingress.runtimeMode === undefined ? {} : { runtimeMode: ingress.runtimeMode }),
    ...(ingress.entrypointPinned === undefined
      ? {}
      : { entrypointPinned: ingress.entrypointPinned }),
  });
}

function validateDirectShredsIngress(
  ingress: DirectShredsIngressInit,
  index: number,
): Result<DirectShredsIngress, AppError> {
  const name = parseIngressName(ingress.name ?? `direct-shreds-${index + 1}`, "ingress.name");
  if (isErr(name)) {
    return name;
  }

  const bindAddress = socketAddress(ingress.bindAddress);
  if (isErr(bindAddress)) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "ingress.bindAddress",
        bindAddress.error.message,
        ingress.bindAddress,
      ),
    );
  }

  let kernelBypass: KernelBypassConfig | undefined;
  if (ingress.kernelBypass !== undefined) {
    const parsedKernelBypass = validateKernelBypassConfig(ingress.kernelBypass);
    if (isErr(parsedKernelBypass)) {
      return parsedKernelBypass;
    }
    kernelBypass = parsedKernelBypass.value;
  }

  return ok({
    kind: IngressKind.DirectShreds,
    name: name.value,
    bindAddress: bindAddress.value,
    ...(ingress.trustMode === undefined ? {} : { trustMode: ingress.trustMode }),
    ...(kernelBypass === undefined ? {} : { kernelBypass }),
  });
}

function validateKernelBypassConfig(
  config: KernelBypassInit,
): Result<KernelBypassConfig, AppError> {
  const networkInterface = parseNonEmptyValue(
    config.interface,
    "ingress.kernelBypass.interface",
    (value) => value,
  );
  if (isErr(networkInterface)) {
    return networkInterface;
  }

  const validateNonNegativeInteger = (value: number, field: string): Result<number, AppError> => {
    if (!Number.isInteger(value) || value < 0) {
      return err(
        appError(
          AppErrorKind.ValidationError,
          field,
          `${field} must be a non-negative integer`,
          String(value),
        ),
      );
    }

    return ok(value);
  };
  const validatePositiveIntegerField = (value: number, field: string): Result<number, AppError> => {
    if (!Number.isInteger(value) || value <= 0) {
      return err(
        appError(
          AppErrorKind.ValidationError,
          field,
          `${field} must be a positive integer`,
          String(value),
        ),
      );
    }

    return ok(value);
  };

  const queueId = validateNonNegativeInteger(
    config.queueId ?? defaultKernelBypassQueueId,
    "ingress.kernelBypass.queueId",
  );
  if (isErr(queueId)) {
    return queueId;
  }
  const batchSize = validatePositiveIntegerField(
    config.batchSize ?? defaultKernelBypassBatchSize,
    "ingress.kernelBypass.batchSize",
  );
  if (isErr(batchSize)) {
    return batchSize;
  }
  const umemFrameCount = validatePositiveIntegerField(
    config.umemFrameCount ?? defaultKernelBypassUmemFrameCount,
    "ingress.kernelBypass.umemFrameCount",
  );
  if (isErr(umemFrameCount)) {
    return umemFrameCount;
  }
  const ringDepth = validatePositiveIntegerField(
    config.ringDepth ?? defaultKernelBypassRingDepth,
    "ingress.kernelBypass.ringDepth",
  );
  if (isErr(ringDepth)) {
    return ringDepth;
  }
  const pollTimeoutMs = validatePositiveIntegerField(
    config.pollTimeoutMs ?? defaultKernelBypassPollTimeoutMs,
    "ingress.kernelBypass.pollTimeoutMs",
  );
  if (isErr(pollTimeoutMs)) {
    return pollTimeoutMs;
  }

  return ok({
    interface: networkInterface.value,
    queueId: queueId.value,
    batchSize: batchSize.value,
    umemFrameCount: umemFrameCount.value,
    ringDepth: ringDepth.value,
    pollTimeoutMs: pollTimeoutMs.value,
  });
}

function validateIngress(
  ingress: readonly IngressInit[] = [],
): Result<readonly Ingress[], AppError> {
  const normalized: Ingress[] = [];
  const names = new Set<string>();

  for (const [index, value] of ingress.entries()) {
    let parsed: Result<Ingress, AppError>;

    switch (value.kind) {
      case IngressKind.WebSocket:
        parsed = validateWebSocketIngress(value, index);
        break;
      case IngressKind.Grpc:
        parsed = validateGrpcIngress(value, index);
        break;
      case IngressKind.Gossip:
        parsed = validateGossipIngress(value, index);
        break;
      case IngressKind.DirectShreds:
        parsed = validateDirectShredsIngress(value, index);
        break;
      default:
        return err(
          appError(
            AppErrorKind.ValidationError,
            "ingress.kind",
            "ingress.kind must be WebSocket, Grpc, Gossip, or DirectShreds",
            String((value as { readonly kind?: unknown }).kind),
          ),
        );
    }

    if (isErr(parsed)) {
      return parsed;
    }

    if (names.has(parsed.value.name)) {
      return err(
        appError(
          AppErrorKind.ValidationError,
          "ingress.name",
          `ingress ${parsed.value.name} is registered more than once`,
          parsed.value.name,
        ),
      );
    }

    names.add(parsed.value.name);
    normalized.push(parsed.value);
  }

  return ok(normalized);
}

function validateFanIn(
  fanIn: FanInInit | undefined,
  ingress: readonly Ingress[],
): Result<FanIn | undefined, AppError> {
  const noFanIn: FanIn | undefined = undefined;
  const rawRuntimeComposition = ingress.length > 1 && isRawRuntimeComposition(ingress);

  if (ingress.length <= 1) {
    if (fanIn === undefined) {
      return ok(noFanIn);
    }
  } else if (rawRuntimeComposition) {
    if (fanIn === undefined) {
      return ok(noFanIn);
    }
    return err(
      appError(
        AppErrorKind.ValidationError,
        "fanIn",
        "fanIn is not used for direct-shreds plus gossip runtime composition",
      ),
    );
  } else if (fanIn === undefined) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "fanIn",
        "fanIn is required when more than one ingress source is configured",
      ),
    );
  }

  if (fanIn === undefined) {
    return ok(noFanIn);
  }

  const strategy = fanIn.strategy ?? FanInStrategy.FirstSeen;

  return ok({
    strategy,
  });
}

function isRawRuntimeComposition(ingress: readonly Ingress[]): boolean {
  let directShredsCount = 0;
  let gossipCount = 0;

  for (const source of ingress) {
    switch (source.kind) {
      case IngressKind.DirectShreds:
        directShredsCount += 1;
        break;
      case IngressKind.Gossip:
        gossipCount += 1;
        break;
      case IngressKind.WebSocket:
      case IngressKind.Grpc:
        return false;
      default:
        return false;
    }
  }

  return directShredsCount <= 1 && gossipCount <= 1 && directShredsCount + gossipCount >= 2;
}

function resolveRuntimeInput(
  runtime: ObserverRuntimeConfigInput | undefined,
  ingress: readonly Ingress[],
): Result<ObserverRuntimeConfig, AppError> {
  const directShreds = ingress.filter(
    (value): value is DirectShredsIngress => value.kind === IngressKind.DirectShreds,
  );
  const inferredTrustModes = new Set(
    directShreds
      .map((value) => value.trustMode)
      .filter((value): value is ShredTrustMode => value !== undefined),
  );

  if (inferredTrustModes.size > 1) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "ingress",
        "direct shred ingress sources must agree on trustMode unless runtime.shredTrustMode is set explicitly",
      ),
    );
  }

  const inferredTrustMode = inferredTrustModes.size === 1 ? [...inferredTrustModes][0] : undefined;

  let baseRuntime: ObserverRuntimeConfigInput = runtime ?? {};
  if (inferredTrustMode !== undefined) {
    if (runtime === undefined) {
      baseRuntime = {
        shredTrustMode: inferredTrustMode,
      };
    } else if (
      !(runtime instanceof ObserverRuntimeConfig) &&
      runtime.shredTrustMode === undefined
    ) {
      baseRuntime = {
        ...runtime,
        shredTrustMode: inferredTrustMode,
      };
    }
  }

  const config = tryCreateRuntimeConfig(baseRuntime);
  if (isErr(config)) {
    return err(appError(AppErrorKind.ValidationError, "runtime", config.error.message));
  }

  return ok(config.value);
}

function toPluginNameKey(value: ExtensionName): string {
  return value;
}

interface AppState {
  readonly name: AppName;
  readonly runtime: ObserverRuntimeConfig;
  readonly ingress: readonly Ingress[];
  readonly fanIn: FanIn | undefined;
  readonly plugins: readonly Plugin[];
  readonly pluginsByName: ReadonlyMap<string, Plugin>;
}

interface RuntimeHostPluginWorkerConfig {
  readonly name: ExtensionName;
  readonly manifest: RuntimeExtensionWorkerManifest;
  readonly command: string;
  readonly args: readonly string[];
  readonly environment: Readonly<Record<string, string>>;
  readonly providerEvents: boolean;
}

interface RuntimeHostConfig {
  readonly sdkVersion: string;
  readonly appName: AppName;
  readonly runtimeEnvironment: Readonly<Record<string, string>>;
  readonly ingress: readonly Ingress[];
  readonly fanIn: FanIn | undefined;
  readonly pluginWorkers: readonly RuntimeHostPluginWorkerConfig[];
}

interface ChildExit {
  readonly code: number | null;
  readonly signal: NodeJS.Signals | null;
}

function waitForChildExit(child: ChildProcess): Promise<Result<ChildExit, Error>> {
  return new Promise((resolve) => {
    const onError = (error: Error) => {
      child.off("exit", onExit);
      resolve(err(error));
    };
    const onExit = (code: number | null, signal: NodeJS.Signals | null) => {
      child.off("error", onError);
      resolve(ok({ code, signal }));
    };

    child.once("error", onError);
    child.once("exit", onExit);
  });
}

function invokePluginReady(plugin: Plugin): Promise<Result<RuntimeExtensionAck, PluginError>> {
  const onReady = pluginDefinition(plugin).onReady;
  if (onReady === undefined) {
    return Promise.resolve(ok(runtimeExtensionAck()));
  }

  return Promise.resolve(
    onReady({
      extensionName: plugin.name,
    }),
  );
}

function invokePluginShutdown(plugin: Plugin): Promise<Result<RuntimeExtensionAck, PluginError>> {
  const onShutdown = pluginDefinition(plugin).onShutdown;
  if (onShutdown === undefined) {
    return Promise.resolve(ok(runtimeExtensionAck()));
  }

  return Promise.resolve(
    onShutdown({
      extensionName: plugin.name,
    }),
  );
}

async function shutdownPlugins(
  startedPlugins: readonly Plugin[],
): Promise<Result<RuntimeExtensionAck, PluginError>> {
  const shutdownResults = await Promise.all(
    startedPlugins.map((plugin) => invokePluginShutdown(plugin)),
  );
  for (const shutdown of shutdownResults) {
    if (isErr(shutdown)) {
      return shutdown;
    }
  }

  return ok(runtimeExtensionAck());
}

function currentNodeAppArgs(): Result<readonly string[], AppError> {
  const entrypoint = process.argv[1];
  if (entrypoint === undefined || entrypoint.trim() === "") {
    return err(
      appError(
        AppErrorKind.ValidationError,
        "process.argv[1]",
        "app.run() needs a Node.js file entrypoint when delegating to the runtime host",
      ),
    );
  }

  return ok([...process.execArgv, ...process.argv.slice(1)]);
}

function createRuntimeHostConfig(state: AppState): Result<RuntimeHostConfig, AppError> {
  const appArgs = currentNodeAppArgs();
  if (isErr(appArgs)) {
    return appArgs;
  }

  return ok({
    sdkVersion: typeScriptSdkVersion,
    appName: state.name,
    runtimeEnvironment: state.runtime.toEnvironmentRecord({ includeDefaults: true }),
    ingress: state.ingress,
    fanIn: state.fanIn,
    pluginWorkers: state.plugins.map((plugin) => ({
      name: plugin.name,
      manifest: pluginDefinition(plugin).manifest,
      command: process.execPath,
      args: appArgs.value,
      environment: {
        [internalPluginWorkerEnvVarName]: plugin.name,
        [internalPluginWorkerModeEnvVarName]: internalPluginWorkerModeEnvValue,
      },
      providerEvents: pluginDefinition(plugin).onProviderEvent !== undefined,
    })),
  });
}

function runtimeHostExecutableName(): string {
  return process.platform === "win32"
    ? `${runtimeHostBinaryBaseName}.exe`
    : runtimeHostBinaryBaseName;
}

function packagedRuntimeHostPath(): string {
  return join(
    import.meta.dirname,
    "native",
    `${process.platform}-${process.arch}`,
    runtimeHostExecutableName(),
  );
}

function repoRuntimeHostPaths(): readonly string[] {
  return [
    join(import.meta.dirname, "..", "..", "..", "target", "debug", runtimeHostExecutableName()),
    join(import.meta.dirname, "..", "..", "..", "target", "release", runtimeHostExecutableName()),
  ];
}

function runtimeHostPackageName(): string | undefined {
  if (process.platform === "darwin") {
    if (process.arch === "arm64") {
      return "@lythaeon-sof/sdk-native-darwin-arm64";
    }
    if (process.arch === "x64") {
      return "@lythaeon-sof/sdk-native-darwin-x64";
    }
    return undefined;
  }

  if (process.platform === "linux") {
    if (process.arch === "arm64") {
      return "@lythaeon-sof/sdk-native-linux-arm64";
    }
    if (process.arch === "x64") {
      return "@lythaeon-sof/sdk-native-linux-x64";
    }
    return undefined;
  }

  if (process.platform === "win32") {
    if (process.arch === "arm64") {
      return "@lythaeon-sof/sdk-native-win32-arm64";
    }
    if (process.arch === "x64") {
      return "@lythaeon-sof/sdk-native-win32-x64";
    }
  }

  return undefined;
}

function installedRuntimeHostPath(): string | undefined {
  const packageName = runtimeHostPackageName();
  if (packageName === undefined) {
    return undefined;
  }

  try {
    const packageJsonPath = require.resolve(`${packageName}/package.json`);
    return join(packageJsonPath, "..", "vendor", runtimeHostExecutableName());
  } catch {
    return undefined;
  }
}

function makeRuntimeHostExecutable(candidate: string): Result<string, AppError> {
  try {
    if (process.platform !== "win32") {
      chmodSync(candidate, 0o755);
    }
    return ok(candidate);
  } catch (error) {
    return err(
      appError(
        AppErrorKind.ValidationError,
        runtimeHostBinaryEnvVarName,
        `runtime host binary exists but could not be made executable: ${
          error instanceof Error ? error.message : String(error)
        }`,
        candidate,
      ),
    );
  }
}

function runtimeHostBinary(): Result<string, AppError> {
  const binary = process.env[runtimeHostBinaryEnvVarName]?.trim();
  if (binary !== undefined && binary !== "") {
    return ok(binary);
  }

  const candidates = [
    installedRuntimeHostPath(),
    packagedRuntimeHostPath(),
    ...repoRuntimeHostPaths(),
  ].filter((candidate): candidate is string => candidate !== undefined);
  for (const candidate of candidates) {
    if (existsSync(candidate)) {
      return makeRuntimeHostExecutable(candidate);
    }
  }

  return err(
    appError(
      AppErrorKind.ValidationError,
      runtimeHostBinaryEnvVarName,
      runtimeHostPackageName() === undefined
        ? `a compatible runtime host binary was not found for ${process.platform}-${process.arch}; set ${runtimeHostBinaryEnvVarName} or provide one of ${candidates.join(", ")}`
        : `a compatible runtime host binary was not found; expected optional dependency ${runtimeHostPackageName()} or one of ${candidates.join(", ")}`,
    ),
  );
}

async function runRuntimeHost(
  config: RuntimeHostConfig,
  options: AppRunOptions,
): Promise<Result<RuntimeExtensionAck, AppRunError>> {
  const hostBinary = runtimeHostBinary();
  if (isErr(hostBinary)) {
    return hostBinary;
  }
  if (options.signal?.aborted === true) {
    return ok(runtimeExtensionAck());
  }

  const tempDir = await mkdtemp(join(tmpdir(), "sof-sdk-runtime-"));
  const configPath = join(tempDir, "runtime-host.json");
  await writeFile(configPath, `${JSON.stringify(config)}\n`, "utf8");

  try {
    const child = spawn(hostBinary.value, [configPath], {
      env: process.env,
      stdio: "inherit",
    });
    let aborted = false;
    let forceKillTimer: NodeJS.Timeout | undefined;
    const abort = () => {
      aborted = true;
      child.kill("SIGTERM");
      forceKillTimer = setTimeout(() => {
        child.kill("SIGKILL");
      }, 2_000);
      forceKillTimer.unref();
    };
    options.signal?.addEventListener("abort", abort, { once: true });

    const exit = await waitForChildExit(child);
    try {
      if (isErr(exit)) {
        return err(
          appError(
            AppErrorKind.ValidationError,
            runtimeHostBinaryEnvVarName,
            `failed to start runtime host: ${exit.error.message}`,
            hostBinary.value,
          ),
        );
      }

      if (aborted || exit.value.code === 0) {
        return ok(runtimeExtensionAck());
      }

      return err(
        appError(
          AppErrorKind.ValidationError,
          runtimeHostBinaryEnvVarName,
          `runtime host exited with ${
            exit.value.signal === null
              ? `code ${String(exit.value.code)}`
              : `signal ${exit.value.signal}`
          }`,
          hostBinary.value,
        ),
      );
    } finally {
      options.signal?.removeEventListener("abort", abort);
      if (forceKillTimer !== undefined) {
        clearTimeout(forceKillTimer);
      }
    }
  } finally {
    await rm(tempDir, { force: true, recursive: true });
  }
}

function createAppState(init: AppInit): Result<AppState, AppError> {
  const ingress = validateIngress(init.ingress);
  if (isErr(ingress)) {
    return ingress;
  }

  const fanIn = validateFanIn(init.fanIn, ingress.value);
  if (isErr(fanIn)) {
    return fanIn;
  }

  const plugins = [...(init.plugins ?? []), ...(init.extensions ?? [])];
  const pluginNames = new Set<string>();
  const pluginsByName = new Map<string, Plugin>();

  for (const plugin of plugins) {
    const nameKey = toPluginNameKey(plugin.name);
    if (pluginNames.has(nameKey)) {
      return err(
        appError(
          AppErrorKind.DuplicatePlugin,
          "plugins",
          `plugin ${nameKey} is registered more than once`,
          nameKey,
        ),
      );
    }

    pluginNames.add(nameKey);
    pluginsByName.set(nameKey, plugin);
  }

  const runtime = resolveRuntimeInput(init.runtime, ingress.value);
  if (isErr(runtime)) {
    return runtime;
  }

  const resolvedName =
    typeof init.name === "string"
      ? parseAppName(init.name)
      : init.name === undefined
        ? parseAppName(defaultAppNameFromPlugins(plugins))
        : ok(init.name);
  if (isErr(resolvedName)) {
    return resolvedName;
  }

  return ok({
    name: resolvedName.value,
    runtime: runtime.value,
    ingress: ingress.value,
    fanIn: fanIn.value,
    plugins,
    pluginsByName,
  });
}

export class Plugin {
  constructor(init: Plugin | PluginInit);
  constructor(init: RuntimeExtensionDefinition, internalDefinition: true);
  constructor(init: Plugin | PluginInit | RuntimeExtensionDefinition, _internalDefinition?: true) {
    if (arguments[1] === true) {
      if (!isRuntimeExtensionDefinition(init)) {
        throw new RangeError("plugin definition is not initialized");
      }
      setPluginDefinition(this, init);
      return;
    }

    if (init instanceof Plugin) {
      setPluginDefinition(this, pluginDefinition(init));
      return;
    }

    const definition = createPluginDefinition(init);
    if (isErr(definition)) {
      throw new RangeError(definition.error.message);
    }

    setPluginDefinition(this, definition.value);
  }

  static create(init: Plugin | PluginInit): Result<Plugin, PluginError> {
    if (init instanceof Plugin) {
      return ok(init);
    }

    const definition = createPluginDefinition(init);
    return isErr(definition) ? definition : ok(new Plugin(definition.value, true));
  }

  get name(): ExtensionName {
    return pluginDefinition(this).manifest.extensionName;
  }

  get manifest(): ExtensionManifest {
    return pluginDefinition(this).manifest.manifest;
  }
}

export { Plugin as Extension };

export class App {
  readonly #name: AppName;
  readonly #runtime: ObserverRuntimeConfig;
  readonly #ingress: readonly Ingress[];
  readonly #fanIn: FanIn | undefined;
  readonly #plugins: readonly Plugin[];
  readonly #pluginsByName: ReadonlyMap<string, Plugin>;

  constructor(init: App | AppInit) {
    if (init instanceof App) {
      this.#name = init.#name;
      this.#runtime = init.#runtime;
      this.#ingress = init.#ingress;
      this.#fanIn = init.#fanIn;
      this.#plugins = init.#plugins;
      this.#pluginsByName = init.#pluginsByName;
      return;
    }

    const state = createAppState(init);
    if (isErr(state)) {
      throwAppError(state.error);
    }

    this.#name = state.value.name;
    this.#runtime = state.value.runtime;
    this.#ingress = state.value.ingress;
    this.#fanIn = state.value.fanIn;
    this.#plugins = state.value.plugins;
    this.#pluginsByName = state.value.pluginsByName;
  }

  static create(init: App | AppInit): Result<App, AppError> {
    if (init instanceof App) {
      return ok(init);
    }

    const state = createAppState(init);
    if (isErr(state)) {
      return state;
    }

    return ok(new App(init));
  }

  get name(): AppName {
    return this.#name;
  }

  get runtime(): ObserverRuntimeConfig {
    return this.#runtime;
  }

  get ingress(): readonly Ingress[] {
    return this.#ingress;
  }

  get fanIn(): FanIn | undefined {
    return this.#fanIn;
  }

  get plugins(): readonly Plugin[] {
    return this.#plugins;
  }

  get extensions(): readonly Plugin[] {
    return this.#plugins;
  }

  getPlugin(name: string | ExtensionName): Result<Plugin, AppError> {
    const parsedName = typeof name === "string" ? extensionName(name) : ok(name);
    if (isErr(parsedName)) {
      return err(
        appError(
          AppErrorKind.ValidationError,
          "pluginName",
          parsedName.error.message,
          typeof name === "string" ? name : String(name),
        ),
      );
    }

    const plugin = this.#pluginsByName.get(toPluginNameKey(parsedName.value));
    if (plugin !== undefined) {
      return ok(plugin);
    }

    return err(
      appError(
        AppErrorKind.MissingPlugin,
        "pluginName",
        `plugin ${String(parsedName.value)} is not registered in app ${String(this.#name)}`,
        String(parsedName.value),
        this.#plugins.map((pluginValue) => String(pluginValue.name)),
      ),
    );
  }

  #toState(): AppState {
    return {
      name: this.#name,
      runtime: this.#runtime,
      ingress: this.#ingress,
      fanIn: this.#fanIn,
      plugins: this.#plugins,
      pluginsByName: this.#pluginsByName,
    };
  }

  #runInternalPluginWorker(pluginName: string): Promise<Result<RuntimeExtensionAck, AppRunError>> {
    const plugin = this.getPlugin(pluginName);
    if (isErr(plugin)) {
      return Promise.resolve(plugin);
    }

    return runRuntimeExtensionWorkerStdio(pluginDefinition(plugin.value));
  }

  run(options: AppRunOptions = {}): Promise<Result<RuntimeExtensionAck, AppRunError>> {
    return this.runRuntime(options);
  }

  async runRuntime(options: AppRunOptions = {}): Promise<Result<RuntimeExtensionAck, AppRunError>> {
    if (this.#plugins.length === 0) {
      return err(
        appError(AppErrorKind.ValidationError, "plugins", "app must define at least one plugin"),
      );
    }

    const internalWorkerPluginName = process.env[internalPluginWorkerEnvVarName];
    if (
      internalWorkerPluginName !== undefined &&
      process.env[internalPluginWorkerModeEnvVarName] === internalPluginWorkerModeEnvValue
    ) {
      return this.#runInternalPluginWorker(internalWorkerPluginName);
    }

    if (this.#ingress.length === 0) {
      const readyResults = await Promise.all(
        this.#plugins.map((plugin) => invokePluginReady(plugin)),
      );
      for (const ready of readyResults) {
        if (isErr(ready)) {
          return ready;
        }
      }
      const startedPlugins = [...this.#plugins];

      if (options.signal !== undefined) {
        if (options.signal.aborted) {
          return ok(runtimeExtensionAck());
        }

        await new Promise<void>((resolve) => {
          options.signal?.addEventListener(
            "abort",
            () => {
              resolve();
            },
            { once: true },
          );
        });
      }

      return shutdownPlugins(startedPlugins);
    }

    const state = this.#toState();
    const runtimeHostConfig = createRuntimeHostConfig(state);
    if (isErr(runtimeHostConfig)) {
      return runtimeHostConfig;
    }

    return runRuntimeHost(runtimeHostConfig.value, options);
  }
}

export function createBalancedRuntime(
  init: Omit<ObserverRuntimeConfigInput, "runtimeDeliveryProfile"> = {},
): ObserverRuntimeConfig {
  return observerRuntimeConfig({
    ...init,
    runtimeDeliveryProfile: RuntimeDeliveryProfile.Balanced,
  });
}

export function createLatencyOptimizedRuntime(
  init: Omit<ObserverRuntimeConfigInput, "runtimeDeliveryProfile"> = {},
): ObserverRuntimeConfig {
  return observerRuntimeConfig({
    ...init,
    runtimeDeliveryProfile: RuntimeDeliveryProfile.LatencyOptimized,
  });
}

export function createDeliveryDisciplinedRuntime(
  init: Omit<ObserverRuntimeConfigInput, "runtimeDeliveryProfile"> = {},
): ObserverRuntimeConfig {
  return observerRuntimeConfig({
    ...init,
    runtimeDeliveryProfile: RuntimeDeliveryProfile.DeliveryDisciplined,
  });
}
