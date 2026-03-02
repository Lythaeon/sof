# Runtime Extension Hooks

`sof-observer` exposes a separate runtime extension framework in `sof::framework`
for declarative resource provisioning and filtered packet callbacks.

This surface is independent from `ObserverPlugin`:

1. `ObserverPlugin` is the event-hook framework documented in `framework-plugin-hooks.md`.
2. `RuntimeExtension` is a separate capability/resource/filter framework documented here.
3. Neither system is a migration path for the other.

## Public API Surface

- Trait:
  - `RuntimeExtension`
- Manifest + capability/resource/filter types:
  - `ExtensionManifest`
  - `ExtensionCapability`
  - `ExtensionResourceSpec`
  - `PacketSubscription`
  - `RuntimePacketEvent`
  - `RuntimePacketSource`
  - `RuntimePacketSourceKind`
  - `RuntimePacketTransport`
  - `RuntimePacketEventClass`
  - `RuntimeWebSocketFrameType`
- Host/runtime wiring:
  - `RuntimeExtensionHost`
  - `RuntimeExtensionHostBuilder`
  - `RuntimeExtensionCapabilityPolicy`
  - `RuntimeExtensionStartupReport`

## Hook Semantics

Current hook count: `3` (must stay in sync with `sof::framework::RuntimeExtension`).

1. `on_startup`:
   - Called once during runtime bootstrap.
   - Returns an `ExtensionManifest` declaring:
     - capabilities,
     - runtime-managed resources,
     - immutable packet subscriptions.
2. `on_packet_received`:
   - Called only for packets matching startup-declared subscriptions.
   - Includes source metadata + packet bytes.
3. `on_shutdown`:
   - Called during runtime shutdown with bounded timeout.
   - Long-running callbacks are force-cancelled after shutdown deadline.

## Capability Model

Runtime validates startup manifests against capability policy before provisioning resources.

Current capability set:

1. `BindUdp`
2. `BindTcp`
3. `ConnectTcp`
4. `ConnectWebSocket`
5. `ObserveObserverIngress`
6. `ObserveSharedExtensionStream`

## Resource Model

Startup manifests can request runtime-managed resources:

1. `UdpListener`
2. `TcpListener`
3. `TcpConnector`
4. `WsConnector`

`WsConnector` supports full WebSocket protocol handling:

1. `ws://` and `wss://` URLs,
2. opening handshake,
3. decoded message frame delivery to extension dispatch,
4. `Ping` / `Pong` handling.

## Visibility and Sharing

Extension-owned streams are private by default.

Visibility options:

1. `Private`:
   - only owning extension receives events from that resource.
2. `Shared { tag }`:
   - owner still receives events,
   - other extensions can subscribe using matching `shared_tag`
     plus `ObserveSharedExtensionStream`.

## Packet Filter Semantics

Subscriptions are immutable after startup and match on metadata fields:

1. source kind (`ObserverIngress` / `ExtensionResource`)
2. transport (`Udp` / `Tcp` / `WebSocket`)
3. event class (`Packet` / `ConnectionClosed`)
4. WebSocket frame type (`Text` / `Binary` / `Ping` / `Pong`) when transport is `WebSocket`
5. local/remote endpoint fields
6. owner extension name
7. resource id
8. shared tag

No payload-byte DSL exists in v1.

## Runtime Wiring

The packaged runtime integrates `RuntimeExtensionHost` in parallel with `PluginHost`:

1. startup:
   - runs extension startup hooks with timeout,
   - validates capabilities/resources,
   - provisions resources for successful extensions,
   - records startup failures per extension without aborting runtime.
2. data plane:
   - observer ingress packets are offered to extension host,
   - extension resource packets are fed through same source envelope model,
   - non-blocking dispatch uses bounded queues.
3. backpressure:
   - full queues drop extension events to protect ingest latency,
   - per-extension drop counters are available.
4. shutdown:
   - resource tasks are aborted,
   - shutdown hooks run with deadline,
   - remaining extension worker tasks are force-cancelled.

WebSocket close behavior:

1. `WsConnector` remote close emits `RuntimePacketEventClass::ConnectionClosed`,
2. close reason bytes are forwarded in `event.bytes` when present,
3. frame-type metadata is not set for close lifecycle events.

## Runtime Entrypoints

Packaged runtime includes extension-specific entrypoints:

1. `run_with_extension_host`
2. `run_async_with_extension_host`
3. `run_with_hosts`
4. `run_async_with_hosts`

The dual-host entrypoints run `PluginHost` and `RuntimeExtensionHost` together.

## Examples

- `crates/sof-observer/examples/runtime_extension_observer_ingress.rs`
- `crates/sof-observer/examples/runtime_extension_udp_listener.rs`
- `crates/sof-observer/examples/runtime_extension_shared_stream.rs`
- `crates/sof-observer/examples/runtime_extension_with_plugins.rs`
- `crates/sof-observer/examples/runtime_extension_websocket_connector.rs`

Run one:

```bash
cargo run --release -p sof --example runtime_extension_websocket_connector
```
