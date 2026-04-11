# Runtime Host and Packaging

`@sof/sdk` is not a pure JavaScript runtime.

It is a thin TypeScript application layer over the SOF Rust runtime host.

## How It Runs

When a user calls `app.run()`:

1. the SDK validates and normalizes the typed app config
2. the SDK writes a temporary runtime-host config file
3. the SDK resolves the native `sof_ts_runtime_host` binary
4. the Rust host starts and owns ingress, queues, replay, and runtime orchestration
5. the Rust host launches the Node worker process for TypeScript plugin callbacks

This keeps the hot path in Rust while making application code simpler.

## npm Packaging Model

The publish model is:

- `@sof/sdk` for the TypeScript API surface
- one native package per platform, for example `@sof/sdk-native-linux-x64`

The main package declares those native packages as optional dependencies.

That means a normal app author should only do:

```sh
pnpm add @sof/sdk
```

and then run their Node app.

## Why This Is Better Than Build-On-Install

The SDK does not require every user to compile Rust during `npm install`.

That matters because install-time native builds are slower and much more fragile in:

- CI
- containers
- locked-down corp environments
- systems where install scripts are disabled

Prebuilt native packages keep the TypeScript experience closer to a normal npm package.

## Local Overrides

There is one deliberate escape hatch:

- `SOF_SDK_RUNTIME_HOST_BINARY`

Use it only when you need to point the SDK at a custom host binary during development or custom
deployment.

It is not the normal application path.
