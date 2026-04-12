# `@lythaeon-sof/sdk`

TypeScript SDK for building apps with a typed `App`, `Plugin`, `runtime`, and `derivedState` model.

## Tooling

- Use `pnpm` for this package.
- `pnpm run build` produces minified ESM output plus `.d.ts` files.
- `pnpm run build:native` builds the current platform native runtime host package in the workspace.
- `pnpm run format:check` verifies Biome formatting.
- `pnpm run lint` runs the type-aware `oxlint` profile.
- `pnpm run check` runs format, lint, typecheck, tests, examples, and package validation.
- SDK release tags and npm publish order are documented in `RELEASING.md`.

## Install

```sh
pnpm add @lythaeon-sof/sdk
```

For published releases, npm installs the matching optional native runtime package for the current platform automatically. App authors only import `@lythaeon-sof/sdk` and run Node.

## Mental Model

- Build one `App`.
- Put ingress on the app with `ingress`.
- Put merge behavior on the app with `fanIn` when you have multiple sources.
- Put runtime behavior on the app with `runtime`, including `derivedState`.
- Put app logic in `Plugin`.
- Start the app with `app.run()`.

`name` is optional on `App`. If you omit it, the SDK derives one from the first plugin.

## Quick Start

```ts
import { App, IngressKind, Plugin, ok, runtimeExtensionAck } from "@lythaeon-sof/sdk";

const app = new App({
  ingress: [
    {
      kind: IngressKind.WebSocket,
      name: "solana-websocket",
      url: "wss://example.invalid",
    },
  ],
  plugins: [
    new Plugin({
      name: "tx-logger",
      onProviderEvent: (event) => {
        console.error(event);
        return ok(runtimeExtensionAck());
      },
    }),
  ],
});

await app.run();
```

`app.run()` is the normal execution path. It runs until the app is stopped.

Current executable coverage:
- `app.run()` delegates every non-empty ingress config to the packaged native runtime host.
- WebSocket ingress uses SOF's native provider-stream websocket adapter and delivers events to `Plugin.onProviderEvent`.
- `DirectShreds` runs through the packaged native runtime host for one raw packet source per app.
- `Grpc` runs through the packaged native runtime host with Yellowstone provider-stream events delivered to `Plugin.onProviderEvent`.
- `Gossip` runs through the packaged native runtime host with gossip-bootstrap support.
- Direct shreds can enable kernel bypass on Linux through a typed `kernelBypass` object.
- One `DirectShreds` ingress plus one `Gossip` ingress run together as one raw runtime composition without `fanIn`.
- Multi-provider fan-in uses the Rust arbitration model: `EmitAll`, `FirstSeen`, or `FirstSeenThenPromote`.
- Published installs resolve the native host from the matching optional platform package such as `@lythaeon-sof/sdk-native-linux-x64`.
- `SOF_SDK_RUNTIME_HOST_BINARY` is only an override for development or custom deployments.
- If the host is missing, `app.run()` returns a typed `Result` error instead of throwing.

## Runtime

Use the runtime object when you want to control delivery profile, provider policy, shred trust mode, or derived state.

```ts
import {
  App,
  DerivedStateReplayBackend,
  DerivedStateReplayDurability,
  IngressKind,
  Plugin,
  createBalancedRuntime,
} from "@lythaeon-sof/sdk";

const app = new App({
  runtime: createBalancedRuntime({
    derivedState: {
      replay: {
        backend: DerivedStateReplayBackend.Disk,
        durability: DerivedStateReplayDurability.Fsync,
        maxEnvelopes: 4096,
        maxSessions: 4,
      },
    },
  }),
  ingress: [
    {
      kind: IngressKind.WebSocket,
      url: "wss://example.invalid",
    },
  ],
  plugins: [new Plugin({ logPackets: true })],
});

app;
```

## gRPC Provider Ingress

gRPC provider ingress delivers provider-stream events, not raw packets.

```ts
import {
  App,
  GrpcIngressStream,
  IngressKind,
  Plugin,
  ProviderCommitment,
  RuntimeProviderEventKind,
  ok,
  runtimeExtensionAck,
} from "@lythaeon-sof/sdk";

const app = new App({
  ingress: [
    {
      kind: IngressKind.Grpc,
      name: "yellowstone",
      endpoint: "https://example.invalid",
      stream: GrpcIngressStream.TransactionStatus,
      commitment: ProviderCommitment.Processed,
    },
  ],
  plugins: [
    new Plugin({
      name: "provider-logger",
      onProviderEvent: (event) => {
        if (event.kind === RuntimeProviderEventKind.TransactionStatus) {
          process.stderr.write(`${event.slot} ${event.signature}\n`);
        }
        return ok(runtimeExtensionAck());
      },
    }),
  ],
});

await app.run();
```

