# Common Questions

These are the questions that usually come up before an integration feels concrete.

## Do I Need To Understand Shreds To Use SOF?

No, not in detail.

You need to understand the role they play:

- raw Solana traffic arrives as packetized shreds
- SOF can ingest and reconstruct them
- your code usually consumes higher-level outputs such as transaction, slot, topology, or derived
  state events

## Do I Need RPC To Use SOF?

Not for observation.

`sof` can observe live traffic without depending on RPC.

You may still use RPC for:

- transaction submission through `sof-tx`
- recent blockhash sourcing in RPC-backed submit flows
- fallback paths in your own service

## Is Public Gossip The Fastest Way To Run SOF?

Usually no.

Use [Before You Start](before-you-start.md) for the full ingress and latency model. The short
answer is:

- public gossip is the open, independent baseline
- it is usually not the earliest shred source
- private raw distribution, validator-adjacent ingress, and better host placement usually beat it

## Does SOF Behave Like A Validator?

No.

SOF is not a validator and it is not trying to be one.

The difference is simple:

- validator behavior is broader and more state-heavy
- SOF keeps explicit queues, bounded relay, bounded repair, and a smaller runtime footprint

## Will SOF Send Traffic Out Or Only Observe?

It depends on the mode you choose.

### If you start in direct UDP mode

SOF behaves more like a local observer/runtime.

### If you enable `gossip-bootstrap`

SOF can also relay traffic and participate in bounded repair.

The vendored gossip backend does not exact-pin the Solana `3.1.11` patch line, so downstream
workspaces can still resolve newer compatible `3.1.x` Solana crates.

If you want the safest first bring-up:

- start without `gossip-bootstrap`
- or disable relay and repair first while you learn the host behavior

## Is SOF The Only Good Option For Low-Latency Solana Work?

No.

Other good options include:

- managed processed providers
- private raw shred networks
- validator-adjacent deployments
- your own custom ingest stack

SOF is useful when you want one reusable runtime foundation with explicit replay, dedupe, health,
plugins, derived state, and operations control.

## What Is The Easiest Useful First Integration?

Usually one of these:

- `sof::runtime::run_async()` if you want to prove the observer starts
- one plugin-backed runtime if you want to prove your service can consume events
- `TxSubmitClient::builder().with_rpc_defaults(...)` if you only need transaction submission

## Should I Start With `sof` Or `sof-tx`?

Start with `sof` when you need:

- local ingest
- transaction or slot events
- topology or leader observation
- operations control over the observer host

Start with `sof-tx` when you need:

- transaction construction
- transaction submission
- RPC, Jito, signed-byte, direct, or hybrid routing

Use both when:

- one service both observes traffic and submits based on that local state

## Do I Need Gossip Bootstrap Immediately?

Usually no.

Start with direct UDP first if your goal is:

- proving your app can start
- proving your plugin wiring works
- keeping network posture simple

Move to gossip bootstrap when you actually need:

- peer discovery
- richer topology
- richer leader context
- relay and repair participation

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

## What Happens If I Do Not Set A Transaction Commitment Filter?

SOF defaults to:

- `at_commitment(TxCommitmentStatus::Processed)`

That means transaction-family hooks receive processed-or-better events unless you tighten the
filter with `.at_commitment(...)` or `.only_at_commitment(...)`.

## What Host Size Should I Start With?

Start with the host you already trust for normal Rust services unless you have measured reasons to
do otherwise.

The right progression is:

1. make the service work
2. measure packet rate, CPU, memory, and network posture
3. then tune queue counts, worker counts, gossip posture, or ingress strategy
