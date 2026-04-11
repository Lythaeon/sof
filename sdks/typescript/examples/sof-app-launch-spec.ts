import {
  ExtensionCapability,
  RuntimeDeliveryProfile,
  tryCreateAppLaunch,
  createRuntimeConfigForProfile,
  tryDefineApp,
  tryDefinePlugin,
  isErr,
} from "../dist/index.js";

const plugin = tryDefinePlugin({
  name: "app-launch-extension",
  capabilities: [ExtensionCapability.ObserveObserverIngress],
});

if (isErr(plugin)) {
  process.stderr.write(`${plugin.error.message}\n`);
  process.exitCode = 1;
} else {
  const app = tryDefineApp({
    name: "demo-sof-app",
    runtime: createRuntimeConfigForProfile(RuntimeDeliveryProfile.Balanced),
    plugins: [plugin.value],
  });
  if (isErr(app)) {
    process.stderr.write(`${app.error.message}\n`);
    process.exitCode = 1;
  } else {
    const launchSpec = tryCreateAppLaunch(app.value, {
      workerEntrypoint: "./dist/worker.js",
    });

    if (isErr(launchSpec)) {
      process.stderr.write(`${launchSpec.error.message}\n`);
      process.exitCode = 1;
    } else {
      process.stdout.write(`${JSON.stringify(launchSpec.value, undefined, 2)}\n`);
    }
  }
}
