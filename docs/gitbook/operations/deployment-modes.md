# Deployment Modes

Choose deployment mode by ingress and trust posture, not by slogans.

The point of SOF is to let downstream teams reuse one runtime foundation instead of rebuilding
provider handling, packet handling, replay, dedupe, health, and plugin surfaces per application.

That does not mean every mode is equally early.

## Runtime Modes

| Mode | Use When | Main Tradeoff |
| --- | --- | --- |
| Direct UDP listener | you control packet sources or want the simplest bring-up | no gossip-discovered topology/bootstrap |
| Gossip bootstrap | you want cluster discovery, topology updates, relay, and bounded repair | more moving parts and more outbound control-plane traffic |
| External kernel-bypass ingress | you own a specialized network front end | higher integration complexity |
| Processed provider stream | you already have a transaction-first provider feed | SOF starts later in the pipeline than raw-shred modes |

For lowest latency, the important distinction is usually not "UDP vs gossip." It is:

- public edge vs private raw feed
- late host placement vs close host placement
- public bootstrap vs validator-adjacent ingress

## Raw-Shred Trust Posture

| Trust posture | Best fit | Main tradeoff |
| --- | --- | --- |
| `public_untrusted` | you want to own the whole stack and keep independent verification | higher CPU cost and usually later visibility than private shred distribution |
| `trusted_raw_shred_provider` | you have access to a trusted private shred feed and want SOF's fastest practical raw-shred path | you are explicitly depending on upstream trust instead of only public-edge verification |

`trusted_raw_shred_provider` disables local shred verification by default. Misuse can let invalid
data enter the observer pipeline. It should be treated as a trust-boundary decision, not just a
performance knob. It is intended for validator-adjacent or privately authenticated raw shred
distribution. If you are still on public gossip or public peers, this is the wrong posture.

Processed providers such as Yellowstone gRPC, LaserStream, or websocket feeds are a separate
category. They are useful, but they are not raw-shred trust modes.

## Direct UDP Listener

Best for:

- local development
- controlled feed sources
- deployments that do not need gossip bootstrap

Important behavior:

- lowest setup complexity
- no live gossip runtime
- still provides the normal runtime pipeline once packets arrive
- can be paired with either public or trusted raw sources

## Gossip Bootstrap

Best for:

- public hosts
- deployments that need cluster discovery, topology, and peer context
- operators willing to accept active network participation

Important behavior:

- bootstrap discovers peers from entrypoints
- SOF can relay recent shreds and serve bounded repair responses
- this is the independent baseline mode, not usually the fastest possible shred source

## External Kernel-Bypass Ingress

Best for:

- AF_XDP or other custom high-performance receivers
- teams that want to own the NIC path but still reuse SOF downstream

Important behavior:

- SOF processes `RawPacketBatch` values from an external queue
- the built-in UDP receive path is replaced by your external receiver
- downstream parse, verify, reassembly, and event surfaces stay the same

## Processed Provider Streams

Best for:

- teams that already buy or run a processed transaction feed
- services that want SOF's plugin and runtime surface without raw-shred ingest
- custom producers that can feed `ProviderStreamMode::Generic`

Important behavior:

- SOF does not run packet parsing, raw shred verification, FEC recovery, or dataset reconstruction
- built-in processed providers are transaction-first
- `ProviderStreamMode::Generic` is the flexible mode for custom producers that need richer updates
- provider durability and replay behavior depend on the specific upstream

## Recommended Starting Point

Most teams should start with:

1. direct UDP for local understanding
2. gossip bootstrap only when they need richer live cluster state
3. kernel-bypass or custom ingress only after measuring a reason to own the front-end network stack

If the main goal is lowest latency, the usual end state is different:

1. prove correctness on direct UDP or public gossip
2. move SOF behind a trusted raw shred provider or validator-adjacent ingress
3. keep public gossip as the independent baseline, not the fastest production path
