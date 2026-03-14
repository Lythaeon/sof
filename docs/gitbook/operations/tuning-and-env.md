# Tuning and Environment Controls

SOF exposes many environment variables, but most operators should not touch most of them.

## Safe Baseline

For first deployments, keep to:

- `RUST_LOG`
- `SOF_BIND`
- `SOF_GOSSIP_ENTRYPOINT` when using gossip bootstrap

If you configure the runtime in code, prefer `RuntimeSetup` and `sof-gossip-tuning` instead of raw
string overrides.

## Preferred Tuning Order

1. keep defaults
2. apply a typed `sof-gossip-tuning` preset
3. measure
4. change one advanced knob at a time

This order matters because many queue and worker controls simply trade one failure mode for
another.

## Advanced Control Families

### Runtime and dataset controls

These affect thread counts, queue capacities, and reconstruction fanout. Oversizing them can add
contention and memory pressure instead of throughput.

### Verification and dedupe controls

These affect CPU cost and correctness-protection windows. Verification adds cryptographic cost.
Dedupe capacity that is too low weakens protection against duplicate downstream emissions.

### Relay and repair controls

These affect outbound traffic, peer pressure, and recovery pacing. Aggressive settings can create
more network noise without improving useful ingest.

### Gossip bootstrap controls

These affect bootstrap worker counts, queue budgets, and control-plane behavior inside the bundled
backend. They are operationally sensitive and should be changed with measurement.

## Signs You Are Overtuning

- queue capacities keep rising but latency keeps getting worse
- worker counts exceed the host's ability to keep CPU caches warm
- repair traffic grows faster than recovered useful data
- changes are made without any metric or before/after capture

## Practical Advice

- prefer typed profiles for repeatable hosts
- keep a host-specific tuning log
- change one family of knobs at a time
- watch dedupe, queue depth, and repair metrics before and after each change
