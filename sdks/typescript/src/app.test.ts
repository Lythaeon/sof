import assert from "node:assert/strict";
import test from "node:test";

import { isErr, isOk } from "./result.js";
import {
  App,
  ExtensionCapability,
  FanInStrategy,
  GrpcIngressStream,
  IngressKind,
  Plugin,
  ProviderCommitment,
  ProviderIngressRole,
  RuntimePacketSourceKind,
  createBalancedRuntime,
} from "./index.js";

test("app derives a stable default name from the first plugin", () => {
  const app = new App({
    runtime: createBalancedRuntime(),
    ingress: [
      {
        kind: IngressKind.WebSocket,
        name: "solana-websocket",
        url: "wss://example.invalid",
      },
    ],
    plugins: [new Plugin({ name: "tx-logger", logPackets: true })],
  });

  assert.equal(app.name, "tx-logger-app");
  assert.equal(app.runtime.runtimeDeliveryProfile, 2);
  assert.deepEqual(app.ingress, [
    {
      kind: IngressKind.WebSocket,
      name: "solana-websocket",
      url: "wss://example.invalid",
      requests: [],
    },
  ]);
});

test("app rejects duplicate plugin names", () => {
  const duplicatePlugin = new Plugin({
    name: "duplicate-extension",
  });

  assert.throws(
    () =>
      new App({
        name: "duplicate-app",
        plugins: [duplicatePlugin, duplicatePlugin],
      }),
    /registered more than once/,
  );
});

test("app resolves a plugin by name", () => {
  const plugin = new Plugin({
    name: "selected-extension",
  });

  const app = new App({
    plugins: [plugin],
  });

  const resolved = app.getPlugin("selected-extension");
  assert.equal(isErr(resolved), false);
  if (!isErr(resolved)) {
    assert.equal(resolved.value.name, "selected-extension");
  }
});

test("app reports missing plugin names with available plugins", () => {
  const plugin = new Plugin({
    name: "available-extension",
  });

  const app = new App({
    name: "selection-app",
    plugins: [plugin],
  });

  const result = app.getPlugin("missing-extension");

  assert.equal(isErr(result), true);
  if (isErr(result)) {
    assert.equal(result.error.field, "pluginName");
    assert.deepEqual(result.error.availablePluginNames, ["available-extension"]);
  }
});

test("plugin packet handlers default to observer ingress manifest access", () => {
  const plugin = new Plugin({
    name: "packet-extension",
    onPacket: () => {
      throw new Error("not invoked by manifest construction");
    },
  });

  assert.deepEqual(plugin.manifest.manifest.capabilities, [
    ExtensionCapability.ObserveObserverIngress,
  ]);
  assert.deepEqual(plugin.manifest.manifest.subscriptions, [
    {
      sourceKind: RuntimePacketSourceKind.ObserverIngress,
    },
  ]);
});

test("plugin explicit manifest access is preserved", () => {
  const plugin = new Plugin({
    name: "explicit-extension",
    capabilities: [ExtensionCapability.ConnectWebSocket],
    subscriptions: [],
    onPacket: () => {
      throw new Error("not invoked by manifest construction");
    },
  });

  assert.deepEqual(plugin.manifest.manifest.capabilities, [ExtensionCapability.ConnectWebSocket]);
  assert.deepEqual(plugin.manifest.manifest.subscriptions, []);
});

test("app requires fanIn when multiple ingress sources are configured", () => {
  assert.throws(
    () =>
      new App({
        ingress: [
          {
            kind: IngressKind.WebSocket,
            url: "wss://one.example.invalid",
          },
          {
            kind: IngressKind.Grpc,
            endpoint: "https://two.example.invalid",
          },
        ],
      }),
    /fanIn is required/,
  );
});

test("app accepts multiple ingress sources when fanIn is explicit", () => {
  const app = new App({
    ingress: [
      {
        kind: IngressKind.WebSocket,
        name: "ws",
        url: "wss://one.example.invalid",
      },
      {
        kind: IngressKind.Grpc,
        name: "grpc",
        endpoint: "https://two.example.invalid",
      },
    ],
    fanIn: {
      strategy: FanInStrategy.FirstSeen,
    },
    plugins: [new Plugin({})],
  });

  assert.deepEqual(app.fanIn, {
    strategy: FanInStrategy.FirstSeen,
  });
});

