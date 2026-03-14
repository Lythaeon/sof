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
