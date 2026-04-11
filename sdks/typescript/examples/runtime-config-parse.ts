import {
  type Result,
  isErr,
  parseRuntimeConfig,
  runtimeDeliveryProfileEnvVarName,
  runtimeDeliveryProfileEnvValues,
} from "../dist/index.js";

function expectOk<Value, Error extends { readonly message: string }>(
  result: Result<Value, Error>,
): Value {
  if (isErr(result)) {
    throw new Error(result.error.message);
  }

  return result.value;
}

const parsed = expectOk(
  parseRuntimeConfig({
    [runtimeDeliveryProfileEnvVarName]: runtimeDeliveryProfileEnvValues.balanced,
    SOF_PROVIDER_STREAM_ALLOW_EOF: "true",
    SOF_DERIVED_STATE_REPLAY_MAX_ENVELOPES: "2048",
  }),
);

process.stdout.write(`${JSON.stringify(parsed.toEnvironmentRecord(), undefined, 2)}\n`);
