import { Buffer } from "node:buffer";
import { once } from "node:events";

import { ResultTag, err, isErr, ok, type Result } from "../result.js";
import {
  RuntimeExtensionErrorKind,
  RuntimeExtensionWorkerHostMessageTag,
  type ExtensionContext,
  type ExtensionName,
  type ExtensionResourceId,
  type SharedStreamTag,
  type SocketAddress,
  type RuntimeExtensionAck,
  type RuntimeExtensionDefinition,
  type RuntimeExtensionError,
  type RuntimeExtensionWorkerHostMessage,
  type RuntimeExtensionWorkerResponse,
  type RuntimePacketEvent,
  type RuntimePacketEventClass,
  type RuntimePacketSource,
  type RuntimePacketSourceKind,
  type RuntimePacketTransport,
  type RuntimeProviderEvent,
  type RuntimeProviderEventKind,
  type RuntimeWebSocketFrameType,
  extensionName,
  extensionResourceId,
  runtimeExtensionAck,
  socketAddress,
  sharedStreamTag,
  tryCreateRuntimeExtensionWorkerRuntime,
} from "./runtime-extension.js";

export * from "./runtime-extension.js";

const runtimePacketSourceKinds = [1, 2] as const satisfies readonly RuntimePacketSourceKind[];
const runtimePacketTransports = [1, 2, 3] as const satisfies readonly RuntimePacketTransport[];
const runtimePacketEventClasses = [1, 2] as const satisfies readonly RuntimePacketEventClass[];
const runtimeWebSocketFrameTypes = [
  1, 2, 3, 4,
] as const satisfies readonly RuntimeWebSocketFrameType[];
const runtimeProviderEventKinds = [
  1, 2, 3, 4, 5, 6, 7,
] as const satisfies readonly RuntimeProviderEventKind[];
const workerFrameHeaderBytes = 5;

type JsonRecord = Record<string, unknown>;

export interface RuntimePacketEventWire {
  readonly source: RuntimePacketSource;
  readonly bytesBase64: string;
  readonly observedUnixMs: number;
}

export interface RuntimePacketEventBatchWire {
  readonly events: readonly RuntimePacketEventWire[];
}

export interface RuntimeProviderEventBatchWire {
  readonly events: readonly RuntimeProviderEvent[];
}

export type RuntimeExtensionWorkerWireHostMessage =
  | {
      readonly tag: RuntimeExtensionWorkerHostMessageTag.GetManifest;
    }
  | {
      readonly tag: RuntimeExtensionWorkerHostMessageTag.Start;
      readonly context: ExtensionContext;
    }
  | {
      readonly tag: RuntimeExtensionWorkerHostMessageTag.DeliverPacket;
      readonly event: RuntimePacketEventWire;
    }
  | {
      readonly tag: RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent;
      readonly event: RuntimeProviderEvent;
    }
  | {
      readonly tag: RuntimeExtensionWorkerHostMessageTag.Shutdown;
      readonly context: ExtensionContext;
    };

export type RuntimeExtensionWorkerWireResponse = RuntimeExtensionWorkerResponse;

export interface RuntimeExtensionWorkerStdioOptions {
  readonly input?: NodeJS.ReadableStream;
  readonly output?: NodeJS.WritableStream;
  readonly error?: NodeJS.WritableStream;
  readonly guardProcessStdout?: boolean;
}

interface WorkerStdoutGuard {
  readonly writeProtocolBytes: (payload: Uint8Array) => Promise<void>;
  readonly restore: () => void;
}

function stdoutReservedError(): Error {
  return new Error(
    "SOF SDK worker stdout is reserved for protocol messages; use stderr, console.error, or console.warn instead",
  );
}

function runtimeExtensionProtocolError(
  field: string,
  message: string,
  received?: string,
  cause?: string,
): RuntimeExtensionError {
  const error: RuntimeExtensionError = {
    kind: RuntimeExtensionErrorKind.ProtocolError,
    field,
    message,
  };

  if (received !== undefined) {
    return {
      ...error,
      received,
      ...(cause === undefined ? {} : { cause }),
    };
  }

  if (cause !== undefined) {
    return {
      ...error,
      cause,
    };
  }

  return error;
}

