import { brand, type Brand } from "../brand.js";
import { err, isErr, ok, type Result } from "../result.js";

export enum RuntimeExtensionErrorKind {
  ValidationError = 1,
  CapabilityError = 2,
  CompatibilityError = 3,
  ProtocolError = 4,
  UnhandledException = 5,
}

export interface RuntimeExtensionError {
  readonly kind: RuntimeExtensionErrorKind;
  readonly field: string;
  readonly message: string;
  readonly received?: string;
  readonly cause?: string;
}

export interface RuntimeExtensionAck {
  readonly acknowledged: true;
}

export type RuntimeJsonPrimitive = string | number | boolean | null;
export type RuntimeJsonValue =
  | RuntimeJsonPrimitive
  | readonly RuntimeJsonValue[]
  | {
      readonly [key: string]: RuntimeJsonValue;
    };

export type ExtensionName = Brand<string, "ExtensionName">;
export type ExtensionResourceId = Brand<string, "ExtensionResourceId">;
export type SharedStreamTag = Brand<string, "SharedStreamTag">;
export type SocketAddress = Brand<string, "SocketAddress">;
export type WebSocketUrl = Brand<string, "WebSocketUrl">;

export enum ExtensionCapability {
  BindUdp = 1,
  BindTcp = 2,
  ConnectTcp = 3,
  ConnectWebSocket = 4,
  ObserveObserverIngress = 5,
  ObserveSharedExtensionStream = 6,
}

export enum ExtensionStreamVisibilityTag {
  Private = 1,
  Shared = 2,
}

export type ExtensionStreamVisibility =
  | {
      readonly tag: ExtensionStreamVisibilityTag.Private;
    }
  | {
      readonly tag: ExtensionStreamVisibilityTag.Shared;
      readonly sharedTag: SharedStreamTag;
    };

export enum ExtensionResourceKind {
  UdpListener = 1,
  TcpListener = 2,
  TcpConnector = 3,
  WsConnector = 4,
}

interface ExtensionResourceBase {
  readonly resourceId: ExtensionResourceId;
  readonly visibility: ExtensionStreamVisibility;
  readonly readBufferBytes: number;
}

export interface UdpListenerResourceSpec extends ExtensionResourceBase {
  readonly kind: ExtensionResourceKind.UdpListener;
  readonly bindAddress: SocketAddress;
}

export interface TcpListenerResourceSpec extends ExtensionResourceBase {
  readonly kind: ExtensionResourceKind.TcpListener;
  readonly bindAddress: SocketAddress;
}

export interface TcpConnectorResourceSpec extends ExtensionResourceBase {
  readonly kind: ExtensionResourceKind.TcpConnector;
  readonly remoteAddress: SocketAddress;
}

export interface WebSocketConnectorResourceSpec extends ExtensionResourceBase {
  readonly kind: ExtensionResourceKind.WsConnector;
  readonly url: WebSocketUrl;
}

export type ExtensionResourceSpec =
  | UdpListenerResourceSpec
  | TcpListenerResourceSpec
  | TcpConnectorResourceSpec
  | WebSocketConnectorResourceSpec;

export enum RuntimePacketSourceKind {
  ObserverIngress = 1,
  ExtensionResource = 2,
}

export enum RuntimePacketTransport {
  Udp = 1,
  Tcp = 2,
  WebSocket = 3,
}

export enum RuntimePacketEventClass {
  Packet = 1,
  ConnectionClosed = 2,
}

export enum RuntimeWebSocketFrameType {
  Text = 1,
  Binary = 2,
  Ping = 3,
  Pong = 4,
}

export interface RuntimePacketSource {
  readonly kind: RuntimePacketSourceKind;
  readonly transport: RuntimePacketTransport;
  readonly eventClass: RuntimePacketEventClass;
  readonly ownerExtension?: ExtensionName;
  readonly resourceId?: ExtensionResourceId;
  readonly sharedTag?: SharedStreamTag;
  readonly webSocketFrameType?: RuntimeWebSocketFrameType;
  readonly localAddress?: SocketAddress;
  readonly remoteAddress?: SocketAddress;
}

export interface RuntimePacketEvent {
  readonly source: RuntimePacketSource;
  readonly bytes: Uint8Array;
  readonly observedUnixMs: number;
}

