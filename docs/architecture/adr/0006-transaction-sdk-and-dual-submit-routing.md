# ADR-0006: Transaction SDK and Dual Submit Routing

- Status: Implemented
- Date: 2026-02-17
- Decision makers: `sof-observer` maintainers
- Related: `docs/architecture/framework-plugin-hooks.md`, `docs/architecture/runtime-bootstrap-modes.md`

## Context

SOF currently focuses on ingest, decode, and observation. Transaction creation and submission remain verbose for users who need:

1. ergonomic transaction construction,
2. low-latency leader-aware submission, and
3. portability outside SOF runtime environments.

Current Solana client workflows are RPC-centric and can be too rigid for latency-sensitive or redundancy-driven send paths. We need a dedicated SDK crate that supports both standard RPC workflows and direct leader routing.

## Decision

Create a new workspace crate dedicated to transaction authoring and submission, tentatively named `sof-tx`.

The crate will provide:

1. Transaction creation abstraction (less boilerplate around message assembly/signing).
2. A unified submit interface with runtime mode selection:
   - `rpc` mode (standard JSON-RPC submit),
   - `direct` mode (leader-targeted submit),
   - `hybrid` mode (direct first with configurable fallback behavior).
3. Leader-aware fanout policy for:
   - current leader,
   - next scheduled leaders,
   - optional backup validators.
4. A pre-signed transaction submit path for users who build/sign transactions themselves.

## Goals

1. Keep tx construction simple and explicit for newcomers.
2. Preserve low-latency paths for advanced users.
3. Make SDK usable inside and outside SOF runtime.
4. Avoid forcing direct networking dependencies on RPC-only users.
5. Keep hot paths non-blocking and bounded.
6. Accept externally signed transactions without forcing re-build or re-sign.

## Non-Goals (v1)

1. Kernel bypass implementation in first release.
2. FPGA acceleration in first release.

## Proposed Crate Shape

### Public modules

1. `builder`:
   - High-level transaction/message builder APIs.
   - Compute budget, priority fee, and account metas helpers.
2. `signing`:
   - Signer trait adapters and offline/remote signing boundaries.
3. `routing`:
   - Leader schedule and backup target selection.
   - Fanout and dedupe policy (`signature` keyed).
4. `submit`:
   - `TxSubmitClient` + `SubmitMode`.
   - Response aggregation and first-success semantics.
5. `providers`:
   - `RecentBlockhashProvider`.
   - `LeaderProvider`.

### Runtime interfaces

```rust
pub enum SubmitMode {
    RpcOnly,
    DirectOnly,
    Hybrid,
}

pub struct RoutingPolicy {
    pub next_leaders: usize,
    pub backup_validators: usize,
    pub max_parallel_sends: usize,
}
```

```rust
pub trait RecentBlockhashProvider {
    fn latest_blockhash(&self) -> Option<[u8; 32]>;
}

pub trait LeaderProvider {
    fn current_leader(&self) -> Option<LeaderTarget>;
    fn next_leaders(&self, n: usize) -> Vec<LeaderTarget>;
}
```

```rust
pub enum SignedTx {
    VersionedTransactionBytes(Vec<u8>),
    WireTransactionBytes(Vec<u8>),
}

impl TxSubmitClient {
    pub async fn submit_signed(
        &self,
        signed_tx: SignedTx,
        mode: SubmitMode,
    ) -> Result<SubmitResult, SubmitError>;
}
```

SOF integration adapters can map these providers to:

- `PluginHost::latest_observed_recent_blockhash()`
- `PluginHost::latest_observed_tpu_leader()`
- gossip-derived leader schedule snapshots

## Feature Flags

Default should remain minimal for broad compatibility.

1. `rpc-submit` (default):
   - JSON-RPC submit path.
2. `direct-submit` (optional):
   - direct leader/validator send path.
3. `hybrid-submit` (optional):
   - orchestration between direct and RPC submit.
4. `gossip-routing` (optional):
   - leader/topology-aware target resolution from gossip data.

Notes:

- Runtime mode (`SubmitMode`) is a runtime decision.
- Capability flags control compile-time dependency surface.

## Direct Submit Behavior (v1)

1. Event-driven routing updates from leader/topology signals.
2. Bounded fanout only; no unbounded spawn.
3. First-success wins response semantics.
4. Timeout budget per target and global attempt deadline.
5. Duplicate suppression by transaction signature for a configurable window.
6. Accept pre-signed bytes and send without mutating message/signatures.

## RPC Behavior (v1)

1. Standard send path for broad compatibility.
2. Optional preflight toggle.
3. Optional confirmation strategy abstraction (none/processed/confirmed/finalized).
4. Accept pre-signed tx submit path in addition to builder-produced tx path.

## Hybrid Behavior (v1)

1. Submit to direct targets first.
2. Submit to RPC fallback if direct attempts exceed deadline or error policy threshold.
3. Preserve idempotency by signature-level dedupe.
4. Support the same behavior for builder-produced and pre-signed transactions.

## Implementation Plan

1. Phase 1: Scaffolding
   - Add crate with builder/signing/provider traits.
   - Add unit tests for tx assembly correctness.
2. Phase 2: RPC path
   - Implement `rpc-submit`.
   - Add integration tests against a local validator.
3. Phase 3: Direct path
   - Implement `direct-submit` transport abstraction.
   - Add deterministic routing/fanout tests with mocked targets.
4. Phase 4: Hybrid path
   - Add mode orchestration and fallback policy.
   - Add behavior tests for deadline/failure matrices.
5. Phase 5: SOF adapters
   - Bridge observed blockhash/leader providers from `PluginHost`.
   - Add example app showing external SDK usage and SOF-integrated usage.

## Alternatives Considered

1. Keep tx logic inside `sof` crate:
   - Rejected: couples runtime ingest concerns with tx authoring/send concerns.
2. RPC-only helper crate:
   - Rejected: does not satisfy leader-aware direct path goals.
3. Direct-only crate:
   - Rejected: excludes broader user base and migration paths.

## Consequences

Positive:

1. Easier tx authoring for users.
2. Cleaner migration path from RPC-only to low-latency direct send.
3. Reusable SDK outside SOF runtime.

Tradeoffs:

1. More API surface to maintain.
2. More transport-specific testing burden.
3. Need tight guardrails to avoid accidental noisy fanout defaults.

## Migration Plan

1. Introduce crate behind additive workspace change.
2. Keep existing SOF runtime behavior unchanged.
3. Add docs/examples before enabling by default in user-facing paths.

## Rollback Strategy

1. If direct mode proves unstable, keep `rpc-submit` default and mark `direct-submit` experimental.
2. Feature flags allow disabling direct/hybrid capability without breaking RPC-only users.

## Compliance Checks

1. New crate respects slice boundaries and type/error guidelines.
2. Direct/hybrid paths include bounded concurrency and backpressure controls.
3. Integration tests validate behavior under packet loss, timeout, and stale leader data.
4. Documentation includes operational guidance for safe rollout defaults.