function isJsonRecord(value: unknown): value is JsonRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseJsonRecord(value: unknown, field: string): Result<JsonRecord, RuntimeExtensionError> {
  if (!isJsonRecord(value)) {
    return err(
      runtimeExtensionProtocolError(field, `${field} must be a JSON object`, JSON.stringify(value)),
    );
  }

  return ok(value);
}

function okMissing<Value>(): Result<Value | undefined, RuntimeExtensionError> {
  return {
    tag: ResultTag.Ok,
    value: void 0,
  };
}

function parseOptionalString(
  value: unknown,
  field: string,
): Result<string | undefined, RuntimeExtensionError> {
  if (value === undefined) {
    return okMissing();
  }
  if (typeof value !== "string") {
    return err(
      runtimeExtensionProtocolError(field, `${field} must be a string`, JSON.stringify(value)),
    );
  }

  return ok(value);
}

function parseNumericEnumValue(
  value: unknown,
  field: string,
  allowed: readonly number[],
): Result<number, RuntimeExtensionError> {
  if (typeof value !== "number" || !Number.isInteger(value) || !allowed.includes(value)) {
    return err(
      runtimeExtensionProtocolError(
        field,
        `${field} must be one of ${allowed.join(", ")}`,
        JSON.stringify(value),
      ),
    );
  }

  return ok(value);
}

function parseRuntimeExtensionWorkerHostMessageTag(
  value: unknown,
): Result<RuntimeExtensionWorkerHostMessageTag, RuntimeExtensionError> {
  const parsed = parseNumericEnumValue(value, "message.tag", [
    RuntimeExtensionWorkerHostMessageTag.GetManifest,
    RuntimeExtensionWorkerHostMessageTag.Start,
    RuntimeExtensionWorkerHostMessageTag.DeliverPacket,
    RuntimeExtensionWorkerHostMessageTag.Shutdown,
    RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent,
  ]);
  if (isErr(parsed)) {
    return parsed;
  }

  if (parsed.value === 1) {
    return ok(RuntimeExtensionWorkerHostMessageTag.GetManifest);
  }
  if (parsed.value === 2) {
    return ok(RuntimeExtensionWorkerHostMessageTag.Start);
  }
  if (parsed.value === 3) {
    return ok(RuntimeExtensionWorkerHostMessageTag.DeliverPacket);
  }
  if (parsed.value === 4) {
    return ok(RuntimeExtensionWorkerHostMessageTag.Shutdown);
  }
  if (parsed.value === 5) {
    return ok(RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent);
  }

  return err(
    runtimeExtensionProtocolError(
      "message.tag",
      "message.tag must be a supported runtime extension worker message tag",
      JSON.stringify(parsed.value),
    ),
  );
}

function parseContext(value: unknown): Result<ExtensionContext, RuntimeExtensionError> {
  const contextRecord = parseJsonRecord(value, "context");
  if (isErr(contextRecord)) {
    return contextRecord;
  }

  const parsedName = parseOptionalString(
    contextRecord.value.extensionName,
    "context.extensionName",
  );
  if (isErr(parsedName) || parsedName.value === undefined) {
    return isErr(parsedName)
      ? parsedName
      : err(
          runtimeExtensionProtocolError(
            "context.extensionName",
            "context.extensionName must be provided",
          ),
        );
  }

  const extension = extensionName(parsedName.value);
  if (isErr(extension)) {
    return err({
      ...extension.error,
      field: "context.extensionName",
    });
  }

  return ok({
    extensionName: extension.value,
  });
}

function parseOptionalExtensionName(
  value: unknown,
  field: string,
): Result<ExtensionName | undefined, RuntimeExtensionError> {
  const parsed = parseOptionalString(value, field);
  if (isErr(parsed)) {
    return parsed;
  }
  if (parsed.value === undefined) {
    return okMissing();
  }

  const extension = extensionName(parsed.value);
  if (isErr(extension)) {
    return err({
      ...extension.error,
      field,
    });
  }

  return ok(extension.value);
}

