import { Buffer } from "node:buffer";
import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import test from "node:test";

import { ResultTag, isErr, ok } from "../result.js";
import {
  ExtensionCapability,
  RuntimeExtensionWorkerHostMessageTag,
  RuntimeExtensionWorkerResponseTag,
  RuntimeProviderEventKind,
  extensionName,
  runtimeExtensionAck,
  socketAddress,
  tryCreateRuntimeExtensionWorkerManifest,
  tryDefineRuntimeExtension,
} from "./runtime-extension.js";
import {
  runRuntimeExtensionWorkerStdio,
  serializeRuntimeExtensionWorkerHostMessageWire,
  serializeRuntimePacketEventWire,
  tryParseRuntimeExtensionWorkerHostMessageWire,
  tryParseRuntimePacketEventWire,
} from "./runtime-extension-stdio.js";

const workerFrameHeaderBytes = 5;

function encodeJsonFrame(tag: number, payload: unknown): Buffer {
  const payloadBytes = Buffer.from(JSON.stringify(payload), "utf8");
  const frame = Buffer.alloc(workerFrameHeaderBytes + payloadBytes.length);
  frame[0] = tag;
  frame.writeUInt32LE(payloadBytes.length, 1);
  payloadBytes.copy(frame, workerFrameHeaderBytes);
  return frame;
}

function decodeFrames(buffer: Buffer): Array<{ tag: number; payload: unknown }> {
  const frames: Array<{ tag: number; payload: unknown }> = [];
  let offset = 0;

  while (offset < buffer.length) {
    const tag = buffer[offset] ?? 0;
    const payloadLength = buffer.readUInt32LE(offset + 1);
    const frameEnd = offset + workerFrameHeaderBytes + payloadLength;
    const payload = JSON.parse(
      buffer.subarray(offset + workerFrameHeaderBytes, frameEnd).toString("utf8"),
    ) as unknown;
    frames.push({ tag, payload });
    offset = frameEnd;
  }

  return frames;
}

test("runtime extension wire helpers round-trip packet delivery messages", () => {
  const parsedExtensionName = extensionName("wire-demo");
  const localAddress = socketAddress("127.0.0.1:21011");

  assert.equal(parsedExtensionName.tag, ResultTag.Ok);
  assert.equal(localAddress.tag, ResultTag.Ok);
  if (parsedExtensionName.tag !== ResultTag.Ok || localAddress.tag !== ResultTag.Ok) {
    return;
  }

  const wireMessage = serializeRuntimeExtensionWorkerHostMessageWire({
    tag: RuntimeExtensionWorkerHostMessageTag.DeliverPacket,
    event: {
      source: {
        kind: 1,
        transport: 1,
        eventClass: 1,
        ownerExtension: parsedExtensionName.value,
        localAddress: localAddress.value,
      },
      bytes: Uint8Array.from([1, 2, 3]),
      observedUnixMs: 42,
    },
  });

  const parsedWireMessage = tryParseRuntimeExtensionWorkerHostMessageWire(wireMessage);
  assert.equal(parsedWireMessage.tag, ResultTag.Ok);
  if (parsedWireMessage.tag !== ResultTag.Ok) {
    return;
  }

  assert.equal(parsedWireMessage.value.tag, RuntimeExtensionWorkerHostMessageTag.DeliverPacket);
  if (parsedWireMessage.value.tag !== RuntimeExtensionWorkerHostMessageTag.DeliverPacket) {
    return;
  }

  assert.deepEqual(Array.from(parsedWireMessage.value.event.bytes), [1, 2, 3]);
  assert.equal(parsedWireMessage.value.event.source.localAddress, localAddress.value);

  const parsedEvent = tryParseRuntimePacketEventWire(
    serializeRuntimePacketEventWire(parsedWireMessage.value.event),
  );
  assert.equal(parsedEvent.tag, ResultTag.Ok);
});

