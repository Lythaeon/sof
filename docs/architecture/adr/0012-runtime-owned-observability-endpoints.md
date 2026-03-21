# ADR-0012: Runtime-Owned Observability Endpoints

- Status: Implemented
- Date: 2026-03-21

## Context

SOF already emitted structured logs and internal runtime snapshots, but operators still lacked one
simple machine-readable surface for:

- Prometheus scraping
- liveness checks
- readiness checks tied to runtime bootstrap

Pushing this concern into examples or downstream embedders would fragment the operational model and
encourage each service to bolt on its own ad hoc HTTP layer around the runtime.

That would create several problems:

1. runtime lifecycle and readiness semantics would drift across adopters
2. metrics export could bypass the existing runtime composition and observability boundaries
3. examples would not reflect how SOF is actually intended to be operated in production

## Decision

SOF provides one runtime-owned observability endpoint that is:

- optional and disabled by default
- enabled explicitly through runtime config (`RuntimeSetup` / `SOF_OBSERVABILITY_BIND`)
- started and stopped by the packaged runtime entrypoint layer
- backed by existing bounded runtime, extension, and derived-state telemetry surfaces

The endpoint exposes three fixed paths on one bind address:

- `/metrics`
- `/healthz`
- `/readyz`

Readiness is owned by the runtime lifecycle:

- `ready = false` during startup
- `ready = true` after receiver bootstrap completes
- `ready = false` again when shutdown begins

Liveness is owned by endpoint lifecycle:

- `live = true` while the runtime process and observability sidecar are active
- `live = false` once shutdown completes

## Rationale

This keeps observability inside the same composition root that already owns:

- runtime startup ordering
- shutdown behavior
- plugin/extension/derived-state lifecycle

It also preserves ARD-0007 and ARD-0008:

- runtime orchestration remains in infra, not in domain slices
- metrics stay bounded and derived from existing low-overhead telemetry paths

The endpoint intentionally uses one minimal runtime-owned TCP server rather than introducing a full
web framework dependency for three small operational routes.

## Consequences

Positive:

- one consistent operational surface for SOF services
- explicit enablement with no surprise listener by default
- readiness now reflects runtime bootstrap instead of mere process existence
- downstream services can scrape SOF metrics without reimplementing wrappers

Trade-offs:

- the packaged runtime now owns a small additional sidecar task when enabled
- the initial metrics surface is constrained to existing low-cardinality runtime data
- operators still need explicit bind-address planning and firewall policy for the endpoint

## Alternatives Considered

### 1. Example- or embedder-owned metrics servers

Rejected because lifecycle and readiness semantics would drift across integrations and would no
longer reflect the packaged runtime model.

### 2. Always-on observability listener

Rejected because opening an HTTP port is an operational choice, not a safe default. SOF defaults
should not silently bind an extra listener.

### 3. Full web framework dependency

Rejected for now because the required surface is small and a dedicated framework would add
dependency weight without changing the core runtime architecture.
