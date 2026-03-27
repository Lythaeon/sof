# Derived State and Control Plane

One of SOF's most important design choices is that local control-plane state is treated as a
first-class product surface.

## What SOF Derives Locally

From live shred flow, the runtime can surface:

- recent blockhash observations
- leader schedule context
- cluster topology and TPU endpoint data
- slot progression and commitment depth
- reorg events and canonical branch changes

That means downstream services can make decisions from local state instead of paying the latency
and failure cost of routing every check through RPC.

Processed provider mode is different:

- built-in Yellowstone, LaserStream, and websocket adapters can drive transaction, transaction
  status, account update, block-meta, log, and slot consumers where supported
- they can still drive recent-blockhash extraction from observed transactions
- they do not, by themselves, form the full live control-plane surface needed by
  `sof-tx` adapters
- `ProviderStreamMode::Generic` is the processed-provider path that can carry
  richer control-plane updates when a custom producer has them

## Why `sof-tx` Cares

Direct or hybrid transaction submission only works well when you have timely answers for:

- who the near-term leaders are
- which TPU endpoints are usable
- whether your recent blockhash is fresh enough
- whether the local topology view is degraded

`sof-tx` consumes those answers through provider traits and optional adapters. It then applies
flow-safety checks before sending.

## Live Adapter vs Replay Adapter

### Live plugin adapter

Use when:

- the observer runtime and sender live in the same process or tightly coupled service
- you want the freshest in-memory state
- restart replay is not the primary concern

### Replayable derived-state adapter

Use when:

- the service must recover safely after restart
- checkpoints and deterministic re-application matter
- you want control-plane state to survive beyond one runtime session

## Practical Rule

Use live state when freshness dominates. Use replayed state when correctness across restarts
dominates. Many serious services need both: fresh live updates on top of a replayable baseline.

## Consumer Shape

Derived-state consumers now follow the same broad discipline as plugins:

- static interest is declared once with `DerivedStateConsumerConfig`
- optional `setup` / `shutdown` hooks handle consumer-local lifecycle work
- `load_checkpoint` / `flush_checkpoint` remain the actual durability boundary

That split matters because startup hooks are operational, while checkpoint methods define replay
continuity.

## Lifecycle and Rebuild Semantics

The intended derived-state lifecycle is:

1. consumer `setup`
2. checkpoint load
3. retained replay tail and/or configured replay source application
4. live event application
5. checkpoint flush on the configured durability boundary
6. consumer `shutdown`

Persistence guarantees depend on what you configured:

- without a checkpoint store, the consumer is live-only
- with a checkpoint store, recovery starts from the last durable checkpoint
- with a retained replay tail, SOF can bridge recent history on top of that checkpoint

Rebuild semantics are explicit too:

- if replay continuity is intact, SOF re-applies retained history and returns to live mode
- if replay continuity is broken, the consumer is marked unhealthy instead of silently pretending recovery succeeded

That is why derived state is the authoritative state surface and plugins are not.
