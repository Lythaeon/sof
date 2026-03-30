# `sof`

`sof` is the observer/runtime crate.

It can start from raw shreds or from processed provider streams while keeping one runtime surface
for plugins, derived state, and local control-plane consumers.

## Core Responsibilities

- packet ingress from direct UDP, gossip bootstrap, or external kernel-bypass receivers
- processed provider ingress from Yellowstone gRPC, LaserStream gRPC, websocket
  transaction/logs/account/program feeds, or `ProviderStreamMode::Generic`
- shred parse, optional verification, recovery, and dataset reconstruction
- plugin-driven event emission for transactions, slots, topology, and blockhash observations
- runtime extension hosting for filtered packet or resource consumers
- bounded relay and repair behavior where the ingress mode supports them
- local canonical and commitment tracking without requiring RPC in the observation path

In raw-shred modes, SOF owns packet, shred, verify, and reconstruction work. In processed provider
modes, SOF owns the provider/runtime boundary and keeps the downstream semantics aligned where the
provider surface allows it.

## Where It Fits

`sof` is the crate you use when you need to observe, derive, and expose local runtime state from
live Solana traffic or from a processed provider feed.

Typical downstream uses:

- observer services consuming plugin events
- local control-plane producers feeding execution systems
- runtime-extension hosts that need managed packet or socket resources
- derived-state consumers that need replay and restart recovery

If your only goal is to build and submit transactions, start with `sof-tx` instead.

## Runtime Modes

### Direct UDP listener

Best for:

- local testing
- controlled private traffic sources
- deployments that do not need gossip bootstrap

### Gossip bootstrap

Best for:

- public ingress hosts
- deployments that need live topology and leader context
- relay and bounded repair participation

This is the independent public-edge mode. It is not the default answer for lowest latency.

### External kernel-bypass ingress

Best for:

- custom AF_XDP or other specialized network receivers
- deployments that want SOF for downstream processing while owning the front-end NIC path

### Processed provider streams

Best for:

- teams that already buy or run a processed transaction feed
- services that want SOF's plugin and runtime surface without raw-shred ingest
- custom producers that can feed `ProviderStreamMode::Generic`

Important boundary:

- built-in Yellowstone, LaserStream, and websocket adapters now cover
  transactions, transaction status, accounts, block-meta, logs, and slots where
  the upstream surface supports them
- `ProviderStreamMode::Generic` is the flexible processed-provider mode
- multi-source fan-in can now arbitrate overlapping duplicates by policy:
  - `EmitAll`
  - `FirstSeen`
  - `FirstSeenThenPromote`
- custom generic producers only become source-aware for readiness after they
  emit typed `Health` updates for their reserved sources; until then SOF can
  only treat generic ingress as "updates are flowing"
- `sof-tx` adapter completeness still depends on a full control-plane feed, not only transaction
  updates
- built-in websocket and transaction-feed gRPC can still supply recent blockhash to `sof-tx`
  adapters through observed transactions; direct routing still needs leaders/topology from gossip,
  manual targets, or another control-plane source
- the packaged runtime now supports one narrower mixed-source shape:
  built-in provider transactions plus gossip-derived cluster topology
- use `PluginHostTxProviderAdapter::topology_only(...)` for that packaged mixed mode
- use raw-shred/gossip runtimes or `ProviderStreamMode::Generic` when you also need
  leader-schedule or reorg hooks

### Plugin vs Derived-State Surface

SOF intentionally does not mirror every plugin callback into derived state.

| Ingest type | Plugin surface | Derived-state surface | Does not emit |
| --- | --- | --- | --- |
| Raw shreds / gossip / trusted raw-shred provider | transactions, recent blockhash, slot status, cluster topology, leader schedule, reorg, plus raw packet/shred/dataset surfaces | transaction apply, recent-blockhash observation, slot status, epoch-boundary observation, topology, leader schedule, control-plane state, reorg/invalidation, account-touch | transaction-status, transaction-log, account-update, block-meta, provider-derived `TransactionStatusObserved`, provider-derived `BlockMetaObserved`, `RootedAccountObserved` |
| Websocket `transactionSubscribe` | `on_transaction`, synthesized `on_recent_blockhash` when requested | `TransactionApplied` | transaction-status, block-meta, and topology/leader/reorg control-plane families |
| Websocket `logsSubscribe` | `on_transaction_log` | none | transaction-status, block-meta, control-plane, and derived-state provider observations |
| Websocket `accountSubscribe` / `programSubscribe` | `on_account_update` | finalized `RootedAccountObserved` | transaction-status, block-meta, control-plane, and non-account provider observations |
| Yellowstone / LaserStream transaction feeds | `on_transaction`, synthesized `on_recent_blockhash` when requested | `TransactionApplied` | topology/leader/reorg control-plane families unless supplied through `Generic` |
| Yellowstone / LaserStream transaction-status feeds | `on_transaction_status` | `TransactionStatusObserved` | block-meta and raw-shred control-plane families unless separately supplied |
| Yellowstone / LaserStream block-meta feeds | `on_block_meta` | `BlockMetaObserved` | transaction-status and raw-shred control-plane families unless separately supplied |
| Yellowstone / LaserStream account feeds | `on_account_update` | finalized `RootedAccountObserved`; built-in account configs default to finalized commitment unless explicitly overridden | provider-derived transaction-status/block-meta observations and raw-shred control-plane families |
| Yellowstone / LaserStream slot feeds | `on_slot_status` | `SlotStatusChanged`, `EpochBoundaryObserved` | recent-blockhash/topology/leader-schedule/reorg unless supplied through `Generic` |
| `ProviderStreamMode::Generic` | any typed `ProviderStreamUpdate` variant the producer emits | the derived-state families SOF currently forwards from those typed updates | anything the producer does not emit |

So the clean split is:

- raw shreds emit the richest local control-plane surface
- built-in websocket emits transactions/logs/accounts and can synthesize recent
  blockhash from observed transactions, but not transaction status, block meta,
  or topology/leader control-plane hooks
- built-in Yellowstone/LaserStream add transaction status and block meta, but
  still do not replace the raw-shred control-plane surface on their own
- finalized account feeds are the current provider-backed rooted-authoritative
  derived-state input surface
- websocket account/program subscriptions default to finalized commitment
- Yellowstone and LaserStream account feeds also default to finalized commitment
  unless the config explicitly overrides it

## When To Use Plugins vs Runtime Extensions

Use plugins when you want decoded semantic events:

- transactions
- datasets
- slot status
- reorgs
- blockhash or topology changes

Use runtime extensions when you need runtime-managed resources or filtered packet sources:

- UDP or TCP bind/connect
- WebSocket connections
- shared extension streams
- raw ingress packet observation before semantic decode
