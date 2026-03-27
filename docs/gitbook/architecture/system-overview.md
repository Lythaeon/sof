# System Overview

This is the shortest architecture view of SOF.

## Architecture At A Glance

<div class="mermaid">
flowchart LR
    subgraph Sources["Ingress"]
        UDP["Direct UDP"]
        GOSSIP["Gossip bootstrap"]
        KB["Kernel-bypass / private raw feed"]
        PROVIDER["Processed provider stream"]
    end

    subgraph Runtime["sof runtime"]
        INGEST["Ingest / provider adaptation"]
        PARSE["Parse / optional verify"]
        FEC["Recovery / reconstruction"]
        STATE["Local state"]
        HOOKS["Plugins / derived state / extensions"]
    end

    UDP --> INGEST
    GOSSIP --> INGEST
    KB --> INGEST
    PROVIDER --> STATE
    INGEST --> PARSE --> FEC --> STATE --> HOOKS
</div>

## Read It From Left To Right

SOF is easiest to reason about as three layers:

- ingress
- runtime
- consumption

Traffic enters from one of the supported ingress families. `sof` turns that ingress into
runtime-owned state and events. Plugins, derived-state consumers, and `sof-tx` adapters sit on top
of that runtime layer.

Built-in processed provider modes start later in the pipeline than raw-shred modes. That is part
of the design, not a bug.

## Why This Matters

The important boundaries are:

- `sof` is the live observer/runtime
- `sof-tx` is the transaction SDK that can consume control-plane outputs from `sof`
- your own services sit on top of those outputs, not inside the packet path

The other important boundary is honesty about ingress:

- raw-shred modes own more of the substrate
- processed providers start later in the pipeline
- public gossip is the independent baseline, not the universal fast path

The same honesty applies to performance claims. SOF treats optimization as measured engineering:

- start from a concrete hypothesis
- measure the baseline
- test one change at a time
- keep only the changes that survive A/B validation

In practice that has meant:

- removing redundant work from hot paths
- adding fast paths so traffic can exit before deeper processing when appropriate
- cutting copies and allocations where the runtime can safely do so
- reducing instructions, branching, and sometimes cache churn for the same work