test("runtime extension wire helpers round-trip provider events", () => {
  const wireMessage = serializeRuntimeExtensionWorkerHostMessageWire({
    tag: RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent,
    event: {
      kind: RuntimeProviderEventKind.TransactionStatus,
      slot: 1,
      commitmentStatus: 1,
      signature: "sig",
      isVote: false,
    },
  });

  const parsedWireMessage = tryParseRuntimeExtensionWorkerHostMessageWire(wireMessage);
  assert.equal(parsedWireMessage.tag, ResultTag.Ok);
  if (parsedWireMessage.tag !== ResultTag.Ok) {
    return;
  }

  assert.equal(
    parsedWireMessage.value.tag,
    RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent,
  );
  if (parsedWireMessage.value.tag !== RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent) {
    return;
  }

  assert.equal(parsedWireMessage.value.event.kind, RuntimeProviderEventKind.TransactionStatus);
  assert.equal(parsedWireMessage.value.event.slot, 1);
});

test("runtime extension stdio worker processes framed batch protocol messages", async () => {
  const input = new PassThrough();
  const output = new PassThrough();
  const errorOutput = new PassThrough();
  const outputChunks: Buffer[] = [];
  let errorText = "";

  output.on("data", (chunk: Buffer | string) => {
    outputChunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  });
  errorOutput.setEncoding("utf8");
  errorOutput.on("data", (chunk: string) => {
    errorText += chunk;
  });

  const manifest = tryCreateRuntimeExtensionWorkerManifest({
    sdkVersion: "0.1.0",
    extensionName: "stdio-demo",
    capabilities: [ExtensionCapability.ObserveObserverIngress],
  });
  assert.equal(manifest.tag, ResultTag.Ok);
  if (manifest.tag !== ResultTag.Ok) {
    return;
  }

  const definition = tryDefineRuntimeExtension({
    manifest: manifest.value,
    onReady: () => ok(runtimeExtensionAck()),
    onPacketReceived: () => ok(runtimeExtensionAck()),
    onProviderEvent: () => ok(runtimeExtensionAck()),
    onShutdown: () => ok(runtimeExtensionAck()),
  });
  assert.equal(definition.tag, ResultTag.Ok);
  if (definition.tag !== ResultTag.Ok) {
    return;
  }

  const runner = runRuntimeExtensionWorkerStdio(definition.value, {
    input,
    output,
    error: errorOutput,
  });

  input.write(encodeJsonFrame(RuntimeExtensionWorkerHostMessageTag.GetManifest, {}));
  input.write(
    encodeJsonFrame(RuntimeExtensionWorkerHostMessageTag.Start, {
      context: {
        extensionName: manifest.value.extensionName,
      },
    }),
  );
  input.write(
    encodeJsonFrame(RuntimeExtensionWorkerHostMessageTag.DeliverPacket, {
      events: [
        serializeRuntimePacketEventWire({
          source: {
            kind: 1,
            transport: 1,
            eventClass: 1,
          },
          bytes: Uint8Array.from([1, 2, 3, 4]),
          observedUnixMs: 100,
        }),
      ],
    }),
  );
  input.write(
    encodeJsonFrame(RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent, {
      events: [
        {
          kind: RuntimeProviderEventKind.TransactionStatus,
          slot: 100,
          commitmentStatus: 1,
          signature: "sig",
          isVote: false,
        },
      ],
    }),
  );
  input.write(
    encodeJsonFrame(RuntimeExtensionWorkerHostMessageTag.Shutdown, {
      context: {
        extensionName: manifest.value.extensionName,
      },
    }),
  );
  input.end();

  const runnerResult = await runner;
  assert.equal(runnerResult.tag, ResultTag.Ok);
  assert.equal(errorText, "");

  const responses = decodeFrames(Buffer.concat(outputChunks));
  assert.equal(responses.length, 3);
  assert.equal(responses[0]?.tag, RuntimeExtensionWorkerResponseTag.Manifest);
  assert.equal(responses[1]?.tag, RuntimeExtensionWorkerResponseTag.Started);
  assert.equal(responses[2]?.tag, RuntimeExtensionWorkerResponseTag.ShutdownComplete);
});