## Multi-Source Ingress

When you configure more than one provider ingress source, `fanIn` is required. The SDK maps it to the Rust provider arbitration model. If you want promotion behavior, set source roles or priorities on each ingress.

```ts
import {
  App,
  FanInStrategy,
  IngressKind,
  Plugin,
  ProviderIngressRole,
} from "@lythaeon-sof/sdk";

const app = new App({
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
  plugins: [new Plugin({ name: "provider-logger" })],
});

app;
```

## Direct Shreds

Direct shred ingress belongs on the app, not on a plugin.

```ts
import {
  App,
  IngressKind,
  Plugin,
  ShredTrustMode,
} from "@lythaeon-sof/sdk";

const app = new App({
  ingress: [
    {
      kind: IngressKind.DirectShreds,
      name: "trusted-shreds",
      bindAddress: "0.0.0.0:20000",
      trustMode: ShredTrustMode.TrustedRawShredProvider,
      kernelBypass: {
        interface: "eth0",
      },
    },
  ],
  plugins: [new Plugin({ name: "observer" })],
});

app;
```

## Plugins

`Plugin` is the app extension surface. A plugin can implement lifecycle hooks and packet handlers:

```ts
import {
  App,
  IngressKind,
  Plugin,
  ok,
  runtimeExtensionAck,
} from "@lythaeon-sof/sdk";

const plugin = new Plugin({
  name: "packet-audit",
  onStart: () => ok(runtimeExtensionAck()),
  onProviderEvent: (event) => {
    process.stderr.write(`${event.kind}\n`);
    return ok(runtimeExtensionAck());
  },
  onStop: () => ok(runtimeExtensionAck()),
});

const app = new App({
  ingress: [
    {
      kind: IngressKind.WebSocket,
      url: "wss://example.invalid",
    },
  ],
  plugins: [plugin],
});

app;
```

`Extension` is also exported as an alias of `Plugin` if you prefer that name.

## Derived State

Use `runtime.derivedState` when the app needs replay/backfill/checkpoint behavior:

```ts
import {
  App,
  DerivedStateReplayBackend,
  DerivedStateReplayDurability,
  IngressKind,
  Plugin,
  createBalancedRuntime,
} from "@lythaeon-sof/sdk";

const app = new App({
  runtime: createBalancedRuntime({
    derivedState: {
      replay: {
        backend: DerivedStateReplayBackend.Disk,
        durability: DerivedStateReplayDurability.Fsync,
        directory: ".sof-derived-state",
      },
    },
  }),
  ingress: [{ kind: IngressKind.WebSocket, url: "wss://example.invalid" }],
  plugins: [new Plugin({ name: "stateful-app" })],
});

app;
```

## Focused Imports

```ts
import { App, IngressKind, Plugin } from "@lythaeon-sof/sdk/app";
import { ObserverRuntimeConfig } from "@lythaeon-sof/sdk/runtime/config";
import { ShredTrustMode } from "@lythaeon-sof/sdk/runtime/policy";
import {
  DerivedStateReplayBackend,
  DerivedStateReplayDurability,
} from "@lythaeon-sof/sdk/runtime/derived-state";
import { RuntimeDeliveryProfile } from "@lythaeon-sof/sdk/runtime/delivery-profile";

const runtime = ObserverRuntimeConfig.forProfile(RuntimeDeliveryProfile.Balanced, {
  shredTrustMode: ShredTrustMode.TrustedRawShredProvider,
  derivedState: {
    replay: {
      backend: DerivedStateReplayBackend.Disk,
      durability: DerivedStateReplayDurability.Fsync,
    },
  },
});

new App({
  runtime,
  ingress: [{ kind: IngressKind.WebSocket, url: "wss://example.invalid" }],
  plugins: [new Plugin({ name: "tx-logger" })],
});
```

## Examples

Runnable examples live in `sdks/typescript/examples`:

- `app-config.ts`
- `app-entrypoint.ts`
- `runtime-config-balanced.ts`
- `runtime-config-parse.ts`
- `runtime-extension-manifest.ts`

Run them with:

```bash
pnpm run check:examples
```