export enum RuntimeProviderEventKind {
  Transaction = 1,
  TransactionLog = 2,
  TransactionStatus = 3,
  AccountUpdate = 4,
  BlockMeta = 5,
  SlotStatus = 6,
  RecentBlockhash = 7,
}

export enum RuntimeProviderCommitmentStatus {
  Processed = 1,
  Confirmed = 2,
  Finalized = 3,
}

export enum RuntimeProviderTransactionKind {
  VoteOnly = 1,
  Mixed = 2,
  NonVote = 3,
}

export enum RuntimeProviderSlotStatus {
  Processed = 1,
  Confirmed = 2,
  Finalized = 3,
  Orphaned = 4,
}

export interface RuntimeProviderSource {
  readonly kind: string;
  readonly instance: string;
  readonly priority: number;
  readonly role: string;
  readonly arbitration: string;
}

interface RuntimeProviderEventBase {
  readonly kind: RuntimeProviderEventKind;
  readonly slot: number;
  readonly providerSource?: RuntimeProviderSource;
}

interface RuntimeProviderCommittedEventBase extends RuntimeProviderEventBase {
  readonly commitmentStatus: RuntimeProviderCommitmentStatus;
  readonly confirmedSlot?: number;
  readonly finalizedSlot?: number;
}

export interface RuntimeProviderTransactionEvent extends RuntimeProviderCommittedEventBase {
  readonly kind: RuntimeProviderEventKind.Transaction;
  readonly signature?: string;
  readonly transactionKind: RuntimeProviderTransactionKind;
  readonly transactionBase64?: string;
}

export interface RuntimeProviderTransactionLogEvent extends RuntimeProviderCommittedEventBase {
  readonly kind: RuntimeProviderEventKind.TransactionLog;
  readonly signature: string;
  readonly err?: RuntimeJsonValue;
  readonly logs: readonly string[];
  readonly matchedFilter?: string;
}

export interface RuntimeProviderTransactionStatusEvent extends RuntimeProviderCommittedEventBase {
  readonly kind: RuntimeProviderEventKind.TransactionStatus;
  readonly signature: string;
  readonly isVote: boolean;
  readonly index?: number;
  readonly err?: string;
}

export interface RuntimeProviderAccountUpdateEvent extends RuntimeProviderCommittedEventBase {
  readonly kind: RuntimeProviderEventKind.AccountUpdate;
  readonly pubkey: string;
  readonly owner: string;
  readonly lamports: number;
  readonly executable: boolean;
  readonly rentEpoch: number;
  readonly dataBase64: string;
  readonly writeVersion?: number;
  readonly txnSignature?: string;
  readonly isStartup: boolean;
  readonly matchedFilter?: string;
}

export interface RuntimeProviderBlockMetaEvent extends RuntimeProviderCommittedEventBase {
  readonly kind: RuntimeProviderEventKind.BlockMeta;
  readonly blockhash: string;
  readonly parentSlot: number;
  readonly parentBlockhash: string;
  readonly blockTime?: number;
  readonly blockHeight?: number;
  readonly executedTransactionCount: number;
  readonly entriesCount: number;
}

export interface RuntimeProviderSlotStatusEvent extends RuntimeProviderEventBase {
  readonly kind: RuntimeProviderEventKind.SlotStatus;
  readonly parentSlot?: number;
  readonly previousStatus?: RuntimeProviderSlotStatus;
  readonly status: RuntimeProviderSlotStatus;
  readonly tipSlot?: number;
  readonly confirmedSlot?: number;
  readonly finalizedSlot?: number;
}

export interface RuntimeProviderRecentBlockhashEvent extends RuntimeProviderEventBase {
  readonly kind: RuntimeProviderEventKind.RecentBlockhash;
  readonly recentBlockhash: string;
  readonly datasetTxCount: number;
}

export type RuntimeProviderEvent =
  | RuntimeProviderTransactionEvent
  | RuntimeProviderTransactionLogEvent
  | RuntimeProviderTransactionStatusEvent
  | RuntimeProviderAccountUpdateEvent
  | RuntimeProviderBlockMetaEvent
  | RuntimeProviderSlotStatusEvent
  | RuntimeProviderRecentBlockhashEvent;

