import {
  ExtensionCapability,
  ExtensionStreamVisibilityTag,
  Plugin,
  RuntimePacketSourceKind,
  RuntimePacketTransport,
  type Result,
  extensionName,
  extensionResourceId,
  isErr,
  sharedExtensionStream,
  socketAddress,
  udpListenerResource,
} from "../dist/index.js";

function expectOk<Value, Error extends { readonly message: string }>(
  result: Result<Value, Error>,
): Value {
  if (isErr(result)) {
    throw new Error(result.error.message);
  }

  return result.value;
}

const extension = expectOk(extensionName("demo-shared-udp-extension"));
const resourceId = expectOk(extensionResourceId("demo-udp"));
const bindAddress = expectOk(socketAddress("127.0.0.1:21011"));
const sharedVisibility = expectOk(sharedExtensionStream("demo-stream"));

const udpResource = expectOk(udpListenerResource(resourceId, bindAddress, sharedVisibility));

const plugin = new Plugin({
  name: extension,
  capabilities: [ExtensionCapability.BindUdp, ExtensionCapability.ObserveSharedExtensionStream],
  resources: [udpResource],
  subscriptions: [
    {
      sourceKind: RuntimePacketSourceKind.ExtensionResource,
      transport: RuntimePacketTransport.Udp,
      ownerExtension: extension,
      resourceId,
    },
    {
      sourceKind: RuntimePacketSourceKind.ExtensionResource,
      ...(sharedVisibility.tag === ExtensionStreamVisibilityTag.Shared
        ? { sharedTag: sharedVisibility.sharedTag }
        : {}),
    },
  ],
});

process.stdout.write(`${JSON.stringify(plugin.manifest, undefined, 2)}\n`);
