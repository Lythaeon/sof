# Toxic-Flow Substrate Status

This document tracks the toxic-flow reduction substrate shipped in SOF so services built on top of
the derived-state feed and `sof-tx` do not have to reimplement stale-input guards, invalidation,
and low-confidence routing policy.

## Implemented

1. First-class freshness metadata.
   - observer-side control-plane state tracks recent-blockhash, topology, and leader-schedule age
   - control-plane snapshots also carry source wallclock skew and slot spread
   - tx-provider adapters expose freshness through `TxProviderFlowSafetyReport`

2. Confidence and quality classification in the derived-state feed.
   - `ControlPlaneStateUpdated` carries canonical quality state
   - quality now distinguishes `Provisional`, `Stable`, `ReorgRisk`, `Stale`, `Degraded`, and
     `IncompleteControlPlane`

3. Feed-level consistency guards.
   - observer-side control-plane tracking computes `strategy_safe`
   - tx-side guard policy can reject degraded or misaligned control-plane state before submit

4. Replay and reorg-aware invalidation primitives.
   - derived-state feed emits `StateInvalidated`
   - branch reorgs emit invalidation automatically
   - control-plane regressions can emit invalidation when the state becomes unsafe

5. Source attribution and conflict tracking.
   - control-plane snapshots include selected topology and leader-schedule sources
   - conflict and disagreement counters are emitted on the canonical control-plane event

6. Tx-outcome feedback in the same substrate.
   - derived-state feed emits `TxOutcomeObserved`
   - tx-side submit code can report landed, expired, dropped, stale-blockhash, and route-health
     outcomes

7. Built-in suppression keys.
   - `sof-tx` exposes typed suppression keys for repeated submit attempts
   - suppression can be keyed by signature, opportunity, account set, or slot window

8. State-drift guards for services using local banks.
   - `TxSubmitGuardPolicy` can reject submits when decision state version and send-time state
     version drift beyond policy

9. Typed safety and guard policy objects in `sof-tx`.
   - `TxSubmitGuardPolicy`
   - `TxFlowSafetySnapshot`
   - `TxFlowSafetyQuality`
   - `TxToxicFlowRejectionReason`

10. Toxic-flow telemetry.
    - `TxToxicFlowTelemetry`
    - `TxToxicFlowTelemetrySnapshot`
    - rejection counters for stale, degraded, replay-pending, and drifted sends
    - outcome counters for external feedback paths

## Main Surfaces

Observer/runtime side:

- `DerivedStateFeedEvent::ControlPlaneStateUpdated`
- `DerivedStateFeedEvent::StateInvalidated`
- `DerivedStateFeedEvent::TxOutcomeObserved`

Transaction side:

- `TxSubmitGuardPolicy`
- `TxSubmitContext`
- `TxSubmitOutcome`
- `TxSubmitOutcomeReporter`
- `TxToxicFlowTelemetry`
- `TxFlowSafetySource`

## Usage Direction

For services built on top of SOF:

1. source control-plane state from the derived-state feed or the official `sof-tx` derived-state
   adapter
2. evaluate it through the built-in flow-safety surfaces before direct or hybrid submit
3. feed submit outcomes back into the same substrate when the service has authoritative delivery
   signals

This is now an implemented substrate, not a speculative roadmap. Future work should extend the same
typed surfaces instead of introducing parallel downstream policy code.
