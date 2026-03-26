# Integration Model

SOF exposes three distinct integration surfaces. They solve different problems and should not be
treated as interchangeable.

## 1. Observer Plugins

Plugins are the semantic event-hook surface.

Use plugins when you want callbacks for:

- transactions
- datasets
- slot status
- reorgs
- recent blockhash updates
- cluster topology changes
- leader schedule changes

Plugins are the right fit for downstream logic that consumes decoded runtime events.
They declare static hook interest with `PluginConfig` and may use startup/shutdown lifecycle hooks
for plugin-local initialization and cleanup.

Provider mode matters here:

- raw-shred and gossip runtimes can emit the full normal plugin surface
- built-in processed providers are intentionally transaction-first
- `ProviderStreamMode::Generic` is the path for custom producers that want to
  feed richer control-plane updates through the same host surface

That asymmetry is deliberate, not accidental. Switching ingress modes changes
both transport and semantics.

## 2. Runtime Extensions

Runtime extensions are the capability and resource-management surface.

Use extensions when you need:

- runtime-managed UDP or TCP listeners
- outbound TCP or WebSocket connectors
- filtered raw packet delivery
- shared extension-owned streams

Extensions are the right fit when you need IO resources or source-level packet filtering rather
than semantic decoded events.

## 3. Derived-State Consumers

Derived-state consumers are for authoritative state materialization and replay.

Use this path when you need:

- deterministic ordering
- checkpointing
- restart-safe replay
- explicit rollback handling

This is the correct surface for stateful systems such as local banks or geyser-like materializers.

## Choosing The Right Surface

| Need | Best Surface |
| --- | --- |
| log or react to observed transactions | plugin |
| consume raw ingress packets from runtime-managed resources | runtime extension |
| maintain restart-safe local derived state | derived-state consumer |
| feed local leader and blockhash state into `sof-tx` | plugin or derived-state adapter, depending on recovery needs |

One important boundary:

- built-in Yellowstone, LaserStream, and websocket adapters are not a complete
  `sof-tx` control-plane source today
- `ProviderStreamMode::Generic` can be, if the producer supplies the full feed

So users should think in two steps:

1. what ingest path reaches the host earliest
2. what semantic surface that path can honestly emit

## Common Mistake To Avoid

Do not use plugins as a substitute for a replay contract. Plugins are primarily observational. If
you need authoritative state derivation with rollback semantics, move to the derived-state path.
