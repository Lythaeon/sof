# Runtime Bootstrap Modes

This document explains how SOF starts packet ingestion in different build modes and why
the `gossip-bootstrap` separation exists.

## Build-Time Modes

SOF has two runtime capability profiles:

1. Build without `gossip-bootstrap`:
   - Ingestion sources:
     - direct UDP bind (`SOF_BIND`)
     - TCP relay upstreams (`SOF_RELAY_CONNECT`)
   - No Solana gossip bootstrap runtime is created.
   - If `SOF_GOSSIP_ENTRYPOINT` is set, startup returns an explicit configuration error.

2. Build with `gossip-bootstrap`:
   - Supports the same direct UDP and relay paths.
   - Adds gossip bootstrapping from one or more entrypoints.
   - Creates and maintains gossip runtime state and optional repair client.

## What Happens Without `gossip-bootstrap`

When the feature is disabled, SOF still runs fully as an observer framework:

- It starts configured relay inputs first.
- If no relay/gossip input is active, it starts direct UDP listener mode on `SOF_BIND`.
- Packet flow, parse, verify, dataset reconstruction, and plugin callbacks are unchanged.

In short: you lose gossip-discovery/bootstrap capability, not the framework runtime itself.

## Why Keep This Separation

This separation is intentional and useful:

- Smaller dependency surface for non-gossip deployments.
- Faster compile/link cycles when gossip is not needed.
- Lower operational complexity for home-router/proxy relay setups.
- Clearer failure domains: relay/direct-ingest users are not coupled to gossip bootstrap state.
- Better framework ergonomics: consumers choose only the runtime capabilities they need.

## Runtime Selection Order

At startup, SOF evaluates ingest sources in this order:

1. Relay server/client settings.
2. Gossip bootstrap (feature-gated).
3. Direct UDP listener fallback.

This guarantees that at least one ingest path is active when configuration is valid.

## Programmatic Setup

SOF exposes `sof::runtime::RuntimeSetup` for code-driven configuration when callers do not want
to rely only on process environment variables.

- `RuntimeSetup::default()` keeps normal env/default behavior.
- `run*_with_setup(...)` applies setup overrides before runtime bootstrap.
- Plain `run*()` entrypoints continue to use only env/default settings.

## Observability Expectations

SOF emits framework-level startup logs by default (`info` when `RUST_LOG` is unset), including:

- enabled plugins
- selected ingest runtime path(s)
- relay/gossip bootstrap status
- verification configuration

If your environment overrides `RUST_LOG` to `warn` or higher, those startup logs are hidden.

## Shutdown Behavior

Long-lived runtime workers are cooperatively shut down:

- dataset workers receive a shutdown signal
- workers drain already-queued dataset jobs
- worker tasks are awaited before runtime teardown completes

This avoids dropping in-flight dataset work at shutdown boundaries.