test("runtime extension stdio worker rejects malformed framed protocol messages", async () => {
  const input = new PassThrough();
  const output = new PassThrough();
  const errorOutput = new PassThrough();

  const manifest = tryCreateRuntimeExtensionWorkerManifest({
    sdkVersion: "0.1.0",
    extensionName: "bad-wire-demo",
    capabilities: [ExtensionCapability.ObserveObserverIngress],
  });
  assert.equal(manifest.tag, ResultTag.Ok);
  if (manifest.tag !== ResultTag.Ok) {
    return;
  }

  const definition = tryDefineRuntimeExtension({
    manifest: manifest.value,
    onReady: () => ok(runtimeExtensionAck()),
    onPacketReceived: () => ok(runtimeExtensionAck()),
    onShutdown: () => ok(runtimeExtensionAck()),
  });
  assert.equal(definition.tag, ResultTag.Ok);
  if (definition.tag !== ResultTag.Ok) {
    return;
  }

  const runner = runRuntimeExtensionWorkerStdio(definition.value, {
    input,
    output,
    error: errorOutput,
  });

  input.write(
    encodeJsonFrame(RuntimeExtensionWorkerHostMessageTag.DeliverPacket, {
      events: [
        {
          source: { kind: 99 },
          bytesBase64: Buffer.from([1]).toString("base64"),
          observedUnixMs: 1,
        },
      ],
    }),
  );
  input.end();

  const runnerResult = await runner;
  assert.equal(isErr(runnerResult), true);
  if (!isErr(runnerResult)) {
    return;
  }
  assert.match(runnerResult.error.field, /event\.source\.kind/);
});

test("runtime extension stdio worker hard-blocks stdout writes inside callbacks", async () => {
  const input = new PassThrough();
  const output = new PassThrough();
  const errorOutput = new PassThrough();
  const outputChunks: Buffer[] = [];
  let errorText = "";

  output.on("data", (chunk: Buffer | string) => {
    outputChunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  });
  errorOutput.setEncoding("utf8");
  errorOutput.on("data", (chunk: string) => {
    errorText += chunk;
  });

  const manifest = tryCreateRuntimeExtensionWorkerManifest({
    sdkVersion: "0.1.0",
    extensionName: "stdout-guard-demo",
    capabilities: [ExtensionCapability.ObserveObserverIngress],
  });
  assert.equal(manifest.tag, ResultTag.Ok);
  if (manifest.tag !== ResultTag.Ok) {
    return;
  }

  const definition = tryDefineRuntimeExtension({
    manifest: manifest.value,
    onPacketReceived: () => {
      process.stdout.write("forbidden\n");
      return ok(runtimeExtensionAck());
    },
    onShutdown: () => ok(runtimeExtensionAck()),
  });
  assert.equal(definition.tag, ResultTag.Ok);
  if (definition.tag !== ResultTag.Ok) {
    return;
  }

  const runner = runRuntimeExtensionWorkerStdio(definition.value, {
    input,
    output,
    error: errorOutput,
    guardProcessStdout: true,
  });

  input.write(
    encodeJsonFrame(RuntimeExtensionWorkerHostMessageTag.DeliverPacket, {
      events: [
        serializeRuntimePacketEventWire({
          source: {
            kind: 1,
            transport: 1,
            eventClass: 1,
          },
          bytes: Uint8Array.from([1, 2, 3, 4]),
          observedUnixMs: 100,
        }),
      ],
    }),
  );
  input.write(
    encodeJsonFrame(RuntimeExtensionWorkerHostMessageTag.Shutdown, {
      context: {
        extensionName: manifest.value.extensionName,
      },
    }),
  );
  input.end();

  const runnerResult = await runner;
  assert.equal(runnerResult.tag, ResultTag.Ok);
  assert.match(errorText, /stdout is reserved for protocol messages/i);

  const responses = decodeFrames(Buffer.concat(outputChunks));
  assert.equal(responses.length, 1);
  const shutdownResponse = responses[0];
  assert.notEqual(shutdownResponse, undefined);
  if (shutdownResponse === undefined) {
    return;
  }
  assert.equal(shutdownResponse.tag, RuntimeExtensionWorkerResponseTag.ShutdownComplete);
  assert.equal(
    (shutdownResponse.payload as { result?: { tag?: number } }).result?.tag,
    ResultTag.Ok,
  );
});
