Current provider-stream audit findings to fix on `feat/provider-multi-source-fanin`:

1. Built-in provider configs do not expose stable user-controlled source instance labels.
   - Current built-in spawns always use generated runtime labels.
   - Operators need durable names for health/metrics and redundant source intent.

2. Provider readiness is still too strict for heterogeneous fan-in.
   - Readiness groups by source kind now, but every observed kind is still treated as required.
   - We need a required vs optional readiness class on provider sources.

3. Provider docs are stale.
   - Observer README still says built-in processed providers remain transaction-first.
   - GitBook pages still describe the old narrower built-in surface.

Implementation plan:
- Add typed provider source readiness metadata.
- Extend built-in provider configs with:
  - `with_source_instance(...)`
  - `with_readiness(...)`
- Default auxiliary built-in feeds to optional readiness where appropriate.
- Propagate readiness through provider health events and observability.
- Update stale observer and GitBook docs to describe the richer built-in surface accurately.

Follow-up findings after the readiness/readiness-class pass:

4. Required-source readiness still grouped by `ProviderSourceId`.
   - Two distinct required sources of the same kind can mask each other.
   - Readiness should require each required source instance to be healthy.

5. Observer README provider docs drifted after the richer built-in mode expansion.
   - Built-in runtime mode surface list was incomplete.
   - Generic typed update list omitted `BlockMeta`.
   - One duplicated LaserStream bullet block slipped into the readiness section.

6. GitBook crate page still described built-in processed providers as transaction-first.
   - That now misstates the built-in surface after accounts, transaction status,
     block-meta, logs, program/account websocket feeds, and slots landed.

7. Built-in source readiness still had a startup registration gap.
   - Required sources only existed in readiness state after their first emitted
     health event.
   - A fast healthy source could briefly make `/readyz` true before another
     configured required source had registered itself.

8. Generic serialized transaction replay dedupe depended on an explicit signature.
   - If a producer fed `SerializedTransaction` without `signature`, replay
     dedupe silently stopped applying to that update family.

9. In-code provider adapter module docs drifted.
   - Websocket module docs still described only transaction adapters.
   - Yellowstone config docs still described only transaction subscriptions.

10. Built-in initial source registration could block startup on a full queue.
   - Initial health registration was sent before `tokio::spawn`.
   - In multi-source startup, that could block a later source spawn before the
     runtime had begun draining the provider queue.

- 2026-03-27 follow-up: fixed stale GitBook wording that still described built-in processed providers as transaction-first; clarified that built-ins now cover typed transaction, transaction-status, account, block-meta, log, and slot feeds where supported, while `ProviderStreamMode::Generic` remains the path for custom typed producers, multi-source fan-in, and richer control-plane feeds.

- 2026-03-27 follow-up: built-in source registration still happened after first session bootstrap, fan-in still allowed duplicate full source identities, and built-in spawn API names remained transaction-only even though config-selected streams now include accounts, block-meta, transaction-status, logs, and slots.

- 2026-03-28 follow-up: startup-failure ghost health still needed an explicit
  source-lifecycle cleanup, fan-in reservations needed to stay owned until
  source/task shutdown for both built-in and generic sender paths, and the old
  public spawn names needed compatibility shims instead of a silent semver
  break.

- 2026-03-28 follow-up: generic reserved senders still needed to bind every
  emitted update to their reserved source identity, built-in source tasks still
  needed to prune themselves on normal stop/abort instead of only on startup
  failure, and public docs/metrics needed to spell out that `Removed` is a
  pruning control event rather than a persistent health state.

- 2026-03-28 follow-up: generic reserved sources still needed to emit
  `Removed` on last-drop so custom producers do not leave stale health behind,
  generic readiness docs needed to say explicitly that per-source readiness
  begins only after custom health events arrive, and the remaining non-test
  `dead_code` suppressions on baseline/provider helper functions needed to be
  removed by tightening cfg boundaries instead of silencing clippy.

- 2026-03-28 follow-up: deferred `Removed` delivery under queue pressure still
  needed to keep source identities reserved until the terminal removal event is
  actually queued, generic last-drop cleanup still needed a no-runtime path
  that cannot panic during teardown, and the GitBook crate page still needed
  the same explicit generic-readiness caveat that already existed in the
  observer README.

- 2026-03-28 follow-up: process-global prune tombstones were removed; built-in
  websocket / Yellowstone / LaserStream startup now queues initial
  `InitialConnectPending` health non-blockingly before first bootstrap and
  flushes it from the spawned task to preserve ordering without blocking spawn
  on a full queue. Existing reservation-lifecycle tests were tightened to check
  eventual identity reuse after delayed `Removed` delivery instead of assuming
  same-tick release timing.
- Fixed reserved generic sender lifecycle gaps:
  - live reserved senders now reject `Health(Removed, ...)` so callers cannot prune a source while its reservation is still active
  - no-runtime full-queue generic shutdown now uses bounded retry and then releases the identity instead of risking an unbounded reservation leak
