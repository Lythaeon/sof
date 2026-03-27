# System Overview

This is the shortest architecture view of SOF.

The diagram is intentionally high-level. It is here to show how the main pieces fit together
without making you read the crate internals first.

## Architecture At A Glance

<div class="mermaid">
flowchart LR
    subgraph Sources["Traffic Sources"]
        UDP["Direct UDP ingest"]
        GOSSIP["Gossip bootstrap"]
        KB["Kernel-bypass queue"]
        PROVIDER["Processed provider stream"]
    end

    subgraph Runtime["sof runtime"]
        INGEST["Packet ingest"]
        PARSE["Shred parse / optional verify"]
        FEC["FEC recovery and dataset reconstruction"]
        STATE["Local state and control plane"]
        HOOKS["Plugins and runtime hooks"]
    end

    UDP --> INGEST
    GOSSIP --> INGEST
    KB --> INGEST
    PROVIDER --> HOOKS
    INGEST --> PARSE --> FEC --> STATE --> HOOKS
</div>

## Read It From Left To Right

SOF's architecture is easiest to reason about as three layers:

- ingress
- runtime
- consumption

- traffic enters from one of the supported ingress modes
- `sof` ingests packets and parses them into useful runtime data
- the runtime reconstructs datasets and updates local control-plane state
- plugins and local state consumers sit on top of those outputs

Processed provider mode is the important exception to the raw packet path:

- Yellowstone gRPC, LaserStream gRPC, websocket `transactionSubscribe`, and
  `ProviderStreamMode::Generic` enter SOF after the packet/shred stages
- built-in processed providers are transaction-first
- generic provider mode is the flexible path when a custom producer wants to
  feed richer control-plane updates into the same runtime surface

## Why This Matters

The important boundary is this:

- `sof` is the live observer/runtime
- `sof-tx` is the transaction SDK that can consume control-plane outputs from `sof`
- your own services sit on top of those outputs, not inside the packet path

That split is what keeps the project composable.

## What The Diagram Does Not Show

This diagram leaves out the lower-level details on purpose:

- dedupe and conflict suppression
- relay and repair internals
- tuning knobs
- exact plugin callback surfaces

Once this picture makes sense, the deeper pages become much easier to read:

- [Why SOF Exists](../use-sof/why-sof-exists.md)
- [SOF Compared To The Usual Alternatives](../use-sof/sof-compared.md)
- [Runtime Pipeline](runtime-pipeline.md)
- [Derived State and Control Plane](derived-state-and-control-plane.md)
