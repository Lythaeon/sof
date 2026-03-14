# Relay, Repair, and Traffic

Operators often miss one key point: in gossip mode, SOF is not observer-only by default.

## Default Network Posture

When `gossip-bootstrap` is enabled, SOF can:

- receive live traffic from the gossip-discovered network
- relay recent shreds through a bounded cache
- serve bounded repair responses

That design is intentional. SOF contributes capacity instead of only consuming it.

## Reduce Outbound Activity

If your goal is to keep ingest while cutting outward network activity, start here:

```bash
SOF_UDP_RELAY_ENABLED=false
SOF_REPAIR_ENABLED=false
```

Those are the blunt controls.

Before disabling behavior entirely, the operations guidance in this repository also recommends
trying more conservative reductions such as:

- narrowing `SOF_GOSSIP_VALIDATORS`
- keeping `SOF_UDP_RELAY_REQUIRE_TURBINE_SOURCE_PORTS=true`
- lowering `SOF_UDP_RELAY_MAX_SENDS_PER_SEC` incrementally

## Why Boundedness Matters

Relay and repair are not allowed to grow without limit.

Examples of explicit bounds:

- relay cache window and maximum retained shreds
- relay fanout and sends-per-second budgets
- repair request budgets per tick
- per-slot repair caps
- per-peer serve limits and byte budgets

These limits are part of the anti-amplification and hot-path discipline, not just tuning details.

## When To Tune These Knobs

Tune relay and repair only when you have one of these symptoms:

- low ingest coverage with otherwise healthy packet sources
- unnecessary outbound bandwidth for your deployment role
- repeated repair escalation caused by genuine packet loss

If those conditions are not present, stick to defaults.
