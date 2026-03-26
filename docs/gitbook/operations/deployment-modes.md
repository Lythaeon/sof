# Deployment Modes

SOF can be deployed in three main runtime modes, but there are also two different trust postures
inside those modes. Pick both deliberately.

One design goal stays the same across all of them:

SOF should let downstream teams reuse one runtime foundation rather than rebuilding provider
ingest, packet handling, reconstruction, performance tuning, and correctness boundaries per
application.

## Mode Comparison

| Mode | Use When | Main Tradeoff |
| --- | --- | --- |
| Direct UDP listener | you control packet sources or want the simplest bring-up | no gossip-discovered topology/bootstrap |
| Gossip bootstrap | you want cluster discovery, topology updates, relay, and bounded repair | more moving parts and more outbound control-plane traffic |
| External kernel-bypass ingress | you own a specialized network front end | higher integration complexity |

## Trust Posture

| Trust posture | Best fit | Main tradeoff |
| --- | --- | --- |
| `public_untrusted` | you want to own the whole stack and keep independent verification | higher CPU cost and usually later visibility than private shred distribution |
| `trusted_raw_shred_provider` | you have access to a trusted private shred feed and want SOF's fastest practical path | you are explicitly depending on upstream trust instead of only public-edge verification |

`processed_provider_stream` products such as Yellowstone gRPC, LaserStream, or websocket feeds are
useful, but they are a different category from raw-shred SOF ingest. They are not raw-shred trust
modes, so they are not values of `SOF_SHRED_TRUST_MODE`.

SOF exposes this category through `ProviderStreamMode`. In that mode:

- upstream provider data goes directly into transaction or transaction-view-batch dispatch
- SOF does not run packet parsing, shred verification, FEC recovery, or dataset reconstruction
- plugin and derived-state logic can still stay on the SOF runtime surface
- current adapters include Yellowstone gRPC, LaserStream gRPC, and websocket
  `transactionSubscribe`
- build flags are provider-specific:
  - `provider-grpc` for Yellowstone gRPC and LaserStream gRPC
  - `provider-websocket`
- provider adapter defaults are inclusive: vote and failed transactions stay in
  the stream unless you explicitly filter them out
- strict provider capability policy only rejects hooks that provider runtime can
  never emit; it does not reject generic provider updates such as recent
  blockhash, slot status, or cluster topology when your producer supplies them

That means deployment mode is not only about network topology. It is also about how much of the
low-level substrate SOF is owning for the application:

- ingest and reconstruction on raw shreds
- or provider-adapter/runtime behavior on processed feeds
- while keeping the same downstream runtime surface where semantics line up

Raw-shred trust posture can be set either by env:

```bash
SOF_SHRED_TRUST_MODE=public_untrusted
SOF_SHRED_TRUST_MODE=trusted_raw_shred_provider
```

or by the typed runtime API:

```rust
use sof::runtime::{RuntimeSetup, ShredTrustMode};

let setup = RuntimeSetup::new()
    .with_shred_trust_mode(ShredTrustMode::PublicUntrusted);
```

If `SOF_VERIFY_SHREDS` or `SOF_VERIFY_RECOVERED_SHREDS` is set explicitly, it overrides the trust
mode defaults.

Trusted raw shred ingress still follows the normal SOF downstream path after admission:

1. raw packet parse/classification
2. optional FEC recovery
3. dataset and transaction reconstruction
4. plugin and runtime-extension dispatch

The trust-mode change only changes the default verification posture. It does not skip the rest of
the observer pipeline.

## Direct UDP Listener

Best for:

- local development
- controlled feed sources
- deployments that do not need SOF to participate in gossip bootstrap

Important behavior:

- lowest setup complexity
- no live gossip runtime
- still provides the normal runtime pipeline once packets arrive
- can be paired with either:
  - public untrusted packet sources
  - or a trusted raw shred provider feeding SOF directly

## Gossip Bootstrap

Best for:

- public hosts
- market-data style deployments that need topology and leader context
- operators willing to accept active network participation

Important behavior:

- bootstrap discovers peers from entrypoints
- SOF can relay recent shreds and serve bounded repair responses
- local topology and leader information become richer and more timely
- this is the independent baseline mode, not usually the fastest possible shred source

Build flag:

```toml
sof = { version = "0.12.0", features = ["gossip-bootstrap"] }
```

## External Kernel-Bypass Ingress

Best for:

- AF_XDP or other custom high-performance receivers
- teams that want to own the NIC path but still reuse SOF downstream

Important behavior:

- SOF processes `RawPacketBatch` values from an external queue
- built-in UDP receive path is replaced by your external receiver
- downstream parse, verify, reassembly, and event surfaces stay the same
- this is the natural fit when a trusted shred-distribution network or custom NIC path feeds SOF

Example:

- [`trusted_raw_shred_provider.rs`](https://github.com/Lythaeon/sof/blob/main/crates/sof-observer/examples/trusted_raw_shred_provider.rs)

Build flag:

```toml
sof = { version = "0.12.0", features = ["kernel-bypass"] }
```

## Recommended Starting Point

Most teams should start with:

1. direct UDP for local understanding
2. gossip bootstrap when they need richer live cluster state
3. kernel-bypass only after they have measured a reason to own the front-end network stack

If the main goal is lowest latency, the usual end state is different:

1. prove correctness on direct UDP or public gossip
2. move SOF behind a trusted raw shred provider
3. keep public gossip as the independent baseline, not the fastest production path