export interface PacketSubscription {
  readonly sourceKind?: RuntimePacketSourceKind;
  readonly transport?: RuntimePacketTransport;
  readonly eventClass?: RuntimePacketEventClass;
  readonly localAddress?: SocketAddress;
  readonly localPort?: number;
  readonly remoteAddress?: SocketAddress;
  readonly remotePort?: number;
  readonly ownerExtension?: ExtensionName;
  readonly resourceId?: ExtensionResourceId;
  readonly sharedTag?: SharedStreamTag;
  readonly webSocketFrameType?: RuntimeWebSocketFrameType;
}

export interface ExtensionManifest {
  readonly capabilities: readonly ExtensionCapability[];
  readonly resources: readonly ExtensionResourceSpec[];
  readonly subscriptions: readonly PacketSubscription[];
}

export interface ExtensionContext {
  readonly extensionName: ExtensionName;
}

export enum SdkLanguage {
  TypeScript = 1,
}

export enum ForeignWorkerKind {
  RuntimeExtension = 1,
}

export interface RuntimeExtensionProtocolVersion {
  readonly major: number;
  readonly minor: number;
}

export const defaultRuntimeExtensionProtocolVersion: RuntimeExtensionProtocolVersion = {
  major: 1,
  minor: 0,
};

export interface RuntimeExtensionWorkerManifest {
  readonly protocolVersion: RuntimeExtensionProtocolVersion;
  readonly sdkLanguage: SdkLanguage.TypeScript;
  readonly sdkVersion: string;
  readonly manifestVersion: number;
  readonly workerKind: ForeignWorkerKind.RuntimeExtension;
  readonly extensionName: ExtensionName;
  readonly manifest: ExtensionManifest;
}

export type MaybePromise<T> = T | Promise<T>;

export interface RuntimeExtensionDefinition {
  readonly manifest: RuntimeExtensionWorkerManifest;
  readonly onReady?: (
    context: ExtensionContext,
  ) => MaybePromise<Result<RuntimeExtensionAck, RuntimeExtensionError>>;
  readonly onPacketReceived?: (
    event: RuntimePacketEvent,
  ) => MaybePromise<Result<RuntimeExtensionAck, RuntimeExtensionError>>;
  readonly onProviderEvent?: (
    event: RuntimeProviderEvent,
  ) => MaybePromise<Result<RuntimeExtensionAck, RuntimeExtensionError>>;
  readonly onShutdown?: (
    context: ExtensionContext,
  ) => MaybePromise<Result<RuntimeExtensionAck, RuntimeExtensionError>>;
}

export enum RuntimeExtensionWorkerHostMessageTag {
  GetManifest = 1,
  Start = 2,
  DeliverPacket = 3,
  Shutdown = 4,
  DeliverProviderEvent = 5,
}

export type RuntimeExtensionWorkerHostMessage =
  | {
      readonly tag: RuntimeExtensionWorkerHostMessageTag.GetManifest;
    }
  | {
      readonly tag: RuntimeExtensionWorkerHostMessageTag.Start;
      readonly context: ExtensionContext;
    }
  | {
      readonly tag: RuntimeExtensionWorkerHostMessageTag.DeliverPacket;
      readonly event: RuntimePacketEvent;
    }
  | {
      readonly tag: RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent;
      readonly event: RuntimeProviderEvent;
    }
  | {
      readonly tag: RuntimeExtensionWorkerHostMessageTag.Shutdown;
      readonly context: ExtensionContext;
    };

export enum RuntimeExtensionWorkerResponseTag {
  Manifest = 1,
  Started = 2,
  EventHandled = 3,
  ShutdownComplete = 4,
  ProviderEventHandled = 5,
}

export type RuntimeExtensionWorkerResponse =
  | {
      readonly tag: RuntimeExtensionWorkerResponseTag.Manifest;
      readonly result: Result<RuntimeExtensionWorkerManifest, RuntimeExtensionError>;
    }
  | {
      readonly tag: RuntimeExtensionWorkerResponseTag.Started;
      readonly result: Result<RuntimeExtensionAck, RuntimeExtensionError>;
    }
  | {
      readonly tag: RuntimeExtensionWorkerResponseTag.EventHandled;
      readonly result: Result<RuntimeExtensionAck, RuntimeExtensionError>;
    }
  | {
      readonly tag: RuntimeExtensionWorkerResponseTag.ProviderEventHandled;
      readonly result: Result<RuntimeExtensionAck, RuntimeExtensionError>;
    }
  | {
      readonly tag: RuntimeExtensionWorkerResponseTag.ShutdownComplete;
      readonly result: Result<RuntimeExtensionAck, RuntimeExtensionError>;
    };