function parseOptionalResourceId(
  value: unknown,
  field: string,
): Result<ExtensionResourceId | undefined, RuntimeExtensionError> {
  const parsed = parseOptionalString(value, field);
  if (isErr(parsed)) {
    return parsed;
  }
  if (parsed.value === undefined) {
    return okMissing();
  }

  const resourceId = extensionResourceId(parsed.value);
  if (isErr(resourceId)) {
    return err({
      ...resourceId.error,
      field,
    });
  }

  return ok(resourceId.value);
}

function parseOptionalSocketAddress(
  value: unknown,
  field: string,
): Result<SocketAddress | undefined, RuntimeExtensionError> {
  const parsed = parseOptionalString(value, field);
  if (isErr(parsed)) {
    return parsed;
  }
  if (parsed.value === undefined) {
    return okMissing();
  }

  const address = socketAddress(parsed.value);
  if (isErr(address)) {
    return err({
      ...address.error,
      field,
    });
  }

  return ok(address.value);
}

function parseOptionalSharedStreamTag(
  value: unknown,
  field: string,
): Result<SharedStreamTag | undefined, RuntimeExtensionError> {
  const parsed = parseOptionalString(value, field);
  if (isErr(parsed)) {
    return parsed;
  }
  if (parsed.value === undefined) {
    return okMissing();
  }

  const tag = sharedStreamTag(parsed.value);
  if (isErr(tag)) {
    return err({
      ...tag.error,
      field,
    });
  }

  return ok(tag.value);
}

function parsePacketBytesBase64(value: unknown): Result<Uint8Array, RuntimeExtensionError> {
  if (typeof value !== "string") {
    return err(
      runtimeExtensionProtocolError(
        "event.bytesBase64",
        "event.bytesBase64 must be a base64 string",
        JSON.stringify(value),
      ),
    );
  }

  try {
    return ok(Uint8Array.from(Buffer.from(value, "base64")));
  } catch (error) {
    return err(
      runtimeExtensionProtocolError(
        "event.bytesBase64",
        "event.bytesBase64 must be valid base64",
        JSON.stringify(value),
        error instanceof Error ? error.message : String(error),
      ),
    );
  }
}

function parseRuntimePacketSource(
  value: unknown,
): Result<RuntimePacketSource, RuntimeExtensionError> {
  const sourceRecord = parseJsonRecord(value, "event.source");
  if (isErr(sourceRecord)) {
    return sourceRecord;
  }

  const kind = parseNumericEnumValue(
    sourceRecord.value.kind,
    "event.source.kind",
    runtimePacketSourceKinds,
  );
  if (isErr(kind)) {
    return kind;
  }

  const transport = parseNumericEnumValue(
    sourceRecord.value.transport,
    "event.source.transport",
    runtimePacketTransports,
  );
  if (isErr(transport)) {
    return transport;
  }

  const eventClass = parseNumericEnumValue(
    sourceRecord.value.eventClass,
    "event.source.eventClass",
    runtimePacketEventClasses,
  );
  if (isErr(eventClass)) {
    return eventClass;
  }

  const ownerExtension = parseOptionalExtensionName(
    sourceRecord.value.ownerExtension,
    "event.source.ownerExtension",
  );
  if (isErr(ownerExtension)) {
    return ownerExtension;
  }

  const resourceId = parseOptionalResourceId(
    sourceRecord.value.resourceId,
    "event.source.resourceId",
  );
  if (isErr(resourceId)) {
    return resourceId;
  }

  const sharedTag = parseOptionalSharedStreamTag(
    sourceRecord.value.sharedTag,
    "event.source.sharedTag",
  );
  if (isErr(sharedTag)) {
    return sharedTag;
  }

  const webSocketFrameType =
    sourceRecord.value.webSocketFrameType === undefined
      ? okMissing<number>()
      : parseNumericEnumValue(
          sourceRecord.value.webSocketFrameType,
          "event.source.webSocketFrameType",
          runtimeWebSocketFrameTypes,
        );
  if (isErr(webSocketFrameType)) {
    return webSocketFrameType;
  }

  const localAddress = parseOptionalSocketAddress(
    sourceRecord.value.localAddress,
    "event.source.localAddress",
  );
  if (isErr(localAddress)) {
    return localAddress;
  }

  const remoteAddress = parseOptionalSocketAddress(
    sourceRecord.value.remoteAddress,
    "event.source.remoteAddress",
  );
  if (isErr(remoteAddress)) {
    return remoteAddress;
  }

  return ok({
    kind: kind.value,
    transport: transport.value,
    eventClass: eventClass.value,
    ...(ownerExtension.value === undefined ? {} : { ownerExtension: ownerExtension.value }),
    ...(resourceId.value === undefined ? {} : { resourceId: resourceId.value }),
    ...(sharedTag.value === undefined ? {} : { sharedTag: sharedTag.value }),
    ...(webSocketFrameType.value === undefined
      ? {}
      : { webSocketFrameType: webSocketFrameType.value }),
    ...(localAddress.value === undefined ? {} : { localAddress: localAddress.value }),
    ...(remoteAddress.value === undefined ? {} : { remoteAddress: remoteAddress.value }),
  });
}

