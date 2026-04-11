import { PassThrough } from "node:stream";

import {
  ExtensionCapability,
  RuntimeExtensionWorkerHostMessageTag,
  SdkLanguage,
  type Result,
  extensionName,
  isErr,
  ok,
  runtimeExtensionAck,
  runRuntimeExtensionWorkerStdio,
  serializeRuntimeExtensionWorkerHostMessageWire,
  socketAddress,
  tryDefineRuntimeExtension,
  tryCreateRuntimeExtensionWorkerManifest,
} from "../dist/index.js";

function expectOk<Value, Error extends { readonly message: string }>(
  result: Result<Value, Error>,
): Value {
  if (isErr(result)) {
    throw new Error(result.error.message);
  }

  return result.value;
}

const extension = expectOk(extensionName("demo-extension-worker"));
const localAddress = expectOk(socketAddress("127.0.0.1:21011"));

const manifest = expectOk(
  tryCreateRuntimeExtensionWorkerManifest({
    sdkVersion: "0.1.0",
    extensionName: extension,
    capabilities: [ExtensionCapability.ObserveObserverIngress],
  }),
);

let observedPacketLog = "";
const definition = expectOk(
  tryDefineRuntimeExtension({
    manifest,
    onReady: () => ok(runtimeExtensionAck()),
    onPacketReceived: (event) => {
      observedPacketLog = `received ${event.bytes.length} bytes from ${String(event.source.localAddress)}`;
      return ok(runtimeExtensionAck());
    },
    onShutdown: () => ok(runtimeExtensionAck()),
  }),
);

const input = new PassThrough();
const output = new PassThrough();
const errorOutput = new PassThrough();

let protocolOutput = "";
let protocolErrors = "";
output.setEncoding("utf8");
errorOutput.setEncoding("utf8");
output.on("data", (chunk: string) => {
  protocolOutput += chunk;
});
errorOutput.on("data", (chunk: string) => {
  protocolErrors += chunk;
});

const runner = runRuntimeExtensionWorkerStdio(definition, {
  input,
  output,
  error: errorOutput,
});

input.write(
  `${JSON.stringify(
    serializeRuntimeExtensionWorkerHostMessageWire({
      tag: RuntimeExtensionWorkerHostMessageTag.Start,
      context: {
        extensionName: extension,
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
        localAddress,
      },
      bytes: [1, 2, 3, 4],
      observedUnixMs: Date.now(),
    },
  })}\n`,
);
input.write(
  `${JSON.stringify(
    serializeRuntimeExtensionWorkerHostMessageWire({
      tag: RuntimeExtensionWorkerHostMessageTag.Shutdown,
      context: {
        extensionName: extension,
      },
    }),
  )}\n`,
);
input.end();

expectOk(await runner);

process.stdout.write(
  `${JSON.stringify(
    {
      sdkLanguage: SdkLanguage.TypeScript,
      observedPacketLog,
      protocolErrors: protocolErrors.trim(),
      responses: protocolOutput
        .trim()
        .split("\n")
        .filter((line) => line !== "")
        .map((line) => JSON.parse(line) as unknown),
    },
    undefined,
    2,
  )}\n`,
);
