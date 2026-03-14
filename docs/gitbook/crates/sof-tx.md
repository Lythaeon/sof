# `sof-tx`

`sof-tx` is the transaction SDK in the workspace. It is built for services that need predictable
submit behavior and can benefit from local control-plane signals.

It is not a traffic-ingest runtime. It does not observe shreds, derive slot state, or discover
leaders by itself.

## What It Owns

- message and transaction construction
- signing boundary types
- submit mode orchestration
- routing policy and signature dedupe
- direct leader-target submission
- optional Jito and kernel-bypass transports
- adapters that ingest live or replayed state from `sof`

## What It Does Not Own

- Solana traffic ingest
- shred parsing or verification
- dataset reconstruction
- slot, fork, or topology observation
- deriving control-plane state directly from live network traffic

Those responsibilities belong to `sof` or to your own external control plane.

## When Not To Use It

`sof-tx` is probably the wrong first dependency if:

- you need shred ingest, dataset reconstruction, or plugin events
- you want local leader and blockhash state but do not yet have a source for it
- you are looking for a wallet-oriented UX helper rather than an execution SDK

## Main Types

| Type | Purpose |
| --- | --- |
| `TxBuilder` | build legacy or `V0` transactions |
| `TxSubmitClient` | configure transports and submit policy |
| `SubmitMode` | choose `RpcOnly`, `JitoOnly`, `DirectOnly`, or `Hybrid` |
| `RoutingPolicy` | choose primary and fallback fanout behavior |
| `SignatureDeduper` | avoid duplicate sends at signature granularity |
| `LeaderProvider` / `RecentBlockhashProvider` | abstract control-plane sources |

## Submission Modes

### `RpcOnly`

Use when you want the simplest operational path and can accept RPC dependency for delivery.

### `DirectOnly`

Use when you have confidence in local leader and TPU endpoint state and want the lowest-latency
path.

### `Hybrid`

Use when you want direct leader targeting first with an RPC fallback path. This is the normal
starting point for latency-sensitive services because it balances speed with operational recovery.

### `JitoOnly`

Use when your flow is built explicitly around block-engine submission.

## Integration With `sof`

With the `sof-adapters` feature enabled, the SDK can consume live or replayed control-plane state
originating from the observer runtime.

This is an optional integration layer, not a hard dependency:

- `sof-tx` can run standalone with static or externally supplied providers
- pair it with `sof` only when you want locally observed control-plane state to drive submission

Two important adapter paths:

- `PluginHostTxProviderAdapter`: live in-process adapter fed by plugin events
- `DerivedStateTxProviderAdapter`: replayable adapter for restart-safe services

That split matters:

- live adapter for fast local runtime coupling
- replay adapter for stateful services that must recover cleanly across restarts

Practical fit:

- `sof` produces the control plane
- `sof-tx` consumes that control plane for send-time decisions

## Flow-Safety Checks

`sof-tx` can evaluate flow-safety before sending on local state. Typical failure causes:

- missing recent blockhash
- stale tip slot
- missing leader schedule
- missing TPU addresses for targeted leaders
- degraded cluster topology freshness

This is a core design choice: the SDK surfaces submit-time safety explicitly instead of burying it
in implicit retries.

## Recommended Adoption Pattern

1. start with `TxBuilder` and `TxSubmitClient`
2. wire in RPC transport first
3. add direct transport and `Hybrid` mode
4. attach `sof` adapters only after local runtime state is available and measured

## Feature Flags

```toml
sof-tx = { version = "0.9.2", features = ["sof-adapters"] }
sof-tx = { version = "0.9.2", features = ["kernel-bypass"] }
sof-tx = { version = "0.9.2", features = ["jito-grpc"] }
```

## Good Fit

`sof-tx` is a good fit when you are building:

- arbitrage or execution services
- strategy engines that own their own routing policy
- infra services that need to consume leader and blockhash state during submission

It is a poor fit if you just need a generic wallet helper with minimal infrastructure concerns.
