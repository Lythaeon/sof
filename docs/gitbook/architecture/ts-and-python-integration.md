# TypeScript and Python Integration

This document defines how SOF should expose TypeScript and Python authoring without moving SOF's
runtime-critical work out of Rust.

The target is simple:

- developers keep SOF's compiled ingest, reconstruction, queueing, replay, and observability model
- developers can write custom business logic in a simpler language
- the foreign-language boundary stays explicit, typed, and measurable

## Spec Status

This page is intended to be implementation-driving.

Interpretation rules:

- `MUST` means required for the first supported production design
- `SHOULD` means strongly preferred unless a measured reason says otherwise
- `MAY` means optional

If a later implementation conflicts with this document, the implementation should not silently win.
Either the implementation must be changed or this document must be revised deliberately.

## Core Position

SOF should not run TypeScript or Python on the ingest hot path.

Rust remains responsible for:

- runtime setup and lifecycle
- static plugin registration
- hot-path transaction/account preclassification
- queue ownership and backpressure policy
- replay, checkpoint, and recovery mechanics
- runtime extension resource ownership

TypeScript and Python should attach only to owned, bounded, runtime-managed surfaces.

That means:

- cheap borrowed classifiers stay in Rust
- async plugin callbacks may cross the language boundary
- runtime extensions may expose owned packet/message events to foreign workers
- replay-safe state stays on the derived-state contract, not the observational plugin contract

## Integration Surfaces

SOF already has the right three surfaces. The TS/Python model should map directly onto them.

### 1. Plugin Workers

This is the first and most important integration target.

Use it for:

- transaction callbacks
- transaction-log callbacks
- transaction-status callbacks
- account updates
- slot, reorg, blockhash, topology, and leader-schedule events

The Rust host owns `PluginConfig`, compiles prefilters, applies commitment selection, and forwards
owned events to the TS/Python worker through a bounded queue.

### 2. Runtime Extension Workers

Use this when a TS/Python component needs runtime-managed IO but should not own sockets directly.

Rust still owns:

- UDP listeners
- TCP listeners/connectors
- WebSocket connectors
- filtered packet subscriptions

The foreign worker receives owned packet/message events and returns typed commands or typed results.

### 3. Derived-State Workers

This is separate from plugin workers.

Use it only when the user needs:

- deterministic ordering
- checkpointing
- replay recovery
- rollback-aware local state

The Rust host must continue to own replay orchestration. A TS/Python derived-state consumer is a
worker behind a Rust `DerivedStateConsumer` adapter, not a bypass around it.

## Production Readiness Bar

The integration is not production-ready until all of the following are true:

- the wire protocol is versioned and compatibility-checked at startup
- the SDK contract is generated from one Rust source of truth
- queue overflow, worker crash, timeout, and compatibility faults are observable through metrics and logs
- worker startup and shutdown semantics are deterministic
- no hot-path classification depends on TS/Python execution
- `Result` handling and enum usage are enforced by SDK design and tooling
- typed configuration exists for both runtime and `sof-tx` flows
- the docs include enough detail to implement the bridge without inventing incompatible behavior

## Type Contract

The foreign-language API must not become stringly typed.

### Result-First API

Every host-facing SDK operation and every user-defined callback return must use an explicit
`Result<Value, Error>` shape.

Rules:

- success returns `Ok(value)`
- failure returns `Err(error)`
- domain failures are returned, not thrown
- exceptions crossing the boundary are converted into a typed `UnhandledException` error variant
- callers must inspect the result before accessing the value

This rule exists to keep failure handling visible across the language boundary. Silent exception
paths are too easy to miss once runtime logic is split across Rust and TS/Python.

### Checked Result Requirement

The SDK contract should treat ignored results as invalid usage.

Required direction:

- TypeScript: SDK returns a concrete `Result<T, E>` type and linting rejects floating or ignored
  results
- Python: SDK returns `Ok[T] | Err[E]` wrappers and linting or review gates reject ignored results
- host bridge: no callback return is treated as an implicit success value