export interface RuntimeExtensionWorkerManifestInit {
  readonly protocolVersion?: RuntimeExtensionProtocolVersion;
  readonly sdkVersion: string;
  readonly manifestVersion?: number;
  readonly extensionName: string | ExtensionName;
  readonly capabilities?: readonly ExtensionCapability[];
  readonly resources?: readonly ExtensionResourceSpec[];
  readonly subscriptions?: readonly PacketSubscription[];
}

const defaultResourceReadBufferBytes = 2_048;
const maxResourceReadBufferBytes = 1024 * 1024;

function runtimeExtensionError(
  kind: RuntimeExtensionErrorKind,
  field: string,
  message: string,
  received?: string,
  cause?: string,
): RuntimeExtensionError {
  const runtimeError: RuntimeExtensionError = {
    kind,
    field,
    message,
  };

  if (received !== undefined) {
    return {
      ...runtimeError,
      received,
      ...(cause === undefined ? {} : { cause }),
    };
  }

  if (cause !== undefined) {
    return {
      ...runtimeError,
      cause,
    };
  }

  return runtimeError;
}

function asExtensionName<const Value extends string>(value: Value): ExtensionName {
  return brand<Value, "ExtensionName">(value);
}

function asExtensionResourceId<const Value extends string>(value: Value): ExtensionResourceId {
  return brand<Value, "ExtensionResourceId">(value);
}

function asSharedStreamTag<const Value extends string>(value: Value): SharedStreamTag {
  return brand<Value, "SharedStreamTag">(value);
}

function asSocketAddress<const Value extends string>(value: Value): SocketAddress {
  return brand<Value, "SocketAddress">(value);
}

function asWebSocketUrl<const Value extends string>(value: Value): WebSocketUrl {
  return brand<Value, "WebSocketUrl">(value);
}

function parseNonEmptyValueObject<T>(
  value: string,
  field: string,
  wrap: (normalized: string) => T,
): Result<T, RuntimeExtensionError> {
  const normalized = value.trim();
  if (normalized === "") {
    return err(
      runtimeExtensionError(
        RuntimeExtensionErrorKind.ValidationError,
        field,
        `${field} must not be empty`,
        value,
      ),
    );
  }
  if (normalized.includes("\u0000")) {
    return err(
      runtimeExtensionError(
        RuntimeExtensionErrorKind.ValidationError,
        field,
        `${field} must not contain NUL bytes`,
        value,
      ),
    );
  }

  return ok(wrap(normalized));
}

export function extensionName(value: string): Result<ExtensionName, RuntimeExtensionError> {
  return parseNonEmptyValueObject(value, "extensionName", asExtensionName);
}

export function extensionResourceId(
  value: string,
): Result<ExtensionResourceId, RuntimeExtensionError> {
  return parseNonEmptyValueObject(value, "resourceId", asExtensionResourceId);
}

export function sharedStreamTag(value: string): Result<SharedStreamTag, RuntimeExtensionError> {
  return parseNonEmptyValueObject(value, "sharedTag", asSharedStreamTag);
}

export function socketAddress(value: string): Result<SocketAddress, RuntimeExtensionError> {
  return parseNonEmptyValueObject(value, "socketAddress", asSocketAddress);
}

export function webSocketUrl(value: string): Result<WebSocketUrl, RuntimeExtensionError> {
  const parsed = parseNonEmptyValueObject(value, "url", asWebSocketUrl);
  if (isErr(parsed)) {
    return parsed;
  }
  if (!parsed.value.startsWith("ws://") && !parsed.value.startsWith("wss://")) {
    return err(
      runtimeExtensionError(
        RuntimeExtensionErrorKind.ValidationError,
        "url",
        "url must start with ws:// or wss://",
        value,
      ),
    );
  }

  return parsed;
}

export function privateExtensionStream(): ExtensionStreamVisibility {
  return {
    tag: ExtensionStreamVisibilityTag.Private,
  };
}

export function sharedExtensionStream(
  tag: SharedStreamTag | string,
): Result<ExtensionStreamVisibility, RuntimeExtensionError> {
  const parsed = typeof tag === "string" ? sharedStreamTag(tag) : ok(tag);
  if (isErr(parsed)) {
    return parsed;
  }

  return ok({
    tag: ExtensionStreamVisibilityTag.Shared,
    sharedTag: parsed.value,
  });
}

