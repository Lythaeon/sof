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
- custom generic producers only become source-aware for readiness after they
  emit typed `Health` updates for their reserved sources; until then SOF can
  only treat generic ingress as "updates are flowing"
- `sof-tx` adapter completeness still depends on a full control-plane feed, not only transaction
  updates

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
