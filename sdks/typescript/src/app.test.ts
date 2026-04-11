import assert from "node:assert/strict";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
  ok,
  runtimeExtensionAck,
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
    plugins: [new Plugin({ name: "tx-logger", onProviderEvent: () => ok(runtimeExtensionAck()) })],
  });

  assert.equal(app.name, "tx-logger-app");
  assert.equal(app.runtime.runtimeDeliveryProfile, 2);
  assert.deepEqual(app.ingress, [
    {
      kind: IngressKind.WebSocket,
      name: "solana-websocket",
      url: "wss://example.invalid",
      stream: 1,
      readiness: 1,
      role: 1,
      accountInclude: [],
      accountExclude: [],
      accountRequired: [],
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

  assert.deepEqual(plugin.manifest.capabilities, [ExtensionCapability.ObserveObserverIngress]);
  assert.deepEqual(plugin.manifest.subscriptions, [
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

  assert.deepEqual(plugin.manifest.capabilities, [ExtensionCapability.ConnectWebSocket]);
  assert.deepEqual(plugin.manifest.subscriptions, []);
});

test("plugin create preserves the validated auto-generated name", () => {
  const plugin = Plugin.create({
    onPacket: () => ok(runtimeExtensionAck()),
  });

  assert.equal(isErr(plugin), false);
  if (!isErr(plugin)) {
    assert.match(plugin.value.name, /^plugin-\d+$/);
    assert.equal(plugin.value.name, "plugin-1");
  }
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

test("app resolves the runtime host from the installed native package", async () => {
  if (process.platform === "win32") {
    return;
  }

  const nativePackageDirectory = join(
    process.cwd(),
    "native",
    `${process.platform}-${process.arch}`,
  );
  const vendorDirectory = join(nativePackageDirectory, "vendor");
  const binaryPath = join(vendorDirectory, "sof_ts_runtime_host");
  const previousRuntimeHost = process.env.SOF_SDK_RUNTIME_HOST_BINARY;
  delete process.env.SOF_SDK_RUNTIME_HOST_BINARY;

  await mkdir(vendorDirectory, { recursive: true });
  await writeFile(binaryPath, "#!/bin/sh\nexit 0\n", "utf8");
  await chmod(binaryPath, 0o755);

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
    await rm(vendorDirectory, { force: true, recursive: true });
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

test("app rejects unsupported ingress kinds from plain JavaScript input", () => {
  const invalidInit: unknown = JSON.parse(`{
    "ingress": [
      {
        "kind": 999,
        "url": "wss://example.invalid"
      }
    ]
  }`);
  assert.notEqual(invalidInit, null);
  assert.equal(typeof invalidInit, "object");
  if (invalidInit === null || typeof invalidInit !== "object") {
    return;
  }

  const result = App.create({
    ...invalidInit,
    plugins: [new Plugin({ name: "packet-extension", logPackets: false })],
  });

  assert.equal(isErr(result), true);
  if (isErr(result)) {
    assert.equal(result.error.field, "ingress.kind");
  }
});

test("app runtime host config does not serialize the full process environment", async () => {
  const previousRuntimeHost = process.env.SOF_SDK_RUNTIME_HOST_BINARY;
  const previousSnapshot = process.env.SOF_SDK_CONFIG_SNAPSHOT;
  const previousSecret = process.env.SOF_SDK_SHOULD_NOT_BE_SERIALIZED;
  const tempDir = await mkdtemp(join(tmpdir(), "sof-sdk-host-test-"));
  const hostPath = join(tempDir, "host.mjs");
  const snapshotPath = join(tempDir, "snapshot.json");
  await writeFile(
    hostPath,
    `#!/usr/bin/env node
import { readFileSync, writeFileSync } from "node:fs";
const configPath = process.argv[2];
const snapshotPath = process.env.SOF_SDK_CONFIG_SNAPSHOT;
if (configPath === undefined || snapshotPath === undefined) process.exit(2);
const config = JSON.parse(readFileSync(configPath, "utf8"));
writeFileSync(snapshotPath, JSON.stringify(config.pluginWorkers[0].environment));
`,
    "utf8",
  );
  await chmod(hostPath, 0o755);
  process.env.SOF_SDK_RUNTIME_HOST_BINARY = hostPath;
  process.env.SOF_SDK_CONFIG_SNAPSHOT = snapshotPath;
  process.env.SOF_SDK_SHOULD_NOT_BE_SERIALIZED = "secret";
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
    const environment = JSON.parse(await readFile(snapshotPath, "utf8")) as Record<string, string>;
    assert.deepEqual(environment, {
      SOF_SDK_INTERNAL_PLUGIN_WORKER: "packet-extension",
      SOF_SDK_INTERNAL_PLUGIN_WORKER_MODE: "1",
    });
  } finally {
    if (previousRuntimeHost === undefined) {
      delete process.env.SOF_SDK_RUNTIME_HOST_BINARY;
    } else {
      process.env.SOF_SDK_RUNTIME_HOST_BINARY = previousRuntimeHost;
    }
    if (previousSnapshot === undefined) {
      delete process.env.SOF_SDK_CONFIG_SNAPSHOT;
    } else {
      process.env.SOF_SDK_CONFIG_SNAPSHOT = previousSnapshot;
    }
    if (previousSecret === undefined) {
      delete process.env.SOF_SDK_SHOULD_NOT_BE_SERIALIZED;
    } else {
      process.env.SOF_SDK_SHOULD_NOT_BE_SERIALIZED = previousSecret;
    }
    await rm(tempDir, { force: true, recursive: true });
  }
});

test("app runtime host config marks provider-event workers explicitly", async () => {
  const previousRuntimeHost = process.env.SOF_SDK_RUNTIME_HOST_BINARY;
  const previousSnapshot = process.env.SOF_SDK_CONFIG_SNAPSHOT;
  const tempDir = await mkdtemp(join(tmpdir(), "sof-sdk-host-test-"));
  const hostPath = join(tempDir, "host.mjs");
  const snapshotPath = join(tempDir, "snapshot.json");
  await writeFile(
    hostPath,
    `#!/usr/bin/env node
import { readFileSync, writeFileSync } from "node:fs";
const configPath = process.argv[2];
const snapshotPath = process.env.SOF_SDK_CONFIG_SNAPSHOT;
if (configPath === undefined || snapshotPath === undefined) process.exit(2);
const config = JSON.parse(readFileSync(configPath, "utf8"));
writeFileSync(snapshotPath, JSON.stringify(config.pluginWorkers.map((worker) => ({
  name: worker.name,
  providerEvents: worker.providerEvents,
}))));
`,
    "utf8",
  );
  await chmod(hostPath, 0o755);
  process.env.SOF_SDK_RUNTIME_HOST_BINARY = hostPath;
  process.env.SOF_SDK_CONFIG_SNAPSHOT = snapshotPath;
  try {
    const result = await new App({
      ingress: [
        {
          kind: IngressKind.Grpc,
          endpoint: "https://example.invalid",
        },
      ],
      plugins: [
        new Plugin({ name: "packet-extension", logPackets: false }),
        new Plugin({
          name: "provider-extension",
          onProviderEvent: () => ok(runtimeExtensionAck()),
        }),
      ],
    }).run();

    assert.equal(isOk(result), true);
    const workers = JSON.parse(await readFile(snapshotPath, "utf8")) as Array<{
      name: string;
      providerEvents: boolean;
    }>;
    assert.deepEqual(workers, [
      { name: "packet-extension", providerEvents: false },
      { name: "provider-extension", providerEvents: true },
    ]);
  } finally {
    if (previousRuntimeHost === undefined) {
      delete process.env.SOF_SDK_RUNTIME_HOST_BINARY;
    } else {
      process.env.SOF_SDK_RUNTIME_HOST_BINARY = previousRuntimeHost;
    }
    if (previousSnapshot === undefined) {
      delete process.env.SOF_SDK_CONFIG_SNAPSHOT;
    } else {
      process.env.SOF_SDK_CONFIG_SNAPSHOT = previousSnapshot;
    }
    await rm(tempDir, { force: true, recursive: true });
  }
});

test("app ignores internal worker env without the internal worker mode flag", async () => {
  const previousWorker = process.env.SOF_SDK_INTERNAL_PLUGIN_WORKER;
  const previousMode = process.env.SOF_SDK_INTERNAL_PLUGIN_WORKER_MODE;
  const previousRuntimeHost = process.env.SOF_SDK_RUNTIME_HOST_BINARY;
  process.env.SOF_SDK_INTERNAL_PLUGIN_WORKER = "packet-extension";
  delete process.env.SOF_SDK_INTERNAL_PLUGIN_WORKER_MODE;
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
    if (previousWorker === undefined) {
      delete process.env.SOF_SDK_INTERNAL_PLUGIN_WORKER;
    } else {
      process.env.SOF_SDK_INTERNAL_PLUGIN_WORKER = previousWorker;
    }
    if (previousMode === undefined) {
      delete process.env.SOF_SDK_INTERNAL_PLUGIN_WORKER_MODE;
    } else {
      process.env.SOF_SDK_INTERNAL_PLUGIN_WORKER_MODE = previousMode;
    }
    if (previousRuntimeHost === undefined) {
      delete process.env.SOF_SDK_RUNTIME_HOST_BINARY;
    } else {
      process.env.SOF_SDK_RUNTIME_HOST_BINARY = previousRuntimeHost;
    }
  }
});

test("app delegates mixed websocket and native ingress to the runtime host", async () => {
  const previousRuntimeHost = process.env.SOF_SDK_RUNTIME_HOST_BINARY;
  process.env.SOF_SDK_RUNTIME_HOST_BINARY = "/bin/true";
  try {
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

    assert.equal(isOk(result), true);
  } finally {
    if (previousRuntimeHost === undefined) {
      delete process.env.SOF_SDK_RUNTIME_HOST_BINARY;
    } else {
      process.env.SOF_SDK_RUNTIME_HOST_BINARY = previousRuntimeHost;
    }
  }
});

test("app accepts multi-websocket ingress with explicit fanIn", async () => {
  const previousRuntimeHost = process.env.SOF_SDK_RUNTIME_HOST_BINARY;
  process.env.SOF_SDK_RUNTIME_HOST_BINARY = "/bin/true";
  try {
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
        strategy: FanInStrategy.FirstSeenThenPromote,
      },
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

test("app accepts typed kernel-bypass config for direct shreds", async () => {
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

test("app delegates multiple direct shred sources to the runtime host", async () => {
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

    assert.equal(isOk(result), true);
  } finally {
    if (previousRuntimeHost === undefined) {
      delete process.env.SOF_SDK_RUNTIME_HOST_BINARY;
    } else {
      process.env.SOF_SDK_RUNTIME_HOST_BINARY = previousRuntimeHost;
    }
  }
});