test("app reports invalid runtime host override for non-websocket ingress", async () => {
  const previousRuntimeHost = process.env.SOF_SDK_RUNTIME_HOST_BINARY;
  process.env.SOF_SDK_RUNTIME_HOST_BINARY = "/definitely/missing/sof_ts_runtime_host";
  try {
    const result = await new App({
      ingress: [
        {
          kind: IngressKind.DirectShreds,
          bindAddress: "127.0.0.1:20000",
        },
      ],
      plugins: [new Plugin({ name: "packet-extension", logPackets: false })],
    }).run();

    assert.equal(isErr(result), true);
    if (isErr(result)) {
      assert.equal(result.error.field, "SOF_SDK_RUNTIME_HOST_BINARY");
      assert.match(result.error.message, /failed to start runtime host/);
    }
  } finally {
    if (previousRuntimeHost === undefined) {
      delete process.env.SOF_SDK_RUNTIME_HOST_BINARY;
    } else {
      process.env.SOF_SDK_RUNTIME_HOST_BINARY = previousRuntimeHost;
    }
  }
});

test("app delegates non-websocket ingress to configured runtime host", async () => {
  const previousRuntimeHost = process.env.SOF_SDK_RUNTIME_HOST_BINARY;
  process.env.SOF_SDK_RUNTIME_HOST_BINARY = "/bin/true";
  try {
    const result = await new App({
      ingress: [
        {
          kind: IngressKind.DirectShreds,
          bindAddress: "127.0.0.1:20000",
        },
      ],
      plugins: [new Plugin({ name: "packet-extension", logPackets: false })],
    }).run();

    assert.equal(isOk(result), true);
  } finally {
    if (previousRuntimeHost === undefined) {
      delete process.env.SOF_SDK_RUNTIME_HOST_BINARY;
    } else {
      process.env.SOF_SDK_RUNTIME_HOST_BINARY = previousRuntimeHost;
    }
  }
});

test("app delegates gRPC ingress to configured runtime host", async () => {
  const previousRuntimeHost = process.env.SOF_SDK_RUNTIME_HOST_BINARY;
  process.env.SOF_SDK_RUNTIME_HOST_BINARY = "/bin/true";
  try {
    const result = await new App({
      ingress: [
        {
          kind: IngressKind.Grpc,
          endpoint: "https://example.invalid",
          stream: GrpcIngressStream.TransactionStatus,
          commitment: ProviderCommitment.Processed,
        },
      ],
      plugins: [
        new Plugin({
          name: "provider-extension",
          onProviderEvent: () => {
            throw new Error("not invoked by host delegation smoke");
          },
        }),
      ],
    }).run();

    assert.equal(isOk(result), true);
  } finally {
    if (previousRuntimeHost === undefined) {
      delete process.env.SOF_SDK_RUNTIME_HOST_BINARY;
    } else {
      process.env.SOF_SDK_RUNTIME_HOST_BINARY = previousRuntimeHost;
    }
  }
});

test("app supports promote-capable native provider fan-in", async () => {
  const previousRuntimeHost = process.env.SOF_SDK_RUNTIME_HOST_BINARY;
  process.env.SOF_SDK_RUNTIME_HOST_BINARY = "/bin/true";
  try {
    const result = await new App({
      ingress: [
        {
          kind: IngressKind.Grpc,
          name: "grpc-a",
          endpoint: "https://one.example.invalid",
          role: ProviderIngressRole.Primary,
        },
        {
          kind: IngressKind.Grpc,
          name: "grpc-b",
          endpoint: "https://two.example.invalid",
          role: ProviderIngressRole.Fallback,
        },
      ],
      fanIn: {
        strategy: FanInStrategy.FirstSeenThenPromote,
      },
      plugins: [new Plugin({ name: "provider-extension" })],
    }).run();

    assert.equal(isOk(result), true);
  } finally {
    if (previousRuntimeHost === undefined) {
      delete process.env.SOF_SDK_RUNTIME_HOST_BINARY;
    } else {
      process.env.SOF_SDK_RUNTIME_HOST_BINARY = previousRuntimeHost;
    }
  }
});

test("app rejects invalid provider ingress priority", () => {
  assert.throws(
    () =>
      new App({
        ingress: [
          {
            kind: IngressKind.Grpc,
            endpoint: "https://one.example.invalid",
            priority: 70_000,
          },
        ],
        plugins: [new Plugin({ name: "provider-extension" })],
      }),
    /ingress.priority must be an integer between 0 and 65535/,
  );
});

test("app reports mixed websocket and native ingress before starting a host", async () => {
  const result = await new App({
    ingress: [
      {
        kind: IngressKind.WebSocket,
        url: "wss://example.invalid",
      },
      {
        kind: IngressKind.DirectShreds,
        bindAddress: "127.0.0.1:20000",
      },
    ],
    fanIn: {
      strategy: FanInStrategy.FirstSeen,
    },
    plugins: [new Plugin({ name: "packet-extension", logPackets: false })],
  }).run();

  assert.equal(isErr(result), true);
  if (isErr(result)) {
    assert.equal(result.error.field, "ingress");
    assert.match(result.error.message, /mixed websocket and native-host ingress/);
  }
});