The important property is not the exact lint tool. The important property is that a developer has
to check the result intentionally.

### Typed Enums Only

Protocol states, commands, capabilities, hook kinds, commitments, dispatch classes, and error kinds
must be enums, not free-form strings.

Rules:

- Rust enums are the source of truth
- generated TS/Python bindings mirror those enums
- wire encoding uses stable numeric discriminants
- user code switches on enums, not raw strings
- unknown discriminants are decoded into a typed compatibility error

This avoids fragile `"confirmed"`, `"transaction"`, `"critical"`, `"timeout"`, or `"retry"`
string contracts that drift over time.

### Typed Value Objects Where Strings Exist Today

Some payloads are textual by nature, but they still should not be passed around as anonymous
`string` or `str` values when they carry domain meaning.

Examples:

- signatures
- pubkeys
- source instance ids
- plugin names

Those should be wrapped in typed value objects in TS/Python even when their serialized form is
textual.

### No Silent Fallbacks

The bridge MUST not silently coerce typed protocol data into weaker forms.

Disallowed behavior:

- unknown enum discriminant mapped to a default string
- invalid config field silently ignored
- unsupported capability downgraded without a typed warning or typed failure
- callback exception treated as a success with an empty value
- malformed worker reply treated as `None`, `null`, or implicit `Ok`

If the bridge cannot preserve the typed contract, it MUST return a typed error or fail startup.

## Foreign-Language Authoring Model

The foreign-language worker should declare a static manifest that Rust consumes at startup.

The manifest should include:

- plugin or worker name
- subscribed surfaces
- commitment policy
- declared capabilities
- declarative prefilters
- versioned protocol compatibility

The manifest must remain static for the process lifetime. SOF should not add or remove foreign
workers dynamically once the runtime starts.

## Lifecycle Contract

The worker lifecycle MUST match SOF's host lifecycle model.

Required behavior:

1. Rust loads and validates the manifest before the runtime enters the main loop.
2. Rust performs protocol compatibility negotiation before event delivery starts.
3. Rust starts the worker and waits for an explicit ready result.
4. Only after readiness succeeds may event delivery begin.
5. On shutdown, Rust stops event intake for that worker, drains or drops according to documented
   lane policy, requests worker shutdown, and records the outcome.

Startup failure MUST fail the worker registration. It must not leave the runtime in a partially
attached or ambiguous state.

## SDK Packaging Model

The TypeScript and Python SDKs should be unified SOF SDKs, not crate-for-crate mirrors of the Rust
workspace.

In Rust, the split between `sof`, `sof-tx`, `sof-types`, and other crates is a repository and
compilation concern. That split is useful for Rust, but it should not be imposed on TS/Python
users.

For each foreign language, the default user-facing package should be one SDK:

- one TypeScript SDK package
- one Python SDK package

That SDK should include all of the main SOF authoring surfaces:

- runtime/plugin worker APIs
- runtime-extension worker APIs
- derived-state worker APIs when that bridge exists
- `sof-tx` submission, routing, and transport logic
- SOF runtime configuration types
- `sof-tx` configuration and policy types
- shared enums, result wrappers, and typed value objects

The package boundary should optimize for application developers, not for preserving the Rust crate
graph.

### What This Means

Foreign-language users should not have to assemble multiple low-level packages just to build one
normal SOF service.

The default experience should be:

- import one SDK
- configure observation and submission from one typed surface
- use the same enums, result types, and value objects across runtime and tx flows
- share one versioned protocol and compatibility story

### `sof-tx` In Scope

The foreign-language SDKs should not stop at observational plugin logic.

They should also include the equivalent of the `sof-tx` surface, including:

- transaction-building configuration types
- submission routing policy types
- transport selection and transport configuration
- local control-plane adapter types where applicable
- typed execution and submission results
- typed retry, timeout, and failure policies

That is important because many real services built on SOF both observe and submit. Splitting those
concerns into separate foreign-language packages would recreate friction that the unified SDK should
remove.

### Configuration Types Stay Typed

