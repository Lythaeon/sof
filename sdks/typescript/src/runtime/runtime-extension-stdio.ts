import { once } from "node:events";
import { createInterface } from "node:readline";

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
const maxPacketByteValue = 255;

type JsonRecord = Record<string, unknown>;

export interface RuntimePacketEventWire {
  readonly source: RuntimePacketSource;
  readonly bytes: readonly number[];
  readonly observedUnixMs: number;
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
  readonly writeProtocolText: (text: string) => Promise<void>;
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

function parsePacketBytes(value: unknown): Result<Uint8Array, RuntimeExtensionError> {
  if (!Array.isArray(value)) {
    return err(
      runtimeExtensionProtocolError(
        "event.bytes",
        "event.bytes must be an array of byte values",
        JSON.stringify(value),
      ),
    );
  }

  const bytes = new Uint8Array(value.length);
  for (const [index, packetByte] of value.entries()) {
    if (
      typeof packetByte !== "number" ||
      !Number.isInteger(packetByte) ||
      packetByte < 0 ||
      packetByte > maxPacketByteValue
    ) {
      return err(
        runtimeExtensionProtocolError(
          `event.bytes[${index}]`,
          `event.bytes[${index}] must be an integer between 0 and ${maxPacketByteValue}`,
          JSON.stringify(packetByte),
        ),
      );
    }
    bytes[index] = packetByte;
  }

  return ok(bytes);
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
    bytes: Array.from(event.bytes),
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

  const bytes = parsePacketBytes(eventRecord.value.bytes);
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

async function writeText(output: NodeJS.WritableStream, text: string): Promise<void> {
  if (output.write(text)) {
    return;
  }

  await once(output, "drain");
}

function createWorkerStdoutGuard(
  output: NodeJS.WritableStream,
  enabled: boolean,
): WorkerStdoutGuard {
  if (!enabled) {
    return {
      writeProtocolText: (text: string) => writeText(output, text),
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
    writeProtocolText: async (text: string) => {
      protocolWriteDepth += 1;
      try {
        await writeText(output, text);
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
  const lineReader = createInterface({
    input,
    crlfDelay: Infinity,
  });

  try {
    for await (const line of lineReader) {
      const trimmed = line.trim();
      if (trimmed === "") {
        continue;
      }

      let parsedJson: unknown;
      try {
        parsedJson = JSON.parse(trimmed);
      } catch (error) {
        const cause = error instanceof Error ? error.message : String(error);
        return err(
          runtimeExtensionProtocolError(
            "message",
            "worker stdin line must be valid JSON",
            trimmed,
            cause,
          ),
        );
      }

      const message = tryParseRuntimeExtensionWorkerHostMessageWire(parsedJson);
      if (isErr(message)) {
        await writeText(errorOutput, `${message.error.message}\n`);
        return message;
      }

      const response = await runtime.value.handleMessage(message.value);
      await stdoutGuard.writeProtocolText(
        `${JSON.stringify(serializeRuntimeExtensionWorkerResponseWire(response))}\n`,
      );

      if (message.value.tag === RuntimeExtensionWorkerHostMessageTag.Shutdown) {
        return ok(runtimeExtensionAck());
      }
    }
  } finally {
    lineReader.close();
    stdoutGuard.restore();
  }

  return err(
    runtimeExtensionProtocolError(
      "message",
      "worker stdin closed before a shutdown message was received",
    ),
  );
}
