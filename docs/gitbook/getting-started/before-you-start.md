# Before You Start

Read this page first if SOF sounds useful but the network side of Solana still feels vague.

You do not need validator-level expertise. You do need the right mental model.

## The Short Version

SOF does not make a host early by itself.

It makes a host useful once traffic reaches it.

That means:

- if your host sees shreds late, SOF starts from late data
- if your host sees shreds early, SOF can keep parsing, local state, and downstream logic on the
  same box with less overhead than an RPC-first or ad hoc stack

So the first question is not "is SOF fast." The first question is "what ingress reaches this host
earliest."

## What SOF Actually Starts From

Depending on mode, SOF starts from one of two families:

- raw ingress
  - direct UDP
  - gossip-discovered peers
  - external kernel-bypass or private raw shred feeds
- processed provider ingress
  - Yellowstone gRPC
  - LaserStream
  - websocket transaction feeds
  - typed custom provider streams

Raw ingress gives SOF more of the substrate. Processed providers give SOF a narrower, already
productized stream.

## What Usually Wins On Latency

If lowest latency is the goal, the usual ordering is:

1. private raw shred distribution or direct validator-adjacent ingress
2. good host placement near the source, often in the same datacenter
3. local runtime and local consumers on the same machine or in the same process
4. only then local runtime efficiency

Public gossip is useful because it is open and independent. It is not usually the fastest source
of shreds.

## What A Shred Is

A shred is one piece of a slot payload sent across the network.

What matters in practice:

- transactions do not arrive as one clean application object
- raw-shred SOF first ingests packets, parses shreds, optionally verifies them, and reconstructs
  usable data
- only then do transaction, slot, and control-plane events appear

## Why Verification And Trust Posture Matter

Raw shred ingest has two very different trust postures:

- `public_untrusted`
  - verification on by default
  - strongest independence
  - highest observer CPU cost
- `trusted_raw_shred_provider`
  - verification off by default
  - intended for private, trusted raw feeds
  - misuse can admit invalid data

This is not just a performance setting. It is a trust decision.

## What Gossip Changes

Gossip mode is not "faster mode." It is "more active cluster mode."

Use gossip when you want:

- cluster discovery
- live topology
- richer peer context
- bounded relay and repair participation

Do not use gossip just because you assume it is the premium path. If you only need a first
observer bring-up or you control the packet sources already, direct UDP is simpler.

## Why Leaders And Blockhash Matter

These matter mostly for submission, not for basic observation.

- leader context helps direct transaction routing
- recent blockhash matters for normal transaction construction

Those inputs can come from:

- `sof`
- RPC
- your own internal control-plane service

## What You Do Not Need On Day One

You do not need:

- the full shred wire format
- validator internals
- every gossip detail
- every tuning knob

You do need:

- the right product choice: `sof`, `sof-tx`, or both
- the right ingress choice for your latency and trust goals
- a clear answer to whether your host should stay passive or participate in gossip/repair

## Safe Reading Order

1. [Choose the Right SOF Path](../use-sof/adoption-paths.md)
2. [Common Questions](common-questions.md)
3. [Install SOF](install-sof.md)
4. [First Runtime Bring-Up](first-runtime.md)

## One Sentence To Keep In Mind

`sof` turns ingress into local runtime state; `sof-tx` turns usable control-plane state into
transaction submission.