SOF runtime configuration and `sof-tx` configuration should be first-class typed objects in TS and
Python, not untyped dictionaries and not string-key maps.

Examples:

- plugin subscription config
- provider ingress config
- commitment selectors
- submission route config
- retry policy config
- source arbitration config

These config objects should use generated enums and typed fields so that runtime and submission
configuration follow the same checked contract as events and results.

## Protocol and Wire Format

The bridge protocol MUST be explicitly versioned.

Minimum required protocol fields:

- protocol version
- sdk language
- sdk version
- manifest version
- worker kind
- capability set
- enum discriminants used by the message family

Wire-level rules:

- enum discriminants MUST be stable numeric values
- messages MUST be self-describing enough to reject incompatible peers deterministically
- field removal or semantic redefinition requires a protocol version change
- additive optional fields MAY be introduced in a backward-compatible minor revision

The implementation MAY choose JSON, MessagePack, CBOR, or another encoding for the initial bridge,
but the encoding must preserve typed discriminants and deterministic decoding behavior.

Human readability is not a reason to weaken type fidelity.

## Prefilter Model

TypeScript and Python must not provide arbitrary hot-path callbacks for transaction filtering.

Instead, they should provide a declarative filter DSL that Rust compiles into native prefilters.

Examples of allowed declarative filter inputs:

- exact signature match
- account include/exclude
- required account set
- commitment threshold
- transaction family or kind

If a requested filter cannot be compiled into the native Rust hot path, startup should fail rather
than silently falling back to a slow path.

## Submission and `sof-tx` Contract

The unified foreign-language SDK MUST cover both observation and submission.

Minimum `sof-tx` parity goals for the foreign SDKs:

- typed transaction-building inputs
- typed route and transport selection
- typed submission configuration
- typed execution outcome and submit outcome surfaces
- typed retry and timeout policy surfaces
- typed control-plane adapter configuration where SOF provides it

The first foreign-language SDK release does not need to reproduce every Rust helper immediately, but
it MUST not define submission around free-form dicts, strings, or implicit exceptions.

## Runtime and Backpressure Model

The foreign-language bridge must preserve SOF's existing runtime discipline.

Rules:

- Rust owns the queue
- queue capacity is bounded
- overflow is explicit and measurable
- drop policy is documented per lane
- worker failures become typed host-visible faults
- readiness and health expose foreign worker status

The bridge should not hide slow TS/Python logic behind unbounded buffering.

## Observability Contract

The bridge MUST expose enough observability to operate it in production.

Minimum required signals:

- worker startup success and failure counts
- worker crash counts
- protocol compatibility failures
- queue depth per worker lane
- queue overflow or dropped-event counts
- event handling latency distribution
- worker restart counts if restarts are supported
- readiness and health state for each worker

Logs should remain human-readable, but metrics and machine-consumable status must carry the typed
error kind as well.

## Error Model

Errors returned across the boundary should be a closed typed hierarchy.

Initial categories:

- `ProtocolError`
- `CompatibilityError`
- `TimeoutError`
- `DecodeError`
- `ValidationError`
- `CapabilityError`
- `QueueOverflowError`
- `WorkerCrashedError`
- `UnhandledException`

Each category should be an enum or typed error variant, not an ad hoc string message. Human-readable
messages may exist for logs, but the programmatic contract is the typed kind plus typed fields.

## Compatibility Rules

Compatibility must be explicit.

Required rules:

- startup MUST fail on unsupported major protocol mismatch
- startup MUST fail when required capabilities are missing
- startup MUST fail when manifest-declared surfaces cannot be honored
- startup MAY continue on additive minor-version differences if both sides declare compatibility
- runtime MUST emit a typed compatibility fault when a later message violates the negotiated contract

`warn and continue` is acceptable only for explicitly optional capabilities that the manifest marked
as optional.

## Recommended SDK Shapes

### TypeScript

The TS SDK should expose a discriminated result type backed by enums, not string literal unions.

```ts
export enum ResultTag {
  Ok = 1,
  Err = 2,
}

export type Result<T, E> =
  | { tag: ResultTag.Ok; value: T }
  | { tag: ResultTag.Err; error: E };
```

