import {
  ExtensionCapability,
  ExtensionStreamVisibilityTag,
  RuntimePacketSourceKind,
  RuntimePacketTransport,
  extensionName,
  extensionResourceId,
  isErr,
  sharedExtensionStream,
  socketAddress,
  tryCreateRuntimeExtensionWorkerManifest,
  udpListenerResource,
} from "../dist/index.js";

const extension = extensionName("demo-shared-udp-extension");
if (isErr(extension)) {
  process.stderr.write(`${extension.error.message}\n`);
  process.exit(1);
}

const resourceId = extensionResourceId("demo-udp");
if (isErr(resourceId)) {
  process.stderr.write(`${resourceId.error.message}\n`);
  process.exit(1);
}

const bindAddress = socketAddress("127.0.0.1:21011");
if (isErr(bindAddress)) {
  process.stderr.write(`${bindAddress.error.message}\n`);
  process.exit(1);
}

const sharedVisibility = sharedExtensionStream("demo-stream");
if (isErr(sharedVisibility)) {
  process.stderr.write(`${sharedVisibility.error.message}\n`);
  process.exit(1);
}

const udpResource = udpListenerResource(
  resourceId.value,
  bindAddress.value,
  sharedVisibility.value,
);
if (isErr(udpResource)) {
  process.stderr.write(`${udpResource.error.message}\n`);
  process.exit(1);
}

const manifest = tryCreateRuntimeExtensionWorkerManifest({
  sdkVersion: "0.1.0",
  extensionName: extension.value,
  capabilities: [
    ExtensionCapability.BindUdp,
    ExtensionCapability.ObserveSharedExtensionStream,
  ],
  resources: [udpResource.value],
  subscriptions: [
    {
      sourceKind: RuntimePacketSourceKind.ExtensionResource,
      transport: RuntimePacketTransport.Udp,
      ownerExtension: extension.value,
      resourceId: resourceId.value,
    },
    {
      sourceKind: RuntimePacketSourceKind.ExtensionResource,
      ...(sharedVisibility.value.tag === ExtensionStreamVisibilityTag.Shared
        ? { sharedTag: sharedVisibility.value.sharedTag }
        : {}),
    },
  ],
});

if (isErr(manifest)) {
  process.stderr.write(`${manifest.error.message}\n`);
  process.exit(1);
}

process.stdout.write(`${JSON.stringify(manifest.value, undefined, 2)}\n`);
