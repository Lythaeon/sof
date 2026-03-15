# Choose the Right SOF Path

Most services do not need the whole workspace on day one. Start from the smallest useful slice.

## Common Adoption Patterns

### I need a local Solana ingest and event runtime

Start with [`sof`](../crates/sof.md) when the service needs:

- shred ingest
- local slot and blockhash signals
- plugin callbacks
- runtime extensions
- bounded relay and repair behavior

### I need transaction construction and submission

Start with [`sof-tx`](../crates/sof-tx.md) when the service needs:

- a transaction builder
- direct leader routing
- hybrid direct plus RPC submission
- Jito integration

Important boundary:

- `sof-tx` builds, signs, routes, and submits transactions
- it does not ingest Solana traffic or derive live leader and blockhash state by itself
- it expects those inputs through provider traits, either from static values, your own control
  plane, or optional adapters fed by `sof`

### I need both local ingest state and execution

Use `sof` together with `sof-tx` when the execution service wants locally sourced control-plane
state for submission decisions.

That is the normal pairing for services that:

- derive leader, topology, slot, and recent blockhash state from live traffic
- want low-latency direct sends
- need flow-safety checks before submitting

How they fit together:

- `sof` observes traffic and emits control-plane signals
- `sof-tx` consumes those signals through `LeaderProvider` and `RecentBlockhashProvider`
- `sof-tx` adapters bridge `sof` plugin events or derived-state feed state into those provider
  traits
- `sof-tx` then uses that state to decide whether direct or hybrid submission is safe

### I need repeatable host tuning profiles

Add [`sof-gossip-tuning`](../crates/sof-gossip-tuning.md) when you want typed host presets
instead of environment-variable sprawl.

## Decision Shortcuts

| If you need... | Start here |
| --- | --- |
| packet ingest, datasets, slot events, plugin hooks | `sof` |
| transaction build and submit client | `sof-tx` |
| transaction submission using live local control-plane state from observed traffic | `sof` + `sof-tx` |
| typed runtime tuning profiles | `sof-gossip-tuning` |
| gossip backend internals | only then look at `sof-solana-gossip` |

## What Can Wait Until Later

These usually do not need attention on day one:

- repository layout
- ADR and ARD process
- contributor quality gates
- vendored gossip backend internals

Those belong to the maintainer path, not the consumer path.
