import assert from "node:assert/strict";
import test from "node:test";

import { isErr, isOk, ok } from "../result.js";
import {
  ExtensionCapability,
  ExtensionStreamVisibilityTag,
  ForeignWorkerKind,
  RuntimeExtensionErrorKind,
  RuntimeExtensionWorkerHostMessageTag,
  RuntimeExtensionWorkerResponseTag,
  RuntimePacketEventClass,
  RuntimePacketSourceKind,
  RuntimePacketTransport,
  SdkLanguage,
  createRuntimeExtensionWorkerManifest,
  createRuntimePacketEvent,
  defineRuntimeExtension,
  extensionName,
  extensionResourceId,
  packetSubscriptionMatches,
  runtimeExtensionAck,
  sharedExtensionStream,
  socketAddress,
  tryCreateRuntimeExtensionWorkerManifest,
  tryCreateRuntimeExtensionWorkerRuntime,
  udpListenerResource,
} from "../runtime.js";

test("runtime extension manifest creation validates stable typed metadata", () => {
  const parsedExtensionName = extensionName("demo-extension");
  const parsedResourceId = extensionResourceId("demo-udp");
  const bindAddress = socketAddress("127.0.0.1:21011");
  const visibility = sharedExtensionStream("demo-stream");

  assert.equal(isOk(parsedExtensionName), true);
  assert.equal(isOk(parsedResourceId), true);
  assert.equal(isOk(bindAddress), true);
  assert.equal(isOk(visibility), true);

  if (
    isOk(parsedExtensionName) &&
    isOk(parsedResourceId) &&
    isOk(bindAddress) &&
    isOk(visibility)
  ) {
    const resource = udpListenerResource(
      parsedResourceId.value,
      bindAddress.value,
      visibility.value,
    );

    assert.equal(isOk(resource), true);
    if (isOk(resource)) {
      const manifest = createRuntimeExtensionWorkerManifest({
        sdkVersion: "0.1.0",
        extensionName: parsedExtensionName.value,
        capabilities: [
          ExtensionCapability.BindUdp,
          ExtensionCapability.ObserveSharedExtensionStream,
        ],
        resources: [resource.value],
        subscriptions: [
          {
            sourceKind: RuntimePacketSourceKind.ExtensionResource,
            transport: RuntimePacketTransport.Udp,
            eventClass: RuntimePacketEventClass.Packet,
            ownerExtension: parsedExtensionName.value,
            resourceId: parsedResourceId.value,
            ...(visibility.value.tag === ExtensionStreamVisibilityTag.Shared
              ? { sharedTag: visibility.value.sharedTag }
              : {}),
          },
        ],
      });

      assert.deepEqual(manifest.protocolVersion, { major: 1, minor: 0 });
      assert.equal(manifest.sdkLanguage, SdkLanguage.TypeScript);
      assert.equal(manifest.workerKind, ForeignWorkerKind.RuntimeExtension);
      assert.equal(manifest.manifest.capabilities.length, 2);
      assert.equal(manifest.manifest.resources.length, 1);
      assert.equal(manifest.manifest.subscriptions.length, 1);
    }
  }
});

test("runtime extension manifest rejects invalid worker metadata", () => {
  const manifest = tryCreateRuntimeExtensionWorkerManifest({
    sdkVersion: "   ",
    extensionName: "",
  });

  assert.equal(isErr(manifest), true);
  if (isErr(manifest)) {
    assert.equal(manifest.error.kind, RuntimeExtensionErrorKind.ValidationError);
    assert.equal(manifest.error.field, "extensionName");
  }
});

test("packet subscription matching mirrors rust-side metadata filters", () => {
  const parsedExtensionName = extensionName("udp-demo");
  const parsedResourceId = extensionResourceId("udp-resource");
  const localAddress = socketAddress("127.0.0.1:21011");

  assert.equal(isOk(parsedExtensionName), true);
  assert.equal(isOk(parsedResourceId), true);
  assert.equal(isOk(localAddress), true);

  if (isOk(parsedExtensionName) && isOk(parsedResourceId) && isOk(localAddress)) {
    const event = createRuntimePacketEvent(
      {
        kind: RuntimePacketSourceKind.ExtensionResource,
        transport: RuntimePacketTransport.Udp,
        eventClass: RuntimePacketEventClass.Packet,
        ownerExtension: parsedExtensionName.value,
        resourceId: parsedResourceId.value,
        localAddress: localAddress.value,
      },
      [1, 2, 3],
      42,
    );

    assert.equal(
      packetSubscriptionMatches(
        {
          sourceKind: RuntimePacketSourceKind.ExtensionResource,
          transport: RuntimePacketTransport.Udp,
          ownerExtension: parsedExtensionName.value,
          resourceId: parsedResourceId.value,
          localPort: 21011,
        },
        event,
      ),
      true,
    );
    assert.equal(
      packetSubscriptionMatches(
        {
          sourceKind: RuntimePacketSourceKind.ObserverIngress,
        },
        event,
      ),
      false,
    );
  }
});

test("runtime extension worker runtime handles manifest lifecycle and exceptions", async () => {
  const parsedExtensionName = extensionName("worker-demo");
  assert.equal(isOk(parsedExtensionName), true);

  if (!isOk(parsedExtensionName)) {
    return;
  }

  const runtime = tryCreateRuntimeExtensionWorkerRuntime(
    defineRuntimeExtension({
      manifest: createRuntimeExtensionWorkerManifest({
        sdkVersion: "0.1.0",
        extensionName: parsedExtensionName.value,
        capabilities: [ExtensionCapability.ObserveObserverIngress],
      }),
      onReady: () => ok(runtimeExtensionAck()),
      onPacketReceived: () => {
        throw new Error("boom");
      },
      onShutdown: () => ok(runtimeExtensionAck()),
    }),
  );

  assert.equal(isOk(runtime), true);
  if (!isOk(runtime)) {
    return;
  }

  const manifest = await runtime.value.handleMessage({
    tag: RuntimeExtensionWorkerHostMessageTag.GetManifest,
  });
  const started = await runtime.value.handleMessage({
    tag: RuntimeExtensionWorkerHostMessageTag.Start,
    context: { extensionName: parsedExtensionName.value },
  });
  const delivered = await runtime.value.handleMessage({
    tag: RuntimeExtensionWorkerHostMessageTag.DeliverPacket,
    event: createRuntimePacketEvent(
      {
        kind: RuntimePacketSourceKind.ObserverIngress,
        transport: RuntimePacketTransport.Udp,
        eventClass: RuntimePacketEventClass.Packet,
      },
      [1],
      1,
    ),
  });
  const shutdown = await runtime.value.handleMessage({
    tag: RuntimeExtensionWorkerHostMessageTag.Shutdown,
    context: { extensionName: parsedExtensionName.value },
  });

  assert.equal(manifest.tag, RuntimeExtensionWorkerResponseTag.Manifest);
  assert.equal(started.tag, RuntimeExtensionWorkerResponseTag.Started);
  assert.equal(delivered.tag, RuntimeExtensionWorkerResponseTag.EventHandled);
  assert.equal(shutdown.tag, RuntimeExtensionWorkerResponseTag.ShutdownComplete);
  assert.equal(isOk(started.result), true);
  assert.equal(isErr(delivered.result), true);
  assert.equal(isOk(shutdown.result), true);

  if (isErr(delivered.result)) {
    assert.equal(delivered.result.error.kind, RuntimeExtensionErrorKind.UnhandledException);
    assert.equal(delivered.result.error.field, "onPacketReceived");
  }
});