function validatePositiveInteger(
  value: number,
  field: string,
): Result<number, RuntimeExtensionError> {
  if (!Number.isInteger(value) || value <= 0) {
    return err(
      runtimeExtensionError(
        RuntimeExtensionErrorKind.ValidationError,
        field,
        `${field} must be a positive integer`,
        String(value),
      ),
    );
  }

  return ok(value);
}

function validateResourceReadBufferBytes(value: number): Result<number, RuntimeExtensionError> {
  const parsed = validatePositiveInteger(value, "readBufferBytes");
  if (isErr(parsed)) {
    return parsed;
  }
  if (parsed.value > maxResourceReadBufferBytes) {
    return err(
      runtimeExtensionError(
        RuntimeExtensionErrorKind.ValidationError,
        "readBufferBytes",
        `readBufferBytes must be <= ${maxResourceReadBufferBytes}`,
        String(value),
      ),
    );
  }

  return parsed;
}

function validateRuntimeExtensionProtocolVersion(
  value: RuntimeExtensionProtocolVersion,
): Result<RuntimeExtensionProtocolVersion, RuntimeExtensionError> {
  const major = validatePositiveInteger(value.major, "protocolVersion.major");
  if (isErr(major)) {
    return major;
  }
  if (!Number.isInteger(value.minor) || value.minor < 0) {
    return err(
      runtimeExtensionError(
        RuntimeExtensionErrorKind.ValidationError,
        "protocolVersion.minor",
        "protocolVersion.minor must be a non-negative integer",
        String(value.minor),
      ),
    );
  }

  return ok({
    major: major.value,
    minor: value.minor,
  });
}

function validateRuntimeExtensionWorkerManifest(
  manifest: RuntimeExtensionWorkerManifest,
): Result<RuntimeExtensionWorkerManifest, RuntimeExtensionError> {
  const protocolVersion = validateRuntimeExtensionProtocolVersion(manifest.protocolVersion);
  if (isErr(protocolVersion)) {
    return protocolVersion;
  }

  const version = manifest.sdkVersion.trim();
  if (version === "") {
    return err(
      runtimeExtensionError(
        RuntimeExtensionErrorKind.ValidationError,
        "sdkVersion",
        "sdkVersion must not be empty",
        manifest.sdkVersion,
      ),
    );
  }

  const manifestVersion = validatePositiveInteger(manifest.manifestVersion, "manifestVersion");
  if (isErr(manifestVersion)) {
    return manifestVersion;
  }

  const duplicateResourceIds = new Set<string>();
  for (const resource of manifest.manifest.resources) {
    const validatedReadBuffer = validateResourceReadBufferBytes(resource.readBufferBytes);
    if (isErr(validatedReadBuffer)) {
      return validatedReadBuffer;
    }

    const resourceId = resource.resourceId as string;
    if (duplicateResourceIds.has(resourceId)) {
      return err(
        runtimeExtensionError(
          RuntimeExtensionErrorKind.ValidationError,
          "resourceId",
          `duplicate resourceId ${resourceId}`,
          resourceId,
        ),
      );
    }
    duplicateResourceIds.add(resourceId);

    if (resource.visibility.tag === ExtensionStreamVisibilityTag.Shared) {
      const sharedTagValue = sharedStreamTag(resource.visibility.sharedTag);
      if (isErr(sharedTagValue)) {
        return sharedTagValue;
      }
    }
  }

  return ok(manifest);
}

export function createRuntimeExtensionWorkerManifest(
  init: RuntimeExtensionWorkerManifestInit,
): RuntimeExtensionWorkerManifest {
  const result = tryCreateRuntimeExtensionWorkerManifest(init);
  if (isErr(result)) {
    throw new RangeError(result.error.message);
  }

  return result.value;
}

