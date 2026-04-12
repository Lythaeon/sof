import {
  DerivedStateReplayBackend,
  DerivedStateReplayDurability,
  ProviderStreamCapabilityPolicy,
  RuntimeDeliveryProfile,
  ShredTrustMode,
  createRuntimeConfigForProfile,
  serializeRuntimeConfigRecord,
} from "../dist/index.js";

const config = createRuntimeConfigForProfile(RuntimeDeliveryProfile.Balanced, {
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

const serialized = serializeRuntimeConfigRecord(config);

process.stdout.write(`${JSON.stringify(serialized, undefined, 2)}\n`);
