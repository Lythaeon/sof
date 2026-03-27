# Choose the Right SOF Path

Most services do not need the whole workspace on day one. Start from the smallest useful slice.

## Common Adoption Patterns

### I need a local Solana ingest and event runtime

Start with [`sof`](../crates/sof.md) when the service needs:

- a local observer/runtime
- plugin or derived-state outputs
- local slot, topology, or blockhash signals
- runtime extensions
- explicit replay, dedupe, health, and operations boundaries

### I need transaction construction and submission

Start with [`sof-tx`](../crates/sof-tx.md) when the service needs:

- a transaction builder
- RPC-backed submission
- Jito integration
- signed-byte submission
- optional direct leader routing or hybrid routing once local control-plane data exists

Important boundary:

- `sof-tx` builds, signs, routes, and submits transactions
- it does not ingest Solana traffic or derive live leader and blockhash state by itself
- it expects those inputs through provider traits, either from static values, your own control
  plane, RPC, or optional adapters fed by `sof`

### I need both local ingest state and execution

Use `sof` together with `sof-tx` when the execution service wants locally sourced control-plane
state for submission decisions.

That is the normal pairing for services that:

- derive leader, topology, slot, and recent blockhash state from live traffic
- want direct or hybrid routing decisions
- need one process or one host to own both observation and submission decisions

That adapter path is complete today in raw-shred and gossip-backed SOF runtimes. Built-in
processed provider adapters can now emit transactions, transaction status, account updates,
block-meta, logs, and slots where supported, but they still do not by themselves provide the full
live `sof-tx` control-plane surface.

This does not mean `sof-tx` depends on `sof` in general. It does not. `sof-tx` is still useful on
its own for RPC, Jito, and signed-byte flows.

### I need repeatable host tuning profiles

Add [`sof-gossip-tuning`](../crates/sof-gossip-tuning.md) when you want typed host presets instead
of environment-variable sprawl.

## Decision Shortcuts

| If you need... | Start here |
| --- | --- |
| local observation, plugins, derived state, or runtime ownership | `sof` |
| transaction build and submit client | `sof-tx` |
| submission informed by local observed state | `sof` + `sof-tx` |
| typed runtime tuning profiles | `sof-gossip-tuning` |
| gossip backend internals | only then look at `sof-solana-gossip` |
