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