function isRuntimeProviderEventKind(value: unknown): value is RuntimeProviderEventKind {
  return (
    typeof value === "number" &&
    Number.isInteger(value) &&
    runtimeProviderEventKinds.some((kind) => kind === value)
  );
}

function isRuntimeProviderEvent(value: unknown): value is RuntimeProviderEvent {
  return isJsonRecord(value) && isRuntimeProviderEventKind(value.kind);
}

export function serializeRuntimePacketEventWire(event: RuntimePacketEvent): RuntimePacketEventWire {
  return {
    source: event.source,
    bytesBase64: Buffer.from(event.bytes).toString("base64"),
    observedUnixMs: event.observedUnixMs,
  };
}

export function tryParseRuntimePacketEventWire(
  value: unknown,
): Result<RuntimePacketEvent, RuntimeExtensionError> {
  const eventRecord = parseJsonRecord(value, "event");
  if (isErr(eventRecord)) {
    return eventRecord;
  }

  const source = parseRuntimePacketSource(eventRecord.value.source);
  if (isErr(source)) {
    return source;
  }

  const bytes = parsePacketBytesBase64(eventRecord.value.bytesBase64);
  if (isErr(bytes)) {
    return bytes;
  }

  if (
    typeof eventRecord.value.observedUnixMs !== "number" ||
    !Number.isSafeInteger(eventRecord.value.observedUnixMs) ||
    eventRecord.value.observedUnixMs < 0
  ) {
    return err(
      runtimeExtensionProtocolError(
        "event.observedUnixMs",
        "event.observedUnixMs must be a non-negative safe integer",
        JSON.stringify(eventRecord.value.observedUnixMs),
      ),
    );
  }

  return ok({
    source: source.value,
    bytes: bytes.value,
    observedUnixMs: eventRecord.value.observedUnixMs,
  });
}

export function tryParseRuntimeProviderEventWire(
  value: unknown,
): Result<RuntimeProviderEvent, RuntimeExtensionError> {
  const eventRecord = parseJsonRecord(value, "event");
  if (isErr(eventRecord)) {
    return eventRecord;
  }

  if (!isRuntimeProviderEvent(eventRecord.value)) {
    return err(
      runtimeExtensionProtocolError(
        "event.kind",
        `event.kind must be one of ${runtimeProviderEventKinds.join(", ")}`,
        JSON.stringify(eventRecord.value.kind),
      ),
    );
  }

  return ok(eventRecord.value);
}

