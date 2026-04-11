import { App, IngressKind, Plugin, ok, runtimeExtensionAck } from "../dist/index.js";

function main(): number {
  const plugin = new Plugin({
    name: "demo-plugin",
    onStart: () => ok(runtimeExtensionAck()),
    onProviderEvent: () => ok(runtimeExtensionAck()),
    onStop: () => ok(runtimeExtensionAck()),
  });

  const app = new App({
    ingress: [
      {
        kind: IngressKind.WebSocket,
        name: "solana-websocket",
        url: "wss://example.invalid",
      },
    ],
    plugins: [plugin],
  });

  process.stdout.write(
    `${JSON.stringify(
      {
        appName: app.name,
        pluginNames: app.plugins.map((value) => value.name),
        ingress: app.ingress,
        runtimeEnvironment: app.runtime.toEnvironmentRecord(),
      },
      undefined,
      2,
    )}\n`,
  );

  return 0;
}

process.exitCode = main();