export function tryCreateRuntimeExtensionWorkerManifest(
  init: RuntimeExtensionWorkerManifestInit,
): Result<RuntimeExtensionWorkerManifest, RuntimeExtensionError> {
  const parsedName =
    typeof init.extensionName === "string"
      ? extensionName(init.extensionName)
      : ok(init.extensionName);
  if (isErr(parsedName)) {
    return parsedName;
  }

  const manifest: RuntimeExtensionWorkerManifest = {
    protocolVersion: init.protocolVersion ?? defaultRuntimeExtensionProtocolVersion,
    sdkLanguage: SdkLanguage.TypeScript,
    sdkVersion: init.sdkVersion,
    manifestVersion: init.manifestVersion ?? 1,
    workerKind: ForeignWorkerKind.RuntimeExtension,
    extensionName: parsedName.value,
    manifest: {
      capabilities: init.capabilities ?? [],
      resources: init.resources ?? [],
      subscriptions: init.subscriptions ?? [],
    },
  };

  return validateRuntimeExtensionWorkerManifest(manifest);
}

export function packetSubscriptionMatches(
  subscription: PacketSubscription,
  event: RuntimePacketEvent,
): boolean {
  if (subscription.sourceKind !== undefined && subscription.sourceKind !== event.source.kind) {
    return false;
  }
  if (subscription.transport !== undefined && subscription.transport !== event.source.transport) {
    return false;
  }
  if (
    subscription.eventClass !== undefined &&
    subscription.eventClass !== event.source.eventClass
  ) {
    return false;
  }
  if (
    subscription.localAddress !== undefined &&
    subscription.localAddress !== event.source.localAddress
  ) {
    return false;
  }
  if (
    subscription.localPort !== undefined &&
    event.source.localAddress?.split(":").at(-1) !== String(subscription.localPort)
  ) {
    return false;
  }
  if (
    subscription.remoteAddress !== undefined &&
    subscription.remoteAddress !== event.source.remoteAddress
  ) {
    return false;
  }
  if (
    subscription.remotePort !== undefined &&
    event.source.remoteAddress?.split(":").at(-1) !== String(subscription.remotePort)
  ) {
    return false;
  }
  if (
    subscription.ownerExtension !== undefined &&
    subscription.ownerExtension !== event.source.ownerExtension
  ) {
    return false;
  }
  if (
    subscription.resourceId !== undefined &&
    subscription.resourceId !== event.source.resourceId
  ) {
    return false;
  }
  if (subscription.sharedTag !== undefined && subscription.sharedTag !== event.source.sharedTag) {
    return false;
  }
  if (
    subscription.webSocketFrameType !== undefined &&
    subscription.webSocketFrameType !== event.source.webSocketFrameType
  ) {
    return false;
  }

  return true;
}

export function runtimeExtensionAck(): RuntimeExtensionAck {
  return {
    acknowledged: true,
  };
}

async function settleExtensionHook(
  field: string,
  callback: () => MaybePromise<Result<RuntimeExtensionAck, RuntimeExtensionError>>,
): Promise<Result<RuntimeExtensionAck, RuntimeExtensionError>> {
  try {
    return await callback();
  } catch (error) {
    const cause = error instanceof Error ? error.message : String(error);
    return err(
      runtimeExtensionError(
        RuntimeExtensionErrorKind.UnhandledException,
        field,
        `${field} threw an unhandled exception`,
        undefined,
        cause,
      ),
    );
  }
}

export function defineRuntimeExtension(
  definition: RuntimeExtensionDefinition,
): RuntimeExtensionDefinition {
  const result = tryDefineRuntimeExtension(definition);
  if (isErr(result)) {
    throw new RangeError(result.error.message);
  }

  return result.value;
}

export function tryDefineRuntimeExtension(
  definition: RuntimeExtensionDefinition,
): Result<RuntimeExtensionDefinition, RuntimeExtensionError> {
  const manifest = validateRuntimeExtensionWorkerManifest(definition.manifest);
  if (isErr(manifest)) {
    return manifest;
  }

  return ok({
    ...definition,
    manifest: manifest.value,
  });
}

export class RuntimeExtensionWorkerRuntime {
  readonly definition: RuntimeExtensionDefinition;

  constructor(definition: RuntimeExtensionDefinition) {
    this.definition = defineRuntimeExtension(definition);
  }

