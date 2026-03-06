# Derived-State Extension Contract

This document is a working contract sketch for official stateful extensions built on top of
`sof`.

It complements ADR-0009 by classifying the current runtime/plugin surfaces and identifying
which parts are usable for deterministic derived state versus best-effort observation.
The target dedicated feed shape is specified in `derived-state-feed-contract.md`.

## Purpose

Stateful official extensions such as:

1. a local bank,
2. geyser-like local streams,
3. account or program indexes,
4. slot-aware invalidation engines,

need stronger guarantees than a normal plugin.

This document defines the current contract baseline and the gaps that must be closed before
those systems can claim deterministic replay and rollback semantics.

## Surface Classification

### `ObserverPlugin`

Best for:

1. decoded-event observation,
2. metrics,
3. logging,
4. lightweight classification,
5. side-effect systems that can tolerate best-effort delivery.

Current limits:

1. async bounded queue delivery,
2. drop-on-pressure behavior to protect ingest latency,
3. no dedicated replay contract,
4. no dedicated per-consumer persistence or rewind protocol.

Conclusion:

- useful for prototyping stateful systems,
- not sufficient by itself as the long-term official derived-state substrate.

### `RuntimeExtension`

Best for:

1. runtime-managed resources,
2. external connectors,
3. filtered packet/resource observation,
4. capability-guarded network integrations.

Current limits:

1. packet/resource oriented, not decoded-state oriented,
2. startup manifests and packet filters are about runtime ownership, not replay semantics,
3. no transaction/slot/reorg/state contract.

Conclusion:

- useful for resource and transport integration around a stateful extension,
- not the right substrate for deterministic bank/geyser-like state feeds.

### Dedicated Derived-State Substrate

Target role:

1. explicitly ordered decoded-state feed,
2. replay-aware and rollback-aware,
3. stable metadata contract for stateful consumers,
4. bounded dispatch with visible lag/drop semantics,
5. designed for snapshot + replay systems.

Conclusion:

- this is the preferred direction for official stateful extensions.

## Current Event Matrix

The matrix below classifies current event families as they exist today.

### Observational Events

#### `on_raw_packet`

- Ordering value: low
- Replay value: low
- Rollback value: none
- Stateful suitability: poor
- Reason: packet-level ingress visibility only; too early and too lossy/noisy for deterministic state

#### `on_shred`

- Ordering value: low to medium
- Replay value: medium
- Rollback value: none by itself
- Stateful suitability: poor
- Reason: protocol-level parsing signal, but still too low-level for most state materialization

#### `on_dataset`

- Ordering value: medium
- Replay value: medium
- Rollback value: indirect only
- Stateful suitability: limited
- Reason: more structured than shreds, but still intermediate rather than canonical state output

#### `on_recent_blockhash`

- Ordering value: low
- Replay value: low
- Rollback value: none
- Stateful suitability: poor
- Reason: useful runtime signal, not a state backbone

#### `on_cluster_topology`

- Ordering value: medium
- Replay value: medium
- Rollback value: limited
- Stateful suitability: domain-specific only
- Reason: useful for network/provider state, not transaction/account state

#### `on_leader_schedule`

- Ordering value: medium
- Replay value: medium
- Rollback value: limited
- Stateful suitability: domain-specific only
- Reason: useful for routing/control-plane logic, not bank-like state

### Candidate Derived-State Inputs

#### `on_transaction`

- Ordering value: high
- Replay value: potentially high
- Rollback value: partial without explicit revocation model
- Stateful suitability: strong candidate
- Required additions:
  - canonical ordering contract
  - replay format/source
  - revocation semantics under reorg

#### `on_account_touch`

- Ordering value: derived from transaction ordering
- Replay value: potentially high
- Rollback value: partial without explicit revocation model
- Stateful suitability: supporting signal only
- Required additions:
  - must remain secondary to transaction/slot truth
  - cannot be treated as a post-write account update stream

#### `on_slot_status`

- Ordering value: high
- Replay value: high if persisted
- Rollback value: high
- Stateful suitability: required companion signal
- Required additions:
  - explicit status transition rules for extensions
  - clear guidance on provisional/confirmed/finalized outputs

#### `on_reorg`

- Ordering value: high
- Replay value: high if persisted
- Rollback value: high
- Stateful suitability: required companion signal
- Required additions:
  - required extension response model: rewind, revoke, or rebuild

## Minimum Contract for Official Stateful Extensions

An official stateful extension should not rely on a single event type.

The minimum useful contract is likely:

1. ordered transaction feed,
2. ordered slot-status feed,
3. ordered reorg feed,
4. optional derived account-touch side channel,
5. explicit checkpoint/replay model.

That implies a future substrate with a feed shaped more like:

1. `TransactionApplied`
2. `SlotStatusChanged`
3. `BranchReorged`
4. optional `AccountTouchObserved`
5. replay/checkpoint metadata

The exact names are not the decision here. The requirement is the shape:

1. deterministic ordering,
2. explicit branch semantics,
3. replayability,
4. bounded delivery behavior.

## Required Consumer Semantics

Before an official local bank or geyser-like extension is merged, it must document:

1. what it treats as provisional,
2. when outputs become confirmed,
3. when outputs become finalized,
4. how it revokes or supersedes previously emitted outputs on reorg,
5. how it rebuilds after crash or schema migration,
6. what it does if the feed drops events or falls behind.

## Current Recommendation

For now:

1. keep stateful official extensions outside core,
2. allow prototyping on top of `ObserverPlugin`,
3. treat that prototype as contract-discovery work, not as the final substrate,
4. design a dedicated derived-state feed once the required semantics are validated.
