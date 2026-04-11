import {
  isErr,
  parseRuntimeConfig,
  runtimeDeliveryProfileEnvVarName,
  runtimeDeliveryProfileEnvValues,
} from "../dist/index.js";

const parsed = parseRuntimeConfig({
  [runtimeDeliveryProfileEnvVarName]: runtimeDeliveryProfileEnvValues.balanced,
  SOF_PROVIDER_STREAM_ALLOW_EOF: "true",
  SOF_DERIVED_STATE_REPLAY_MAX_ENVELOPES: "2048",
});

if (isErr(parsed)) {
  process.stderr.write(`${parsed.error.message}\n`);
  process.exit(1);
}

process.stdout.write(
  `${JSON.stringify(parsed.value.toEnvironmentRecord(), undefined, 2)}\n`,
);