All externally visible states should be enums generated from the Rust protocol crate.

### Python

The Python SDK should expose explicit wrappers plus `Enum` or `IntEnum` values.

```python
from dataclasses import dataclass
from enum import IntEnum
from typing import Generic, TypeVar

T = TypeVar("T")
E = TypeVar("E")

class ResultTag(IntEnum):
    OK = 1
    ERR = 2

@dataclass(frozen=True)
class Ok(Generic[T]):
    value: T

@dataclass(frozen=True)
class Err(Generic[E]):
    error: E
```

Foreign callbacks should return `Ok(value)` or `Err(error)`, never bare strings and never ad hoc
exception text as the domain contract.

## Non-Goals

This plan does not propose:

- rewriting SOF's runtime in TS or Python
- direct foreign-language hot-path classifiers
- dynamic plugin loading after runtime start
- string-based hook or capability negotiation
- exceptions as the normal domain error model

## Acceptance Criteria

An implementation should not be called complete until it satisfies all of these checks:

- one generated protocol source defines enums, result wrappers, and message DTOs
- TS and Python each ship one unified SDK including runtime and `sof-tx` surfaces
- examples compile or type-check in both foreign languages
- ignored result handling is rejected by SDK guidance and lint/type-check defaults
- enum-only protocol paths exist for hook kinds, commitments, capabilities, and error kinds
- worker startup, crash, timeout, and shutdown behavior is covered by tests
- queue overflow behavior is measured and documented
- docs build succeeds with the new page included

## Next Plan

The next implementation plan should be staged in this order.

### Phase 1: Protocol and Type Contract

- create one Rust protocol crate for shared DTOs, enums, result tags, and typed error kinds
- define stable numeric discriminants for all enums
- define TS and Python code generation from the Rust protocol source of truth
- document the checked-result requirement as a non-optional SDK rule
- define one unified SDK packaging model per foreign language instead of mirroring the Rust crate split

### Phase 2: Async Plugin Bridge

- implement one Rust plugin bridge host for owned async events only
- compile declarative foreign-language prefilters into Rust `PluginConfig` and native prefilters
- expose bounded queue metrics, worker health, and typed startup failures
- reject non-compilable prefilters at startup
- implement startup compatibility negotiation and deterministic worker readiness gating

### Phase 3: TypeScript SDK

- ship one unified SOF TS SDK package, not separate `sof` and `sof-tx` packages
- add manifest authoring
- add generated enums and result wrappers
- add typed SOF runtime config and `sof-tx` config objects
- add `sof-tx` routing, transport, and submission surfaces on the same SDK
- add lint rules that reject ignored results
- add examples for transaction, status, and account callbacks

### Phase 4: Python SDK

- ship one unified SOF Python SDK package, not separate `sof` and `sof-tx` packages
- add manifest authoring
- add generated enums and result wrappers
- add typed SOF runtime config and `sof-tx` config objects
- add `sof-tx` routing, transport, and submission surfaces on the same SDK
- add linting and type-check guidance that rejects ignored results
- add examples for transaction, status, and account callbacks

### Phase 5: Extension and Derived-State Bridges

- add runtime-extension worker support on top of Rust-owned resources
- add a Rust-owned derived-state adapter for replay-safe TS/Python consumers
- keep replay and checkpoint orchestration in Rust

### Phase 6: Production Hardening

- add compatibility and failure-injection tests
- add queue overflow and worker-crash recovery tests
- add docs and examples for submission plus observation in one service
- document support policy and versioning guarantees for the foreign SDKs

## Decision Summary

If SOF adds TS and Python, the contract should be:

- Rust owns the runtime-critical path
- TS/Python run as bounded workers on owned surfaces
- each foreign language gets one unified SOF SDK that also includes the `sof-tx` surface
- every cross-language operation returns an explicit checked result
- enums replace stringly typed states and commands
- startup stays static and declarative
- replay-safe state remains on the derived-state model
