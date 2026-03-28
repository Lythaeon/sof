# Choose Your Control Plane Source

The main architecture decision behind `sof-tx` is not "RPC or direct send." It is where leader,
TPU, slot, topology, and recent blockhash state comes from.

## The Four Real Options

### 1. Static or manually supplied inputs

Use when:

- you are testing
- your targets are fixed by another system
- you only need `sof-tx` as a submission client

Tradeoff:

- simplest integration
- weakest freshness guarantees unless your external source is strong

### 2. External dynamic control plane

Use when:

- you already run an internal service for blockhash, leaders, or TPU routing
- you do not want a local observer/runtime in the same process

Tradeoff:

- clear separation of concerns
- freshness and failure behavior now depend on that external service

### 3. Live in-process SOF adapter

Use when:

- one service both observes traffic and submits transactions
- you want the freshest local view the host can provide
- restart replay is less important than tight local coupling

Tradeoff:

- best in-process freshness for that host
- adapter state is process-local unless you add your own persistence
- complete today with raw-shred and gossip SOF runtimes; built-in processed provider adapters are
  not yet a full `sof-tx` control-plane source by themselves

### 4. Replayable SOF-derived control plane

Use when:

- the service must recover after restart without rebuilding all control-plane state from scratch
- you need checkpointing and replay semantics
- you are building a longer-lived stateful execution service

Tradeoff:

- strongest restart posture
- more moving parts and more integration work
- assumes a runtime that emits the full control-plane feed, not only the narrower built-in
  processed-provider event surface

## Recommended Starting Point

Most teams should pick one of these:

1. `sof-tx` with external providers if they already have a control-plane service
2. `sof` plus `sof-tx` with the live plugin adapter if they want one local service
3. derived-state adapter only after restart recovery becomes a hard requirement

## Quick Mapping

| If you need... | Use this source |
| --- | --- |
| lowest local decision latency on one host | `PluginHostTxProviderAdapter` |
| restart-safe local control plane | `DerivedStateTxProviderAdapter` |
| no local observer runtime at all | custom external providers |
| test-only or fixed targets | static providers |

Your control-plane source directly affects:

- whether `DirectOnly` or `Hybrid` is trustworthy
- how much stale-state risk you carry
- whether flow-safety checks reject valid sends or miss bad ones
- how cleanly the service recovers after restart
