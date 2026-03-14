# `sof`

`sof` is the packaged observer/runtime crate for low-latency shred ingestion and local
control-plane signals.

## Core Responsibilities

- packet ingress from direct UDP, gossip bootstrap, or external kernel-bypass receivers
- shred parse, optional verification, FEC recovery, and dataset reconstruction
- plugin-driven event emission for transactions, slots, topology, and blockhash observations
- runtime extension hosting for filtered packet/resource consumers
- bounded relay and repair behavior
- local canonical and commitment tracking without requiring an RPC dependency

## Where It Fits

`sof` is the crate you use when you need to observe, derive, and expose local runtime state from
live Solana traffic.

Typical downstream uses:

- market-data or observer services consuming plugin events
- local control-plane producers feeding execution systems
- runtime-extension hosts that need managed packet or socket resources
- derived-state consumers that need replay and restart recovery

If your only goal is to build and submit transactions, start with `sof-tx` instead.

## When Not To Use It

`sof` is probably the wrong first dependency if:

- you only need transaction build and submit
- you already trust an external control-plane service and do not need local ingest
- you do not want to operate an observer/runtime process at all

## Public Entry Points

| Area | Start Here |
| --- | --- |
| Packaged runtime | `sof::runtime` |
| Plugins | `sof::framework::ObserverPlugin` and `PluginHost` |
| Runtime extensions | `sof::framework::RuntimeExtension` and `RuntimeExtensionHost` |
| Derived-state consumers | `sof::framework::DerivedStateConsumer` and `DerivedStateHost` |
| Event payloads | `sof::event` |

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

Build flag:

```toml
sof = { version = "0.9.2", features = ["gossip-bootstrap"] }
```

### External kernel-bypass ingress

Best for:

- custom AF_XDP or other specialized network receivers
- deployments that want SOF for downstream processing while owning the front-end NIC path

Build flag:

```toml
sof = { version = "0.9.2", features = ["kernel-bypass"] }
```

## When To Use Plugins vs Runtime Extensions

Use plugins when you want decoded semantic events:

- transactions
- datasets
- slot status
- reorgs
- blockhash or topology changes

Use runtime extensions when you need runtime-managed resources or filtered packet sources:

- UDP/TCP bind or connect
- WebSocket connections
- shared extension streams
- raw ingress packet observation before semantic decode

## Semantics That Matter In Production

### Dedupe is part of the correctness boundary

SOF treats duplicate and conflicting shred suppression as a semantic contract. Downstream consumers
should not need their own duplicate-shred filter just to keep event streams sane.

### Relay and repair are bounded

SOF is not attempting validator-style unbounded network behavior. Cache windows, fanout, and repair
budgets are explicit and intentionally conservative.

### Local control plane is first-class

The runtime surfaces locally derived slot progression, leader schedule data, recent blockhash
observations, and reorg state so downstream services can act without routing every decision through
RPC.

## Useful Examples

| Example | Why It Matters |
| --- | --- |
| `observer_runtime` | minimal packaged runtime |
| `observer_with_non_vote_plugin` | simplest plugin attachment |
| `observer_with_multiple_plugins` | multi-plugin host |
| `runtime_extension_observer_ingress` | raw ingress observation with extensions |
| `runtime_extension_websocket_connector` | runtime-managed WebSocket resource |
| `derived_state_slot_mirror` | replayable stateful-consumer pattern; requires `SOF_RUN_EXAMPLE=1` |
| `tpu_leader_logger` | local leader and TPU endpoint observation |
| `kernel_bypass_ingress_metrics` | external ingress handoff |

## Operational Baseline

For initial bring-up, set only:

- `RUST_LOG`
- `SOF_BIND`
- `SOF_GOSSIP_ENTRYPOINT` when using gossip bootstrap

Reduce outbound network activity if needed:

- `SOF_UDP_RELAY_ENABLED=false`
- `SOF_REPAIR_ENABLED=false`

Then move to the [Operations](../operations/README.md) section once the host is stable.
