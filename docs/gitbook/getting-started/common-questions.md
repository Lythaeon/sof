# Common Questions

This page is for the questions people usually have before they trust themselves to integrate SOF.

## Do I Need To Understand Shreds To Use SOF?

No, not in detail.

You need to understand what role they play:

- Solana traffic arrives as packetized shreds
- SOF ingests and reconstructs them
- your code usually consumes higher-level outputs such as transaction, slot, topology, or derived
  state events

You can build useful services without learning the full shred format first.

## Do I Need RPC To Use SOF?

Not for observation.

`sof` can observe live traffic without depending on RPC.

You may still use RPC for:

- transaction submission through `sof-tx`
- recent blockhash sourcing in RPC-backed submit flows
- fallback paths in your own service

## Does SOF Behave Like A Validator?

No.

SOF is not a validator and it is not trying to be one.

The important difference:

- validator behavior tends to be broader and more state-heavy
- SOF keeps bounded queues, bounded relay, bounded repair, and a smaller operational footprint

## Will SOF Send Traffic Out Or Only Observe?

It depends on the mode you choose.

### If you start in direct UDP mode

SOF behaves more like a local observer/runtime.

### If you enable `gossip-bootstrap`

SOF can also relay traffic and participate in bounded repair.

If you want the safest first bring-up:

- start without `gossip-bootstrap`
- or disable relay and repair first while you learn the host behavior

## What Is The Easiest Useful First Integration?

Usually one of these:

- `sof::runtime::run_async()` if you want to prove the observer starts
- one plugin-backed runtime if you want to prove your service can consume events
- `TxSubmitClient::builder().with_rpc_defaults(...)` if you only need transaction submission

Those are the lowest-friction paths because each one proves one major capability without forcing
the rest of the stack into the same first step.

## Should I Start With `sof` Or `sof-tx`?

Start with `sof` when you need:

- local ingest
- transaction or slot events
- topology or leader observation
- operations control over the observer host

Start with `sof-tx` when you need:

- transaction construction
- transaction submission
- RPC/Jito/direct/hybrid routing

Use both when:

- one service both observes traffic and submits based on that local state

## Do I Need Gossip Bootstrap Immediately?

Usually no.

Start with direct UDP bring-up first if your goal is:

- proving your app can start
- proving your plugin wiring works
- keeping the network posture simple

Move to gossip bootstrap when you actually need:

- peer discovery
- richer topology
- richer leader context
- relay/repair participation

## Does `sof-tx` Require Local Leader State?

Not always.

### RPC-only flow

No local leader state is required.

### Jito-only flow

You still need a recent blockhash for unsigned submit, but not local leader targeting.

### Direct or hybrid flow

Yes, you need trustworthy leader and TPU target information.

That can come from:

- `sof`
- your own control-plane service
- another provider implementation

## What Host Size Should I Start With?

Start with the host you already trust for normal Rust services unless you have measured reasons to
do otherwise.

Do not begin by optimizing for the final extreme shape.

The right progression is:

1. make the service work
2. measure packet rate, CPU, memory, and network posture
3. then tune queue counts, worker counts, gossip posture, or ingress strategy

## What Should I Read Next?

If your biggest problem is:

- product choice: [Choose the Right SOF Path](../use-sof/adoption-paths.md)
- basic mental model: [Before You Start](before-you-start.md)
- first integration: [Install SOF](install-sof.md)
- first real runtime: [First Runtime Bring-Up](first-runtime.md)
- operations posture: [Relay, Repair, and Traffic](../operations/relay-repair-and-traffic.md)
