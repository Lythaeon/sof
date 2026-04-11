import { PassThrough } from "node:stream";

import {
  ExtensionCapability,
  RuntimeExtensionWorkerHostMessageTag,
  SdkLanguage,
  extensionName,
  isErr,
  ok,
  runtimeExtensionAck,
  serializeRuntimeExtensionWorkerHostMessageWire,
  sofRuntimeExtensionNameEnvVarName,
  socketAddress,
  tryDefineApp,
  tryDefinePlugin,
  tryRunSelectedPlugin,
} from "../dist/index.js";

async function main(): Promise<number> {
  const pluginName = extensionName("demo-plugin");
  if (isErr(pluginName)) {
    process.stderr.write(`${pluginName.error.message}\n`);
    return 1;
  }

  const localAddress = socketAddress("127.0.0.1:21011");
  if (isErr(localAddress)) {
    process.stderr.write(`${localAddress.error.message}\n`);
    return 1;
  }

  let observedPacketLog = "";
  const plugin = tryDefinePlugin({
    name: pluginName.value,
    capabilities: [ExtensionCapability.ObserveObserverIngress],
    onStart: () => ok(runtimeExtensionAck()),
    onPacket: (event) => {
      observedPacketLog = `received ${event.bytes.length} bytes from ${String(event.source.localAddress)}`;
      return ok(runtimeExtensionAck());
    },
    onStop: () => ok(runtimeExtensionAck()),
  });
  if (isErr(plugin)) {
    process.stderr.write(`${plugin.error.message}\n`);
    return 1;
  }

  const app = tryDefineApp({
    name: "demo-sof-app",
    plugins: [plugin.value],
  });
  if (isErr(app)) {
    process.stderr.write(`${app.error.message}\n`);
    return 1;
  }

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

  const runner = tryRunSelectedPlugin(
    app.value,
    {
      [sofRuntimeExtensionNameEnvVarName]: pluginName.value,
    },
    {
      input,
      output,
      error: errorOutput,
    },
  );

  input.write(
    `${JSON.stringify(
      serializeRuntimeExtensionWorkerHostMessageWire({
        tag: RuntimeExtensionWorkerHostMessageTag.Start,
        context: {
          extensionName: pluginName.value,
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
          localAddress: localAddress.value,
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
          extensionName: pluginName.value,
        },
      }),
    )}\n`,
  );
  input.end();

  const result = await runner;
  if (isErr(result)) {
    process.stderr.write(`${result.error.message}\n`);
    return 1;
  }

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

  return 0;
}

process.exitCode = await main();
