# Deployment Modes

SOF can be deployed in three main modes. Pick the one that matches your traffic source and
operational tolerance.

## Mode Comparison

| Mode | Use When | Main Tradeoff |
| --- | --- | --- |
| Direct UDP listener | you control packet sources or want the simplest bring-up | no gossip-discovered topology/bootstrap |
| Gossip bootstrap | you want cluster discovery, topology updates, relay, and bounded repair | more moving parts and more outbound control-plane traffic |
| External kernel-bypass ingress | you own a specialized network front end | higher integration complexity |

## Direct UDP Listener

Best for:

- local development
- controlled feed sources
- deployments that do not need SOF to participate in gossip bootstrap

Important behavior:

- lowest setup complexity
- no live gossip runtime
- still provides the normal runtime pipeline once packets arrive

## Gossip Bootstrap

Best for:

- public hosts
- market-data style deployments that need topology and leader context
- operators willing to accept active network participation

Important behavior:

- bootstrap discovers peers from entrypoints
- SOF can relay recent shreds and serve bounded repair responses
- local topology and leader information become richer and more timely

Build flag:

```toml
sof = { version = "0.11.0", features = ["gossip-bootstrap"] }
```

## External Kernel-Bypass Ingress

Best for:

- AF_XDP or other custom high-performance receivers
- teams that want to own the NIC path but still reuse SOF downstream

Important behavior:

- SOF processes `RawPacketBatch` values from an external queue
- built-in UDP receive path is replaced by your external receiver
- downstream parse, verify, reassembly, and event surfaces stay the same

Build flag:

```toml
sof = { version = "0.11.0", features = ["kernel-bypass"] }
```

## Recommended Starting Point

Most teams should start with:

1. direct UDP for local understanding
2. gossip bootstrap when they need richer live cluster state
3. kernel-bypass only after they have measured a reason to own the front-end network stack