test("app reports multi-websocket ingress fan-in as unsupported today", async () => {
  const result = await new App({
    ingress: [
      {
        kind: IngressKind.WebSocket,
        name: "ws-a",
        url: "wss://one.example.invalid",
      },
      {
        kind: IngressKind.WebSocket,
        name: "ws-b",
        url: "wss://two.example.invalid",
      },
    ],
    fanIn: {
      strategy: FanInStrategy.FirstSeen,
    },
    plugins: [new Plugin({ name: "packet-extension", logPackets: false })],
  }).run();

  assert.equal(isErr(result), true);
  if (isErr(result)) {
    assert.equal(result.error.field, "ingress");
    assert.match(result.error.message, /multi-websocket ingress fan-in/);
  }
});

test("app accepts typed kernel-bypass config for direct shreds", async () => {
  if (process.platform !== "linux") {
    const result = await new App({
      ingress: [
        {
          kind: IngressKind.DirectShreds,
          bindAddress: "127.0.0.1:20000",
          kernelBypass: {
            interface: "eth0",
          },
        },
      ],
      plugins: [new Plugin({ name: "packet-extension", logPackets: false })],
    }).run();

    assert.equal(isErr(result), true);
    if (isErr(result)) {
      assert.equal(result.error.field, "ingress.kernelBypass");
      assert.match(result.error.message, /only supported on Linux/);
    }
    return;
  }

  const previousRuntimeHost = process.env.SOF_SDK_RUNTIME_HOST_BINARY;
  process.env.SOF_SDK_RUNTIME_HOST_BINARY = "/bin/true";
  try {
    const result = await new App({
      ingress: [
        {
          kind: IngressKind.DirectShreds,
          bindAddress: "127.0.0.1:20000",
          kernelBypass: {
            interface: "eth0",
            queueId: 0,
          },
        },
      ],
      plugins: [new Plugin({ name: "packet-extension", logPackets: false })],
    }).run();

    assert.equal(isOk(result), true);
  } finally {
    if (previousRuntimeHost === undefined) {
      delete process.env.SOF_SDK_RUNTIME_HOST_BINARY;
    } else {
      process.env.SOF_SDK_RUNTIME_HOST_BINARY = previousRuntimeHost;
    }
  }
});

test("app composes direct shreds and gossip without fanIn", async () => {
  const previousRuntimeHost = process.env.SOF_SDK_RUNTIME_HOST_BINARY;
  process.env.SOF_SDK_RUNTIME_HOST_BINARY = "/bin/true";
  try {
    const result = await new App({
      ingress: [
        {
          kind: IngressKind.DirectShreds,
          name: "direct-a",
          bindAddress: "127.0.0.1:20000",
        },
        {
          kind: IngressKind.Gossip,
          name: "gossip-a",
          entrypoints: ["entrypoint.example.invalid:8001"],
        },
      ],
      plugins: [new Plugin({ name: "packet-extension", logPackets: false })],
    }).run();

    assert.equal(isOk(result), true);
  } finally {
    if (previousRuntimeHost === undefined) {
      delete process.env.SOF_SDK_RUNTIME_HOST_BINARY;
    } else {
      process.env.SOF_SDK_RUNTIME_HOST_BINARY = previousRuntimeHost;
    }
  }
});

test("app rejects fanIn for direct shreds plus gossip composition", () => {
  assert.throws(
    () =>
      new App({
        ingress: [
          {
            kind: IngressKind.DirectShreds,
            bindAddress: "127.0.0.1:20000",
          },
          {
            kind: IngressKind.Gossip,
            entrypoints: ["entrypoint.example.invalid:8001"],
          },
        ],
        fanIn: {
          strategy: FanInStrategy.FirstSeen,
        },
        plugins: [new Plugin({ name: "packet-extension", logPackets: false })],
      }),
    /fanIn is not used/,
  );
});

test("app reports multiple direct shred native ingress sources as unsupported today", async () => {
  const result = await new App({
    ingress: [
      {
        kind: IngressKind.DirectShreds,
        name: "direct-a",
        bindAddress: "127.0.0.1:20000",
      },
      {
        kind: IngressKind.DirectShreds,
        name: "direct-b",
        bindAddress: "127.0.0.1:20001",
      },
    ],
    fanIn: {
      strategy: FanInStrategy.FirstSeen,
    },
    plugins: [new Plugin({ name: "packet-extension", logPackets: false })],
  }).run();

  assert.equal(isErr(result), true);
  if (isErr(result)) {
    assert.equal(result.error.field, "ingress");
    assert.match(result.error.message, /one direct shred ingress source/);
  }
});
