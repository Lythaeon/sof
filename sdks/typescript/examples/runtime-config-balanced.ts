import {
  DerivedStateReplayBackend,
  DerivedStateReplayDurability,
  ProviderStreamCapabilityPolicy,
  RuntimeDeliveryProfile,
  ShredTrustMode,
  isErr,
  tryCreateRuntimeConfigForProfile,
  trySerializeRuntimeConfigRecord,
} from "../dist/index.js";

const config = tryCreateRuntimeConfigForProfile(RuntimeDeliveryProfile.Balanced, {
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
});

if (isErr(config)) {
  process.stderr.write(`${config.error.message}\n`);
  process.exit(1);
}

const serialized = trySerializeRuntimeConfigRecord(config.value);
if (isErr(serialized)) {
  process.stderr.write(`${serialized.error.message}\n`);
  process.exit(1);
}

process.stdout.write(`${JSON.stringify(serialized.value, undefined, 2)}\n`);