function parseRuntimePacketEventBatchWire(
  value: unknown,
): Result<readonly RuntimePacketEvent[], RuntimeExtensionError> {
  const batchRecord = parseJsonRecord(value, "message");
  if (isErr(batchRecord)) {
    return batchRecord;
  }
  if (!Array.isArray(batchRecord.value.events)) {
    return err(
      runtimeExtensionProtocolError(
        "message.events",
        "message.events must be an array",
        JSON.stringify(batchRecord.value.events),
      ),
    );
  }

  const events: RuntimePacketEvent[] = [];
  for (const eventValue of batchRecord.value.events) {
    const event = tryParseRuntimePacketEventWire(eventValue);
    if (isErr(event)) {
      return event;
    }
    events.push(event.value);
  }

  return ok(events);
}

function parseRuntimeProviderEventBatchWire(
  value: unknown,
): Result<readonly RuntimeProviderEvent[], RuntimeExtensionError> {
  const batchRecord = parseJsonRecord(value, "message");
  if (isErr(batchRecord)) {
    return batchRecord;
  }
  if (!Array.isArray(batchRecord.value.events)) {
    return err(
      runtimeExtensionProtocolError(
        "message.events",
        "message.events must be an array",
        JSON.stringify(batchRecord.value.events),
      ),
    );
  }

  const events: RuntimeProviderEvent[] = [];
  for (const eventValue of batchRecord.value.events) {
    const event = tryParseRuntimeProviderEventWire(eventValue);
    if (isErr(event)) {
      return event;
    }
    events.push(event.value);
  }

  return ok(events);
}

export function serializeRuntimeExtensionWorkerHostMessageWire(
  message: RuntimeExtensionWorkerHostMessage,
): RuntimeExtensionWorkerWireHostMessage {
  switch (message.tag) {
    case RuntimeExtensionWorkerHostMessageTag.GetManifest:
      return { tag: RuntimeExtensionWorkerHostMessageTag.GetManifest };
    case RuntimeExtensionWorkerHostMessageTag.Start:
      return {
        tag: RuntimeExtensionWorkerHostMessageTag.Start,
        context: message.context,
      };
    case RuntimeExtensionWorkerHostMessageTag.DeliverPacket:
      return {
        tag: RuntimeExtensionWorkerHostMessageTag.DeliverPacket,
        event: serializeRuntimePacketEventWire(message.event),
      };
    case RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent:
      return {
        tag: RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent,
        event: message.event,
      };
    case RuntimeExtensionWorkerHostMessageTag.Shutdown:
      return {
        tag: RuntimeExtensionWorkerHostMessageTag.Shutdown,
        context: message.context,
      };
  }

  return {
    tag: RuntimeExtensionWorkerHostMessageTag.GetManifest,
  };
}

export function tryParseRuntimeExtensionWorkerHostMessageWire(
  value: unknown,
): Result<RuntimeExtensionWorkerHostMessage, RuntimeExtensionError> {
  const messageRecord = parseJsonRecord(value, "message");
  if (isErr(messageRecord)) {
    return messageRecord;
  }

  const tag = parseRuntimeExtensionWorkerHostMessageTag(messageRecord.value.tag);
  if (isErr(tag)) {
    return tag;
  }

  switch (tag.value) {
    case RuntimeExtensionWorkerHostMessageTag.GetManifest:
      return ok({ tag: RuntimeExtensionWorkerHostMessageTag.GetManifest });
    case RuntimeExtensionWorkerHostMessageTag.Start: {
      const context = parseContext(messageRecord.value.context);
      if (isErr(context)) {
        return context;
      }

      return ok({
        tag: RuntimeExtensionWorkerHostMessageTag.Start,
        context: context.value,
      });
    }
    case RuntimeExtensionWorkerHostMessageTag.DeliverPacket: {
      const event = tryParseRuntimePacketEventWire(messageRecord.value.event);
      if (isErr(event)) {
        return event;
      }

      return ok({
        tag: RuntimeExtensionWorkerHostMessageTag.DeliverPacket,
        event: event.value,
      });
    }
    case RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent: {
      const event = tryParseRuntimeProviderEventWire(messageRecord.value.event);
      if (isErr(event)) {
        return event;
      }

      return ok({
        tag: RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent,
        event: event.value,
      });
    }
    case RuntimeExtensionWorkerHostMessageTag.Shutdown: {
      const context = parseContext(messageRecord.value.context);
      if (isErr(context)) {
        return context;
      }

      return ok({
        tag: RuntimeExtensionWorkerHostMessageTag.Shutdown,
        context: context.value,
      });
    }
  }

  return err(
    runtimeExtensionProtocolError(
      "message.tag",
      "message.tag must be a supported runtime extension worker message tag",
      JSON.stringify(tag.value),
    ),
  );
}

