# SOF Compared To The Usual Alternatives

SOF is usually evaluated against one of four things:

- an RPC-first application
- a managed stream or Geyser provider
- a self-operated validator with Geyser
- another self-operated ingest stack

One note matters across every comparison on this page:

SOF is only earlier when its ingress is earlier.

It removes local runtime and API-boundary overhead. It does not win the upstream visibility race by
itself. For the ingress/latency model behind that statement, use
[Before You Start](../getting-started/before-you-start.md).

## Quick Comparison

| Option | Primary data path | Operational model | Typical fit |
| --- | --- | --- | --- |
| RPC-first app | RPC responses | consume a standard API from your own or someone else's RPC endpoint | account reads, transaction history, general application backends |
| Managed stream / Geyser provider | provider-managed validator or stream infrastructure | subscribe to a hosted stream product | teams that want stream consumption without operating ingest infrastructure |
| Self-operated validator with Geyser | your own validator and plugin pipeline | operate validator-shaped infrastructure yourself | teams that already need validator-level data access or want full validator-adjacent control |
| SOF | live observed traffic or processed provider ingress | operate a focused runtime boundary yourself | low-latency analytics, monitoring, automation, custom streams, and execution-adjacent services |

Another way to read the table:

- RPC/provider-first architectures optimize for consuming an already-productized API
- SOF optimizes for owning one runtime foundation and letting your application code sit directly on
  top of it

## SOF vs RPC

| Question | RPC-first | SOF |
| --- | --- | --- |
| Data source | RPC server responses | live traffic or provider ingress owned by your service |
| Integration shape | request/response | runtime ingest and event flow |
| Control over parsing and retention | limited to the RPC/API surface | owned by the embedding service |
| Control-plane inputs such as recent blockhash or leader context | external | can be locally derived, depending on ingress mode |
| Operational effort | lower | higher |

RPC is usually the simpler fit when a standard read surface is enough.

SOF is a better fit when the service needs the runtime boundary itself rather than only an API
surface built on top of it.

## SOF vs Managed Stream Or Geyser Providers

| Question | Managed stream / Geyser provider | SOF |
| --- | --- | --- |
| Who operates ingest? | provider | you |
| Time to first integration | shorter | longer |
| Stream schema and product surface | provider-defined | service-defined |
| Filtering and enrichment | limited to what the provider exposes | implemented by your own service |
| Cost model | subscription or usage pricing | your own host and network cost |

Managed providers are a strong fit when the main goal is to consume a stream quickly and move on.

SOF is a stronger fit when the goal is to own the runtime boundary, filtering logic, and
downstream product surface.

If the main requirement is lowest latency, managed providers are not the only alternative. SOF can
also sit behind private raw distribution, validator-adjacent ingress, or validator-adjacent
processed feeds. Public gossip is not the benchmark to beat there.

## SOF vs Self-Operated Validator With Geyser

| Question | Self-operated validator + Geyser | SOF |
| --- | --- | --- |
| Core mission | validator participation plus plugin output | focused observation and local runtime state |
| State and infrastructure footprint | larger | smaller |
| Operational complexity | higher | lower |
| Best for | validator operators and heavy validator-adjacent systems | focused backend services that need a reusable local runtime |

SOF is not a validator replacement. It is a narrower runtime for a narrower job.

## SOF vs Another Self-Operated Ingest Stack

| Question | Other custom ingest stack | SOF |
| --- | --- | --- |
| Build vs buy inside your own stack | build and maintain the full ingest/runtime surface yourself | start from an existing Solana-focused runtime |
| Solana-specific runtime features | depends on your implementation | built in |
| Ability to customize | maximum | high, within SOF's runtime model |
| Time to production | depends on your team and scope | usually shorter |

The biggest difference is not just time to first prototype. It is how much low-level work you stop
repeating across every service:

- provider adapters
- reconnect and replay logic
- queueing and overload boundaries
- dedupe and replay correctness
- hot-path copy and allocation cleanup
- runtime health and observability

## When SOF Is The Right Choice

SOF is a good fit for services that need:

- local observation of live traffic
- service-owned filtering and enrichment
- local control-plane inputs for execution
- one reusable runtime across multiple ingress families

The low-latency case assumes the host has early ingress. Without that, SOF can still be useful for
control and architecture, but it will not become earlier than the source feeding it.

## When SOF Is Not The Right Choice

SOF is usually not the first move when the main need is:

- standard RPC reads and transaction history
- the fastest possible path to consuming a hosted stream with no interest in owning the runtime
- the absolute earliest traffic with no interest in SOF's runtime surface
- validator participation and the full validator data surface
- a completely custom ingest runtime with no desire to adopt an existing runtime model
