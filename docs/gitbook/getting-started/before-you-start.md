# Before You Start

Read this page first if SOF makes sense conceptually but Solana network internals still feel
opaque.

You do not need to become a validator engineer before you can use SOF. You do need a practical
mental model for the data SOF sees and the kinds of decisions it can make locally.

## What SOF Actually Sees

SOF does not start from RPC responses.

It starts from live Solana network traffic. In practice that means:

- packets arriving from UDP or gossip-discovered peers
- packet payloads that contain shreds
- shreds that belong to slots
- slots that eventually reconstruct into datasets and transactions

That is why SOF can be lower latency than an RPC-first architecture: it is closer to the traffic
source.

## What A Shred Is

The shortest useful definition:

- a shred is one small piece of a Solana block/slot payload sent across the network

You do not need to memorize the binary layout to use SOF.

What matters for you:

- one transaction does not arrive as one neat application object
- SOF first has to ingest packets, parse shreds, verify them if configured, and reassemble the
  larger dataset they belong to
- only after that do higher-level transaction and slot events become available

If you are building with plugins, this is why SOF can emit transaction and dataset events without
asking RPC for them first.

## What A Dataset Is

In SOF docs, a dataset is the reconstructed payload for one slot/data stream after enough shreds
arrive to make it usable.

What matters operationally:

- packets can arrive out of order
- some shreds can be missing temporarily
- recovery and repair can matter before a dataset becomes complete

You do not usually consume raw datasets directly on day one. You care because dataset
reconstruction is the bridge between packet ingest and the transaction-level events your service
actually uses.

## Why Leaders And Blockhash Matter

These two terms matter because they affect transaction submission.

### Leader

The leader is the validator expected to produce traffic for a given slot window.

Why you care:

- direct transaction submission needs to know who to target
- SOF can derive that locally when it has enough live control-plane state

### Recent blockhash

A recent blockhash is part of normal Solana transaction construction.

Why you care:

- if `sof-tx` is building and signing the transaction for you, it needs a current blockhash
- that blockhash can come from RPC or from locally observed state fed by `sof`

## What Changes When You Enable Gossip Bootstrap

This is the most important network-behavior distinction for new users.

### Direct UDP listener mode

This is the quieter mode.

Use it when you want:

- a simpler bring-up path
- controlled packet sources
- a more observer-like posture

### Gossip bootstrap mode

This is the richer but more active mode.

Use it when you want:

- cluster discovery
- live topology updates
- richer leader and peer context
- bounded relay and repair behavior

What changes operationally:

- SOF is no longer only ingesting traffic
- it can also participate in bounded relay and bounded repair

That is why the operations docs talk so much about network posture.

## What Relay And Repair Mean In Plain Language

### Relay

Relay means SOF can forward recent useful traffic onward instead of only consuming it locally.

### Repair

Repair means SOF can request or serve missing pieces when traffic arrived with gaps.

You should think about both as explicit posture decisions, not invisible background magic.

If you want the easiest first bring-up:

- start without `gossip-bootstrap`
- or disable relay/repair first when you do enable gossip mode

## What You Do Not Need To Know On Day One

You do not need all of this before your first working integration:

- the exact shred wire layout
- validator internals
- the full gossip protocol implementation
- every tuning knob in the registry

You do need to know:

- whether you want observation, submission, or both
- whether your host should stay mostly passive or participate in gossip/repair
- whether your control plane comes from RPC, from `sof`, or from another internal service

## The Safe Learning Order

If you are new, this is the order that usually prevents confusion:

1. read [Choose the Right SOF Path](../use-sof/adoption-paths.md)
2. read [Common Questions](common-questions.md)
3. read [Install SOF](install-sof.md)
4. run [First Runtime Bring-Up](first-runtime.md)
5. only then go deeper into crate pages and operations

## If You Want One Sentence To Remember

SOF is the part that turns live Solana network traffic into local usable runtime state; `sof-tx`
is the part that turns that state into transaction submission decisions.