export function serializeRuntimeExtensionWorkerResponseWire(
  response: RuntimeExtensionWorkerResponse,
): RuntimeExtensionWorkerWireResponse {
  return response;
}

function encodeRuntimeExtensionWorkerFrame(tag: number, payload: Uint8Array): Uint8Array {
  const frame = new Uint8Array(workerFrameHeaderBytes + payload.length);
  const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength);
  frame[0] = tag;
  view.setUint32(1, payload.length, true);
  frame.set(payload, workerFrameHeaderBytes);
  return frame;
}

function encodeRuntimeExtensionWorkerJsonFrame(tag: number, payload: unknown): Uint8Array {
  const jsonBytes = new TextEncoder().encode(JSON.stringify(payload));
  return encodeRuntimeExtensionWorkerFrame(tag, jsonBytes);
}

async function writeBytes(output: NodeJS.WritableStream, bytes: Uint8Array): Promise<void> {
  if (output.write(bytes)) {
    return;
  }

  await once(output, "drain");
}

async function writeText(output: NodeJS.WritableStream, text: string): Promise<void> {
  await writeBytes(output, new TextEncoder().encode(text));
}

function bytesFromChunk(chunk: string | Uint8Array): Uint8Array {
  return typeof chunk === "string" ? new TextEncoder().encode(chunk) : Uint8Array.from(chunk);
}

class RuntimeExtensionFrameReader {
  readonly #iterator: AsyncIterator<string | Uint8Array>;
  #buffer = new Uint8Array(0);

  constructor(input: NodeJS.ReadableStream) {
    this.#iterator = input[Symbol.asyncIterator]() as AsyncIterator<string | Uint8Array>;
  }

  async nextFrame(): Promise<
    Result<
      { readonly tag: number; readonly payload: Uint8Array } | undefined,
      RuntimeExtensionError
    >
  > {
    const headerReady = await this.#fillHeader();
    if (isErr(headerReady)) {
      return headerReady;
    }
    if (!headerReady.value) {
      return okMissing();
    }

    const view = new DataView(
      this.#buffer.buffer,
      this.#buffer.byteOffset,
      this.#buffer.byteLength,
    );
    const tag = this.#buffer[0] ?? 0;
    const payloadLength = view.getUint32(1, true);
    const totalFrameLength = workerFrameHeaderBytes + payloadLength;

    const payloadReady = await this.#fillPayload(totalFrameLength);
    if (isErr(payloadReady)) {
      return payloadReady;
    }

    const payload = this.#buffer.slice(workerFrameHeaderBytes, totalFrameLength);
    this.#buffer = this.#buffer.slice(totalFrameLength);
    return ok({ tag, payload });
  }

  async #fillHeader(): Promise<Result<boolean, RuntimeExtensionError>> {
    if (this.#buffer.length >= workerFrameHeaderBytes) {
      return ok(true);
    }

    const next = await this.#iterator.next();
    if (next.done === true) {
      return this.#buffer.length === 0
        ? ok(false)
        : err(
            runtimeExtensionProtocolError(
              "message",
              "worker stdin closed while reading a framed message header",
            ),
          );
    }

    this.#buffer = new Uint8Array(appendChunk(this.#buffer, bytesFromChunk(next.value)));
    return this.#fillHeader();
  }

  async #fillPayload(totalFrameLength: number): Promise<Result<true, RuntimeExtensionError>> {
    if (this.#buffer.length >= totalFrameLength) {
      return ok(true);
    }

    const next = await this.#iterator.next();
    if (next.done === true) {
      return err(
        runtimeExtensionProtocolError(
          "message",
          "worker stdin closed while reading a framed message payload",
        ),
      );
    }

    this.#buffer = new Uint8Array(appendChunk(this.#buffer, bytesFromChunk(next.value)));
    return this.#fillPayload(totalFrameLength);
  }
}

