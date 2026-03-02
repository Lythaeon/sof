# ADR-0008: RuntimeExtension Capability Model and Filtered Ingress

- Status: Implemented
- Date: 2026-03-02
- Decision makers: `sof-observer` maintainers
- Related: `docs/architecture/framework-plugin-hooks.md`, `docs/architecture/runtime-extension-hooks.md`, `docs/architecture/runtime-bootstrap-modes.md`, `docs/architecture/adr/0007-always-on-relay-and-shred-cache-mesh.md`

## Context

`sof-observer` already provides `ObserverPlugin` hooks for runtime observation callbacks. That hook system is intentionally focused on event consumption from SOF-managed ingest/parse/reassembly flow.

We also need a separate runtime extension substrate for users who want to:

1. declare additional runtime-owned network resources (UDP/TCP/WebSocket-style connectors),
2. subscribe to packet events with strict startup-time filters, and
3. keep ingest-safe behavior (non-blocking dispatch, queue-drop metrics, bounded shutdown).

This ADR is not a migration from `ObserverPlugin`. It defines an additional, independent subsystem named `RuntimeExtension`.

## Decision

Adopted `RuntimeExtension` as a separate runtime system from `ObserverPlugin`, with:

1. independent host/dispatcher lifecycle,
2. declarative startup manifests for capabilities/resources/subscriptions,
3. immutable startup-time packet filters, and
4. owner-only visibility for extension streams unless explicitly shared by tag.

## Public API

### RuntimeExtension trait

Added trait:

1. `on_startup(ctx) -> Result<ExtensionManifest, ExtensionStartupError>`
2. `on_packet_received(event)`
3. `on_shutdown(ctx)`

`ObserverPlugin` remains unchanged and separate.

### RuntimeExtensionHost

Added dedicated host/builder APIs:

1. `RuntimeExtensionHostBuilder`
2. `RuntimeExtensionHost`
3. startup report + startup failure records
4. capability policy type with allow/deny controls

### Declarative capabilities

Startup manifests declare explicit capabilities:

1. `BindUdp`
2. `BindTcp`
3. `ConnectTcp`
4. `ConnectWebSocket`
5. `ObserveObserverIngress`
6. `ObserveSharedExtensionStream`

### Declarative resources

Startup manifests declare resources with stable `resource_id`:

1. `UdpListener`
2. `TcpListener`
3. `TcpConnector`
4. `WsConnector`

Each resource carries visibility:

1. `Private` (owner-only)
2. `Shared { tag }` (opt-in cross-extension subscription by tag)

### Startup-only filters

Packet subscriptions are immutable after startup and support endpoint/type metadata matching:

1. source kind (`ObserverIngress` / `ExtensionResource`)
2. transport (`Udp` / `Tcp` / `WebSocket`)
3. event class (`Packet` / `ConnectionClosed`)
4. WebSocket frame type (`Text` / `Binary` / `Ping` / `Pong`) when transport is `WebSocket`
5. local/remote endpoint fields
6. owner extension, resource id, shared tag

Payload-DSL filtering remains out of scope in v1.

## Runtime Behavior

### Startup

1. host calls each extension `on_startup` with timeout,
2. validates manifest against capability policy,
3. validates resource/capability consistency,
4. provisions requested resources for successful extensions, and
5. isolates startup failures per extension (runtime continues).

### Ingress and filtering

1. observer ingress packets are wrapped into shared packet source metadata,
2. extension-owned resources emit the same packet envelope shape,
3. dispatch checks capability gates + visibility + immutable subscriptions, and
4. `on_packet_received` is called only for matching extensions.

### Visibility and ownership

1. owner extension always can observe its own resource packets (subject to its own subscriptions),
2. other extensions can observe only when stream is shared by tag and they have `ObserveSharedExtensionStream`,
3. owner-only behavior is default.

### Backpressure and failure isolation

1. non-blocking queue dispatch (`try_send`) for extension packet events,
2. queue pressure drops events and increments per-extension counters,
3. extension callback panics are isolated and logged; runtime continues.

### Shutdown

1. abort resource tasks first,
2. call extension `on_shutdown` with timeout,
3. force-cancel lingering extension worker tasks after deadline.

### WebSocket connectors

`WsConnector` is implemented with full WebSocket protocol handling:

1. `ws://` and `wss://` URLs are supported,
2. opening handshake is performed by runtime-managed connector code,
3. incoming WebSocket messages are decoded and emitted into extension packet dispatch,
4. decoded frame type metadata is attached to packet-class source envelopes,
5. close frames emit `ConnectionClosed` lifecycle events, and
6. `Ping` frames trigger `Pong` replies from connector runtime.

## Explicit Non-Goals (v1)

1. Dynamic shared-library extension loading.
2. Payload-byte filter DSL in core runtime.
3. Runtime-managed WebSocket server/listener resources (connector-only in v1).

## Consequences

Positive:

1. Explicit extension boundary without changing plugin behavior.
2. Capability-gated runtime resource model.
3. Predictable packet routing with startup-only filtering.
4. Ingest latency safety preserved under extension load.

Tradeoffs:

1. Additional host/runtime surface area to maintain.
2. More startup validation and telemetry complexity.

## Validation Plan

Implemented validation includes:

1. unit tests for filter matching, capability denial, and startup failure isolation,
2. startup resource provisioning coverage including `WsConnector`,
3. WebSocket-specific tests for frame-type subscription filtering and `ConnectionClosed` lifecycle dispatch,
4. queue-pressure tests ensuring drops/metrics without ingest blocking,
5. shutdown tests for graceful timeout and force-cancel behavior,
6. regression checks that existing `ObserverPlugin` runtime path is unchanged,
7. example compilation and runtime smoke checks for runtime extension examples.
