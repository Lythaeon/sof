import {
  DerivedStateReplayBackend,
  DerivedStateReplayDurability,
  ProviderStreamCapabilityPolicy,
  RuntimeDeliveryProfile,
  ShredTrustMode,
  type Result,
  isErr,
  tryCreateRuntimeConfigForProfile,
  trySerializeRuntimeConfigRecord,
} from "../dist/index.js";

function expectOk<Value, Error extends { readonly message: string }>(
  result: Result<Value, Error>,
): Value {
  if (isErr(result)) {
    throw new Error(result.error.message);
  }

  return result.value;
}

const config = expectOk(
  tryCreateRuntimeConfigForProfile(RuntimeDeliveryProfile.Balanced, {
    shredTrustMode: ShredTrustMode.TrustedRawShredProvider,
    providerStreamCapabilityPolicy: ProviderStreamCapabilityPolicy.Strict,
    derivedState: {
      replay: {
        backend: DerivedStateReplayBackend.Disk,
        durability: DerivedStateReplayDurability.Fsync,
        maxEnvelopes: 1024,
        maxSessions: 2,
      },
    },
  }),
);

const serialized = expectOk(trySerializeRuntimeConfigRecord(config));

process.stdout.write(`${JSON.stringify(serialized, undefined, 2)}\n`);
