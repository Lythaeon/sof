import {
  ExtensionCapability,
  RuntimeExtensionWorkerHostMessageTag,
  RuntimePacketEventClass,
  RuntimePacketSourceKind,
  RuntimePacketTransport,
  SdkLanguage,
  createRuntimePacketEvent,
  extensionName,
  isErr,
  ok,
  runtimeExtensionAck,
  socketAddress,
  tryDefineRuntimeExtension,
  tryCreateRuntimeExtensionWorkerManifest,
  tryCreateRuntimeExtensionWorkerRuntime,
} from "../dist/index.js";

const extension = extensionName("demo-extension-worker");
if (isErr(extension)) {
  process.stderr.write(`${extension.error.message}\n`);
  process.exit(1);
}

const localAddress = socketAddress("127.0.0.1:21011");
if (isErr(localAddress)) {
  process.stderr.write(`${localAddress.error.message}\n`);
  process.exit(1);
}

const manifest = tryCreateRuntimeExtensionWorkerManifest({
  sdkVersion: "0.1.0",
  extensionName: extension.value,
  capabilities: [ExtensionCapability.ObserveObserverIngress],
  subscriptions: [
    {
      sourceKind: RuntimePacketSourceKind.ObserverIngress,
      transport: RuntimePacketTransport.Udp,
      eventClass: RuntimePacketEventClass.Packet,
      localAddress: localAddress.value,
    },
  ],
});
if (isErr(manifest)) {
  process.stderr.write(`${manifest.error.message}\n`);
  process.exit(1);
}

const definition = tryDefineRuntimeExtension({
  manifest: manifest.value,
  onReady: () => ok(runtimeExtensionAck()),
  onPacketReceived: (event) => {
    process.stdout.write(
      `received ${event.bytes.length} bytes from ${String(event.source.localAddress)}\n`,
    );
    return ok(runtimeExtensionAck());
  },
  onShutdown: () => ok(runtimeExtensionAck()),
});
if (isErr(definition)) {
  process.stderr.write(`${definition.error.message}\n`);
  process.exit(1);
}

const worker = tryCreateRuntimeExtensionWorkerRuntime(definition.value);
if (isErr(worker)) {
  process.stderr.write(`${worker.error.message}\n`);
  process.exit(1);
}

const started = await worker.value.handleMessage({
  tag: RuntimeExtensionWorkerHostMessageTag.Start,
  context: {
    extensionName: extension.value,
  },
});

const delivered = await worker.value.handleMessage({
  tag: RuntimeExtensionWorkerHostMessageTag.DeliverPacket,
  event: createRuntimePacketEvent(
    {
      kind: RuntimePacketSourceKind.ObserverIngress,
      transport: RuntimePacketTransport.Udp,
      eventClass: RuntimePacketEventClass.Packet,
      localAddress: localAddress.value,
    },
    Uint8Array.from([1, 2, 3, 4]),
    Date.now(),
  ),
});

const shutdown = await worker.value.handleMessage({
  tag: RuntimeExtensionWorkerHostMessageTag.Shutdown,
  context: {
    extensionName: extension.value,
  },
});

process.stdout.write(
  `${JSON.stringify(
    {
      sdkLanguage: SdkLanguage.TypeScript,
      started,
      delivered,
      shutdown,
    },
    undefined,
    2,
  )}\n`,
);