  async handleMessage(
    message: RuntimeExtensionWorkerHostMessage,
  ): Promise<RuntimeExtensionWorkerResponse> {
    switch (message.tag) {
      case RuntimeExtensionWorkerHostMessageTag.GetManifest:
        return {
          tag: RuntimeExtensionWorkerResponseTag.Manifest,
          result: ok(this.definition.manifest),
        };
      case RuntimeExtensionWorkerHostMessageTag.Start:
        return {
          tag: RuntimeExtensionWorkerResponseTag.Started,
          result:
            this.definition.onReady === undefined
              ? ok(runtimeExtensionAck())
              : await settleExtensionHook(
                  "onReady",
                  () => this.definition.onReady?.(message.context) ?? ok(runtimeExtensionAck()),
                ),
        };
      case RuntimeExtensionWorkerHostMessageTag.DeliverPacket:
        return {
          tag: RuntimeExtensionWorkerResponseTag.EventHandled,
          result:
            this.definition.onPacketReceived === undefined
              ? ok(runtimeExtensionAck())
              : await settleExtensionHook(
                  "onPacketReceived",
                  () =>
                    this.definition.onPacketReceived?.(message.event) ?? ok(runtimeExtensionAck()),
                ),
        };
      case RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent:
        return {
          tag: RuntimeExtensionWorkerResponseTag.ProviderEventHandled,
          result:
            this.definition.onProviderEvent === undefined
              ? ok(runtimeExtensionAck())
              : await settleExtensionHook(
                  "onProviderEvent",
                  () =>
                    this.definition.onProviderEvent?.(message.event) ?? ok(runtimeExtensionAck()),
                ),
        };
      case RuntimeExtensionWorkerHostMessageTag.Shutdown:
        return {
          tag: RuntimeExtensionWorkerResponseTag.ShutdownComplete,
          result:
            this.definition.onShutdown === undefined
              ? ok(runtimeExtensionAck())
              : await settleExtensionHook(
                  "onShutdown",
                  () => this.definition.onShutdown?.(message.context) ?? ok(runtimeExtensionAck()),
                ),
        };
    }

    return {
      tag: RuntimeExtensionWorkerResponseTag.ShutdownComplete,
      result: err(
        runtimeExtensionError(
          RuntimeExtensionErrorKind.ProtocolError,
          "message.tag",
          "unknown runtime extension worker message tag",
        ),
      ),
    };
  }
}

export function createRuntimeExtensionWorkerRuntime(
  definition: RuntimeExtensionDefinition,
): RuntimeExtensionWorkerRuntime {
  return new RuntimeExtensionWorkerRuntime(definition);
}

export function tryCreateRuntimeExtensionWorkerRuntime(
  definition: RuntimeExtensionDefinition,
): Result<RuntimeExtensionWorkerRuntime, RuntimeExtensionError> {
  const validated = tryDefineRuntimeExtension(definition);
  if (isErr(validated)) {
    return validated;
  }

  return ok(new RuntimeExtensionWorkerRuntime(validated.value));
}

export function createRuntimePacketEvent(
  source: RuntimePacketSource,
  bytes: Uint8Array | readonly number[],
  observedUnixMs: number,
): RuntimePacketEvent {
  return {
    source,
    bytes: bytes instanceof Uint8Array ? bytes : Uint8Array.from(bytes),
    observedUnixMs,
  };
}

export function udpListenerResource(
  resourceId: ExtensionResourceId,
  bindAddress: SocketAddress,
  visibility: ExtensionStreamVisibility = privateExtensionStream(),
  readBufferBytes = defaultResourceReadBufferBytes,
): Result<UdpListenerResourceSpec, RuntimeExtensionError> {
  const validatedReadBuffer = validateResourceReadBufferBytes(readBufferBytes);
  if (isErr(validatedReadBuffer)) {
    return validatedReadBuffer;
  }

  return ok({
    kind: ExtensionResourceKind.UdpListener,
    resourceId,
    bindAddress,
    visibility,
    readBufferBytes: validatedReadBuffer.value,
  });
}

export function webSocketConnectorResource(
  resourceId: ExtensionResourceId,
  url: WebSocketUrl,
  visibility: ExtensionStreamVisibility = privateExtensionStream(),
  readBufferBytes = defaultResourceReadBufferBytes,
): Result<WebSocketConnectorResourceSpec, RuntimeExtensionError> {
  const validatedReadBuffer = validateResourceReadBufferBytes(readBufferBytes);
  if (isErr(validatedReadBuffer)) {
    return validatedReadBuffer;
  }

  return ok({
    kind: ExtensionResourceKind.WsConnector,
    resourceId,
    url,
    visibility,
    readBufferBytes: validatedReadBuffer.value,
  });
}