function appendChunk(left: Uint8Array, right: Uint8Array): Uint8Array {
  const merged = new Uint8Array(left.length + right.length);
  merged.set(left, 0);
  merged.set(right, left.length);
  return merged;
}

function parseJsonPayload(payload: Uint8Array): Result<unknown, RuntimeExtensionError> {
  const jsonText = new TextDecoder().decode(payload);
  try {
    return ok(JSON.parse(jsonText));
  } catch (error) {
    return err(
      runtimeExtensionProtocolError(
        "message",
        "worker stdin frame payload must be valid JSON",
        jsonText,
        error instanceof Error ? error.message : String(error),
      ),
    );
  }
}

async function reportRuntimeHookError(
  errorOutput: NodeJS.WritableStream,
  result: Result<RuntimeExtensionAck, RuntimeExtensionError>,
): Promise<void> {
  if (!isErr(result)) {
    return;
  }

  const cause = result.error.cause === undefined ? "" : ` (${result.error.cause})`;
  await writeText(errorOutput, `${result.error.message}${cause}\n`);
}

function createWorkerStdoutGuard(
  output: NodeJS.WritableStream,
  enabled: boolean,
): WorkerStdoutGuard {
  if (!enabled) {
    return {
      writeProtocolBytes: (payload: Uint8Array) => writeBytes(output, payload),
      restore: () => {},
    };
  }

  const originalWrite = process.stdout.write.bind(process.stdout);
  const originalConsoleLog = globalThis.console.log;
  const originalConsoleInfo = globalThis.console.info;
  const originalConsoleDebug = globalThis.console.debug;
  const originalConsoleDir = globalThis.console.dir;
  let protocolWriteDepth = 0;
  const writeGuard: typeof process.stdout.write = (
    buffer: string | Uint8Array,
    encodingOrCallback?: BufferEncoding | ((err?: Error | null) => void),
    callback?: (err?: Error | null) => void,
  ) => {
    if (protocolWriteDepth > 0) {
      if (typeof encodingOrCallback === "function") {
        return originalWrite(buffer, encodingOrCallback);
      }

      return originalWrite(buffer, encodingOrCallback, callback);
    }

    throw stdoutReservedError();
  };
  const blockedConsoleWrite = (..._args: readonly unknown[]) => {
    throw stdoutReservedError();
  };
  const blockedConsoleDir: typeof globalThis.console.dir = (
    _item?: unknown,
    _options?: unknown,
  ) => {
    throw stdoutReservedError();
  };

  process.stdout.write = writeGuard;
  globalThis.console.log = blockedConsoleWrite;
  globalThis.console.info = blockedConsoleWrite;
  globalThis.console.debug = blockedConsoleWrite;
  globalThis.console.dir = blockedConsoleDir;

  return {
    writeProtocolBytes: async (payload: Uint8Array) => {
      protocolWriteDepth += 1;
      try {
        await writeBytes(output, payload);
      } finally {
        protocolWriteDepth -= 1;
      }
    },
    restore: () => {
      process.stdout.write = originalWrite;
      globalThis.console.log = originalConsoleLog;
      globalThis.console.info = originalConsoleInfo;
      globalThis.console.debug = originalConsoleDebug;
      globalThis.console.dir = originalConsoleDir;
    },
  };
}

