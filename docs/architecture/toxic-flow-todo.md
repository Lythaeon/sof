# Toxic-Flow Reduction Todo

This file tracks the next substrate improvements needed so services built on top of SOF do not
have to hand-roll stale-input guards, invalidation handling, and low-confidence routing policy.

## Best Improvements

1. Add first-class freshness metadata everywhere.
   - event age
   - slot age
   - source timestamp skew
   - blockhash age
   - topology age
   - leader-schedule age

2. Add confidence and quality classification to the derived-state feed.
   - `provisional`
   - `stable`
   - `reorg-risk`
   - `stale`
   - `degraded`
   - `incomplete-control-plane`

3. Add feed-level consistency guards.
   - do not emit strategy-safe signals until required inputs are aligned
   - recent blockhash, topology, leader schedule, and tip watermark should be policy-checkable as
     one coherent control-plane snapshot

4. Add replay and reorg-aware invalidation primitives.
   - explicit revocation events
   - invalidation of derived opportunities or state views after reorg

5. Add source attribution and conflict tracking.
   - source-of-truth metadata
   - winner/loser source selection
   - conflict visibility when two control-plane sources disagree

6. Add tx-outcome feedback into the same substrate.
   - landed
   - expired
   - dropped
   - leader missed
   - blockhash stale
   - unhealthy route

7. Add built-in suppression keys.
   - signature-level
   - opportunity-level
   - account-set-level
   - slot-window-level

8. Add state-drift guards for services using local banks.
   - decision state version
   - send-time state version
   - policy for rejecting or downgrading drifted decisions

9. Add typed safety and guard policy objects in `sof-tx`.
   - minimum freshness
   - maximum slot lag
   - require stable topology
   - require leader confidence
   - suppress on replay recovery pending

10. Add toxic-flow telemetry.
    - `rejected_due_to_staleness`
    - `rejected_due_to_reorg_risk`
    - `rejected_due_to_state_drift`
    - `submit_on_stale_blockhash`
    - `leader_route_miss_rate`
    - `opportunity_age_at_send`

## Current Implementation Slice

This branch now covers three concrete pieces:

1. Observer-side control-plane freshness and quality classification in the derived-state feed
2. Typed tx-provider control-plane safety policy and freshness metadata in `sof-tx`
3. Typed gossip/runtime tuning integration in `sof`

That still does not finish the full toxic-flow roadmap, but it removes several immediate sources of
downstream boilerplate:

- ad hoc observer-side freshness and quality classification for recent blockhash, topology, and leader schedule inputs
- ad hoc tx-provider freshness guards around control-plane state
- ad hoc numeric/string tuning overlays for gossip bootstrap and ingest

Implemented now:

- item 1: partial
  - per-input freshness metadata for recent blockhash, topology, and leader schedule
- item 2: partial
  - coarse `stable` / `degraded` / `stale` / `incomplete-control-plane` classification
- item 3: partial
  - coherent control-plane spread checks
- item 9: expanded
  - typed tx-provider guard policy, freshness snapshots, and safety reports

Still open:

- explicit invalidation and replay/reorg-aware revocation
- tx outcome feedback
- suppression keys
- state-drift guards
- toxic-flow counters/telemetry
