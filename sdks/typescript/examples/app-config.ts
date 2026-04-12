import {
  App,
  FanInStrategy,
  IngressKind,
  Plugin,
  ProviderIngressRole,
  createBalancedRuntime,
} from "../dist/index.js";

const app = new App({
  runtime: createBalancedRuntime(),
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
  plugins: [new Plugin({ name: "multi-ingress-demo" })],
});

process.stdout.write(
  `${JSON.stringify(
    {
      appName: app.name,
      ingress: app.ingress,
      fanIn: app.fanIn,
      runtimeEnvironment: app.runtime.toEnvironmentRecord(),
    },
    undefined,
    2,
  )}\n`,
);
