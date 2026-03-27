# SOF Compared To The Usual Alternatives

SOF is usually evaluated against one of four things:

- an RPC-first application
- a managed stream or Geyser provider
- a self-operated validator with Geyser
- another self-operated ingest stack

The goal here is a factual comparison that is fast to scan.

One positioning note matters across every comparison on this page:

SOF is only faster when its ingress is also early.

SOF removes external API and stream-layer overhead between your application and the observed
traffic. It does not remove the upstream visibility race. If your host sees shreds late, SOF will
still start late. The strongest SOF deployments combine:

- early ingress, such as direct low-latency validator or peer access, or an external shred
  propagation network
- local processing and local consumers on the same system

That creates two raw-shred SOF trust modes plus one different provider-stream category:

- `public_untrusted`: public gossip/direct peers, verification on, highest independence
- `trusted_raw_shred_provider`: trusted raw shred distribution, usually the fastest SOF mode
- `processed_provider_stream`: Yellowstone, LaserStream, websocket, or other processed feeds;
  useful, but not the same raw-shred observer model and not a `SOF_SHRED_TRUST_MODE` value

## Quick Comparison

| Option | Primary data path | Operational model | Typical fit |
| --- | --- | --- | --- |
| RPC-first app | RPC responses | consume a standard API from your own or someone else's RPC endpoint | account reads, transaction history, general application backends |
| Managed stream / Geyser provider | provider-managed validator or stream infrastructure | subscribe to a hosted stream product | teams that want stream consumption without operating ingest infrastructure |
| Self-operated validator with Geyser | your own validator and plugin pipeline | operate validator-shaped infrastructure yourself | teams that already need validator-level data access or want full validator-adjacent control |
| SOF | live observed network traffic | operate a focused observer/runtime host | low-latency analytics, monitoring, automation, custom streams, and execution-adjacent services |

Another way to read the table:

- RPC/provider-first architectures optimize for consuming an already-productized API
- SOF optimizes for owning one efficient runtime foundation and letting your application code sit
  directly on top of it

## SOF vs RPC

| Question | RPC-first | SOF |
| --- | --- | --- |
| Data source | RPC server responses | live observed Solana traffic |
| Integration shape | request/response | runtime ingest and event flow |
| Control over parsing and retention | limited to the RPC/API surface | owned by the embedding service |
| Control-plane inputs such as recent blockhash or leader context | external | can be locally derived |
| Operational effort | lower | higher |

RPC is usually the simpler fit when a standard read surface is enough.

SOF is the better fit when the service needs the live traffic path itself rather than only an API
surface built on top of it.

That advantage is largest when the SOF host has earlier traffic visibility than the RPC path you
would otherwise consume.

## SOF vs Managed Stream Or Geyser Providers

This is the comparison most external consumers actually make. In practice, many teams do not
consume Geyser by running validators themselves. They consume a hosted product built on top of that
infrastructure.

| Question | Managed stream / Geyser provider | SOF |
| --- | --- | --- |
| Who operates ingest? | provider | you |
| Time to first integration | shorter | longer |
| Stream schema and product surface | provider-defined | service-defined |
| Filtering and enrichment | limited to what the provider exposes | implemented by your own service |
| Cost model | subscription or usage pricing | your own host and network cost |

Managed providers are a strong fit when the main goal is to consume streams quickly.

SOF is a stronger fit when the goal is to own the ingest path, filtering logic, runtime behavior,
and downstream product surface.

It is also a stronger fit when you do not want every Solana service in your stack to separately
rebuild:

- provider adapters
- filter parity across providers
- reconnect and backoff logic
- low-level performance work around allocations, copies, SIMD, and cache behavior
- correctness boundaries such as duplicate suppression, replay, and verification posture

It is also the stronger latency fit when you can put SOF on a host with better shred visibility
than the hosted stream consumer path you would otherwise depend on.

The strongest practical version of that is often:

- SOF fed by a trusted raw shred distribution network
- instead of SOF waiting on the public gossip edge

## SOF vs Self-Operated Validator With Geyser

This is a different comparison from using a hosted Geyser provider.

| Question | Self-operated validator + Geyser | SOF |
| --- | --- | --- |
| Core mission | validator participation plus plugin output | focused observation, reconstruction, and local state derivation |
| State and infrastructure footprint | larger | smaller |
| Operational complexity | higher | lower |
| Best for | validator operators and heavy validator-adjacent systems | focused backend services that need live traffic and local state |

SOF is not a validator replacement. It is a narrower runtime for a narrower job.

One nuance matters:

- validator or Geyser paths are already after validator-side verification and processing
- SOF on public gossip keeps more independence, but pays more observer-side verification cost
- SOF on a trusted raw shred provider is the middle ground: shred-native runtime behavior without
  paying the full public-gossip cost

## SOF vs Another Self-Operated Ingest Stack

If the comparison is not RPC or Geyser at all, it usually comes down to control and implementation
focus.

| Question | Other custom ingest stack | SOF |
| --- | --- | --- |
| Build vs buy inside your own stack | build and maintain the full ingest/runtime surface yourself | start from an existing Solana-focused runtime |
| Solana-specific runtime features | depends on your implementation | built in |
| Ability to customize | maximum | high, within SOF's runtime model |
| Time to production | depends on your team and scope | usually shorter |

The biggest difference is usually not just time to first prototype. It is how much low-level work
you avoid repeating across every service:

- tuning for lower instructions and lower cache waste
- cutting allocator/copy churn in provider and packet paths
- pressure controls and bounded worker behavior
- robust reconnect / replay / restart behavior
- consistent plugin/filter semantics across ingress modes

## When SOF Is The Right Choice

SOF is a good fit for services that need something like:

- low-latency analytics or monitoring from live traffic
- execution services that want local control-plane inputs
- custom stream products with service-owned filtering and enrichment
- direct control over ingest posture without operating a full validator stack

The low-latency case assumes the host has early ingress. Without that, SOF can still be useful for
control and architecture, but not magically earlier than the traffic source feeding it.

## When SOF Is Not The Right Choice

SOF is usually not the first move when the main need is:

- standard RPC reads and transaction history
- the fastest possible path to consuming a hosted stream
- validator participation and the full validator data surface
- a completely custom ingest runtime with no desire to adopt an existing runtime model
