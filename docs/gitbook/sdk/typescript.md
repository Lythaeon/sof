# TypeScript SDK

The TypeScript SDK is now the normal way to author a SOF app in Node.js.

The intended user experience is:

1. install `@lythaeon-sof/sdk`
2. create an `App`
3. register one or more `Plugin`s
4. call `await app.run()`

The user should not need to think about the Rust runtime host underneath.

## Mental Model

Keep the shape small:

- `App` owns ingress, fan-in, runtime policy, and plugin registration
- `Plugin` owns your callbacks
- `app.run()` hands execution to the native SOF runtime host

That is deliberately close to the Rust framework shape. The SDK no longer asks users to assemble
scattered helper functions just to start a runtime.

## What Improved

Compared to the earlier TS wrapper state, the SDK is materially easier to reason about now:

- the public API is constructor-first with `new App({...})` and `new Plugin({...})`
- non-empty ingress delegates to the Rust runtime instead of recreating websocket loops in TS
- the root export surface is narrower and more app-facing
- worker/stdio internals are not public package exports
- native runtime packaging is part of the release model instead of an afterthought
- examples and tests cover real runtime handoff behavior

## Example

```ts
import { App, IngressKind, Plugin, ok, runtimeExtensionAck } from "@lythaeon-sof/sdk";

const app = new App({
  ingress: [
    {
      kind: IngressKind.WebSocket,
      url: "wss://example.invalid",
    },
  ],
  plugins: [
    new Plugin({
      name: "tx-logger",
      onProviderEvent: (event) => {
        process.stderr.write(`${JSON.stringify(event)}\n`);
        return ok(runtimeExtensionAck());
      },
    }),
  ],
});

await app.run();
```

## Supported Runtime Shapes

The current TypeScript SDK supports the same Rust-owned runtime composition model for:

- websocket provider ingress
- gRPC provider ingress
- multi-provider fan-in
- gossip ingress
- direct shred ingress
- Linux kernel-bypass options for direct shreds
- mixed raw ingress plus provider ingress through the native runtime host

## Safety Rules

Two rules matter in practice:

1. treat callback returns as explicit `Result` values
2. do not write to `stdout` inside worker callbacks

`stdout` is reserved for the worker protocol. The SDK now hard-blocks that misuse so plugin code
fails loudly instead of corrupting the bridge.

Use `stderr` for logging inside callbacks.

## Current Scope

The TypeScript SDK is production-oriented for SOF runtime authoring.

What is still separate:

- npm release rollout is new and still being exercised
- Python SDK work is not finished
- Rust contributor APIs remain documented in the maintainer and architecture sections, not here
