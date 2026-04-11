import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import test from "node:test";

import { isErr, isOk, ok } from "./result.js";
import {
  ExtensionCapability,
  RuntimeDeliveryProfile,
  RuntimeExtensionWorkerHostMessageTag,
  SofApplication,
  createAppLaunch,
  createRuntimeConfigForProfile,
  defineRuntimeExtension,
  defineSofApplication,
  runtimeExtensionAck,
  sofApplicationNameEnvVarName,
  serializeRuntimeExtensionWorkerHostMessageWire,
  sofRuntimeExtensionNameEnvVarName,
  tryDefineApp,
  tryDefinePlugin,
  tryCreateRuntimeExtensionWorkerManifest,
  tryRunSelectedPlugin,
} from "./index.js";

test("sof application produces node launch specs from ts-authored runtime and extensions", () => {
  const plugin = tryDefinePlugin({
    name: "launch-spec-extension",
    capabilities: [ExtensionCapability.ObserveObserverIngress],
    onStart: () => ok(runtimeExtensionAck()),
    onPacket: () => ok(runtimeExtensionAck()),
    onStop: () => ok(runtimeExtensionAck()),
  });

  assert.equal(isOk(plugin), true);
  if (!isOk(plugin)) {
    return;
  }

  const app = tryDefineApp({
    name: "demo-sof-app",
    runtime: createRuntimeConfigForProfile(RuntimeDeliveryProfile.Balanced),
    plugins: [plugin.value],
  });
  assert.equal(isOk(app), true);
  if (!isOk(app)) {
    return;
  }

  const launchSpec = createAppLaunch(app.value, {
    workerEntrypoint: "./dist/worker.js",
    workerCommand: "node",
    workerArgs: ["--enable-source-maps"],
  });

  assert.equal(launchSpec.appName, "demo-sof-app");
  assert.equal(launchSpec.runtimeEnvironment.SOF_RUNTIME_DELIVERY_PROFILE, "balanced");
  assert.equal(launchSpec.plugins.length, 1);
  assert.equal(launchSpec.runtimeExtensions.length, 1);
  assert.deepEqual(launchSpec.runtimeExtensions[0]?.args, [
    "--enable-source-maps",
    "./dist/worker.js",
  ]);
  assert.equal(
    launchSpec.runtimeExtensions[0]?.environment[sofApplicationNameEnvVarName],
    "demo-sof-app",
  );
  assert.equal(
    launchSpec.runtimeExtensions[0]?.environment[sofRuntimeExtensionNameEnvVarName],
    "launch-spec-extension",
  );
});

test("sof application rejects duplicate runtime extension names", () => {
  const manifest = tryCreateRuntimeExtensionWorkerManifest({
    sdkVersion: "0.1.0",
    extensionName: "duplicate-extension",
    capabilities: [ExtensionCapability.ObserveObserverIngress],
  });

  assert.equal(isOk(manifest), true);
  if (!isOk(manifest)) {
    return;
  }

  const duplicateApp = SofApplication.tryCreate({
    name: "duplicate-app",
    runtimeExtensions: [
      defineRuntimeExtension({
        manifest: manifest.value,
        onReady: () => ok(runtimeExtensionAck()),
      }),
      defineRuntimeExtension({
        manifest: manifest.value,
        onReady: () => ok(runtimeExtensionAck()),
      }),
    ],
  });

  assert.equal(isErr(duplicateApp), true);
  if (isErr(duplicateApp)) {
    assert.match(duplicateApp.error.message, /registered more than once/);
  }
});

test("sof application runs one plugin selected from environment", async () => {
  const manifest = tryCreateRuntimeExtensionWorkerManifest({
    sdkVersion: "0.1.0",
    extensionName: "selected-extension",
    capabilities: [ExtensionCapability.ObserveObserverIngress],
  });

  assert.equal(isOk(manifest), true);
  if (!isOk(manifest)) {
    return;
  }

  const app = defineSofApplication({
    name: "selection-app",
    runtimeExtensions: [
      defineRuntimeExtension({
        manifest: manifest.value,
        onReady: () => ok(runtimeExtensionAck()),
        onPacketReceived: () => ok(runtimeExtensionAck()),
        onShutdown: () => ok(runtimeExtensionAck()),
      }),
    ],
  });

  const input = new PassThrough();
  const output = new PassThrough();
  let responseText = "";

  output.setEncoding("utf8");
  output.on("data", (chunk: string) => {
    responseText += chunk;
  });

  const runner = tryRunSelectedPlugin(
    app,
    {
      [sofRuntimeExtensionNameEnvVarName]: "selected-extension",
    },
    {
      input,
      output,
    },
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

  const result = await runner;
  assert.equal(isOk(result), true);
  assert.match(responseText, /"tag":2/);
  assert.match(responseText, /"tag":4/);
});

test("sof application reports missing runtime extension selection with available names", async () => {
  const manifest = tryCreateRuntimeExtensionWorkerManifest({
    sdkVersion: "0.1.0",
    extensionName: "available-extension",
    capabilities: [ExtensionCapability.ObserveObserverIngress],
  });

  assert.equal(isOk(manifest), true);
  if (!isOk(manifest)) {
    return;
  }

  const app = defineSofApplication({
    name: "selection-app",
    runtimeExtensions: [
      defineRuntimeExtension({
        manifest: manifest.value,
        onReady: () => ok(runtimeExtensionAck()),
      }),
    ],
  });

  const result = await tryRunSelectedPlugin(app, {});

  assert.equal(isErr(result), true);
  if (isErr(result)) {
    assert.equal(result.error.field, sofRuntimeExtensionNameEnvVarName);
    if ("availableExtensionNames" in result.error) {
      assert.deepEqual(result.error.availableExtensionNames, ["available-extension"]);
    }
  }
});
