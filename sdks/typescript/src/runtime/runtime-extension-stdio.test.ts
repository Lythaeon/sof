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

test("runtime extension stdio worker processes newline-delimited protocol messages", async () => {
  const input = new PassThrough();
  const output = new PassThrough();
  const errorOutput = new PassThrough();
  let outputText = "";
  let errorText = "";

  output.setEncoding("utf8");
  errorOutput.setEncoding("utf8");
  output.on("data", (chunk: string) => {
    outputText += chunk;
  });
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

  input.write(
    `${JSON.stringify(
      serializeRuntimeExtensionWorkerHostMessageWire({
        tag: RuntimeExtensionWorkerHostMessageTag.GetManifest,
      }),
    )}\n`,
  );
  input.write(
    `${JSON.stringify(
      serializeRuntimeExtensionWorkerHostMessageWire({
        tag: RuntimeExtensionWorkerHostMessageTag.Start,
        context: {
          extensionName: manifest.value.extensionName,
        },
      }),
    )}\n`,
  );
  input.write(
    `${JSON.stringify({
      tag: RuntimeExtensionWorkerHostMessageTag.DeliverPacket,
      event: {
        source: {
          kind: 1,
          transport: 1,
          eventClass: 1,
        },
        bytes: [1, 2, 3, 4],
        observedUnixMs: 100,
      },
    })}\n`,
  );
  input.write(
    `${JSON.stringify({
      tag: RuntimeExtensionWorkerHostMessageTag.DeliverProviderEvent,
      event: {
        kind: RuntimeProviderEventKind.TransactionStatus,
        slot: 100,
        commitmentStatus: 1,
        signature: "sig",
        isVote: false,
      },
    })}\n`,
  );
  input.write(
    `${JSON.stringify(
      serializeRuntimeExtensionWorkerHostMessageWire({
        tag: RuntimeExtensionWorkerHostMessageTag.Shutdown,
        context: {
          extensionName: manifest.value.extensionName,
        },
      }),
    )}\n`,
  );
  input.end();

  const runnerResult = await runner;
  assert.equal(runnerResult.tag, ResultTag.Ok);
  assert.equal(errorText, "");

  const responses = outputText
    .trim()
    .split("\n")
    .filter((line) => line !== "")
    .map((line) => JSON.parse(line) as { tag: number });

  assert.equal(responses.length, 5);
  assert.equal(responses[0]?.tag, RuntimeExtensionWorkerResponseTag.Manifest);
  assert.equal(responses[1]?.tag, RuntimeExtensionWorkerResponseTag.Started);
  assert.equal(responses[2]?.tag, RuntimeExtensionWorkerResponseTag.EventHandled);
  assert.equal(responses[3]?.tag, RuntimeExtensionWorkerResponseTag.ProviderEventHandled);
  assert.equal(responses[4]?.tag, RuntimeExtensionWorkerResponseTag.ShutdownComplete);
});

test("runtime extension stdio worker rejects malformed protocol messages", async () => {
  const input = new PassThrough();
  const output = new PassThrough();
  const errorOutput = new PassThrough();
  let errorText = "";

  errorOutput.setEncoding("utf8");
  errorOutput.on("data", (chunk: string) => {
    errorText += chunk;
  });

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

  input.write('{"tag":3,"event":{"source":{"kind":99},"bytes":[1],"observedUnixMs":1}}\n');
  input.end();

  const runnerResult = await runner;
  assert.equal(isErr(runnerResult), true);
  assert.match(errorText, /event\.source\.kind/);
});

test("runtime extension stdio worker hard-blocks stdout writes inside callbacks", async () => {
  const input = new PassThrough();
  const output = new PassThrough();
  const errorOutput = new PassThrough();
  let outputText = "";

  output.setEncoding("utf8");
  output.on("data", (chunk: string) => {
    outputText += chunk;
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
    `${JSON.stringify({
      tag: RuntimeExtensionWorkerHostMessageTag.DeliverPacket,
      event: {
        source: {
          kind: 1,
          transport: 1,
          eventClass: 1,
        },
        bytes: [1, 2, 3, 4],
        observedUnixMs: 100,
      },
    })}\n`,
  );
  input.write(
    `${JSON.stringify(
      serializeRuntimeExtensionWorkerHostMessageWire({
        tag: RuntimeExtensionWorkerHostMessageTag.Shutdown,
        context: {
          extensionName: manifest.value.extensionName,
        },
      }),
    )}\n`,
  );
  input.end();

  const runnerResult = await runner;
  assert.equal(runnerResult.tag, ResultTag.Ok);

  const responses = outputText
    .trim()
    .split("\n")
    .filter((line) => line !== "")
    .map(
      (line) =>
        JSON.parse(line) as {
          tag: number;
          result?: { tag: number; error?: { message?: string; cause?: string } };
        },
    );

  assert.equal(responses.length, 2);
  assert.equal(responses[0]?.tag, RuntimeExtensionWorkerResponseTag.EventHandled);
  assert.equal(responses[0]?.result?.tag, ResultTag.Err);
  assert.match(
    responses[0]?.result?.error?.cause ?? "",
    /stdout is reserved for protocol messages/i,
  );
  assert.equal(responses[1]?.tag, RuntimeExtensionWorkerResponseTag.ShutdownComplete);
  assert.equal(responses[1]?.result?.tag, ResultTag.Ok);
});