export async function runRuntimeExtensionWorkerStdio(
  definition: RuntimeExtensionDefinition,
  options: RuntimeExtensionWorkerStdioOptions = {},
): Promise<Result<RuntimeExtensionAck, RuntimeExtensionError>> {
  const runtime = tryCreateRuntimeExtensionWorkerRuntime(definition);
  if (isErr(runtime)) {
    return runtime;
  }

  const input = options.input ?? process.stdin;
  const output = options.output ?? process.stdout;
  const errorOutput = options.error ?? process.stderr;
  const stdoutGuard = createWorkerStdoutGuard(
    output,
    options.guardProcessStdout ?? output === process.stdout,
  );
  const frameReader = new RuntimeExtensionFrameReader(input);

  const writeResponse = async (response: RuntimeExtensionWorkerResponse): Promise<void> => {
    await stdoutGuard.writeProtocolBytes(
      encodeRuntimeExtensionWorkerJsonFrame(
        response.tag,
        serializeRuntimeExtensionWorkerResponseWire(response),
      ),
    );
  };

  const processPacketEvents = async (
    events: readonly RuntimePacketEvent[],
    index = 0,
  ): Promise<void> => {
    const event = events[index];
    if (event === undefined) {
      return;
    }

    await reportRuntimeHookError(errorOutput, await runtime.value.handlePacketEvent(event));
    await processPacketEvents(events, index + 1);
  };

  const processProviderEvents = async (
    events: readonly RuntimeProviderEvent[],
    index = 0,
  ): Promise<void> => {
    const event = events[index];
    if (event === undefined) {
      return;
    }

    await reportRuntimeHookError(errorOutput, await runtime.value.handleProviderEvent(event));
    await processProviderEvents(events, index + 1);
  };

  const processNextFrame = async (): Promise<
    Result<RuntimeExtensionAck, RuntimeExtensionError>
  > => {
    const frame = await frameReader.nextFrame();
    if (isErr(frame)) {
      return frame;
    }
    if (frame.value === undefined) {
      return err(
        runtimeExtensionProtocolError(
          "message",
          "worker stdin closed before a shutdown frame was received",
        ),
      );
    }

    const payload = parseJsonPayload(frame.value.payload);
    if (isErr(payload)) {
      return payload;
    }

    const frameTag = parseRuntimeExtensionWorkerHostMessageTag(frame.value.tag);
    if (isErr(frameTag)) {
      return frameTag;
    }

    switch (frameTag.value) {
      case RuntimeExtensionWorkerHostMessageTag.GetManifest: {
        const response = await runtime.value.handleMessage({
          tag: RuntimeExtensionWorkerHostMessageTag.GetManifest,
        });
        await writeResponse(response);
        return processNextFrame();
      }
      case RuntimeExtensionWorkerHostMessageTag.Start: {
        const payloadRecord = parseJsonRecord(payload.value, "message");
        if (isErr(payloadRecord)) {
          return payloadRecord;
        }
        const message = tryParseRuntimeExtensionWorkerHostMessageWire({
          tag: RuntimeExtensionWorkerHostMessageTag.Start,
          context: payloadRecord.value.context,
        });
        if (isErr(message)) {
          return message;
        }
        const response = await runtime.value.handleMessage(message.value);
        await writeResponse(response);
        return processNextFrame();
      }
      case RuntimeExtensionWorkerHostMessageTag.DeliverPacket: {
        const events = parseRuntimePacketEventBatchWire(payload.value);
        if (isErr(events)) {
          return events;
        }
        await processPacketEvents(events.value);
        return processNextFrame();
      }
      case RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent: {
        const events = parseRuntimeProviderEventBatchWire(payload.value);
        if (isErr(events)) {
          return events;
        }
        await processProviderEvents(events.value);
        return processNextFrame();
      }
      case RuntimeExtensionWorkerHostMessageTag.Shutdown: {
        const payloadRecord = parseJsonRecord(payload.value, "message");
        if (isErr(payloadRecord)) {
          return payloadRecord;
        }
        const message = tryParseRuntimeExtensionWorkerHostMessageWire({
          tag: RuntimeExtensionWorkerHostMessageTag.Shutdown,
          context: payloadRecord.value.context,
        });
        if (isErr(message)) {
          return message;
        }
        const response = await runtime.value.handleMessage(message.value);
        await writeResponse(response);
        return ok(runtimeExtensionAck());
      }
    }

    return err(
      runtimeExtensionProtocolError(
        "message.tag",
        "message.tag must be a supported runtime extension worker message tag",
        JSON.stringify(frameTag.value),
      ),
    );
  };

  try {
    return await processNextFrame();
  } finally {
    stdoutGuard.restore();
  }
}
