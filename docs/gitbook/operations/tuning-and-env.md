# Tuning and Environment Controls

SOF exposes many environment variables, but most operators should not touch most of them.

The first performance decision is ingress choice, not knob count. If the host sees traffic late,
local tuning cannot recover that loss.

## Safe Baseline

For first deployments, keep to:

- `RUST_LOG`
- `SOF_BIND`
- `SOF_GOSSIP_ENTRYPOINT` when using gossip bootstrap

Enable SOF's runtime-owned probe and scrape listener only when you actually need it:

- `SOF_OBSERVABILITY_BIND`

When enabled, SOF exposes:

- `/metrics`
- `/healthz`
- `/readyz`

If you are starting in processed provider mode instead of raw-shred ingest, the safe baseline is
different:

- keep provider endpoint and auth config explicit in code
- keep replay and durability settings at their defaults first
- do not tune packet-worker, dataset-worker, FEC, or relay/repair knobs before you have measured a
  raw-shred runtime, because built-in provider mode does not use those stages the same way

If you configure the runtime in code, prefer `RuntimeSetup` and `sof-gossip-tuning` instead of raw
string overrides.

## Placement Guidance

Pinning and placement should be read narrowly:

- SOF exposes useful single-host controls
- SOF does not claim full NUMA-aware scheduling
- multi-socket hosts still need measured placement decisions

Current playbook:

- public single-socket VPS: start from `sof-gossip-tuning`'s validated `Vps` preset
- processed provider mode: tune replay, durability, and source health before touching packet/shred
  worker knobs
- trusted raw-shred mode: keep receive, packet-worker, and dataset-worker placement on the same
  socket when possible
- multi-socket hosts: prefer one socket first unless measurement proves cross-socket fanout helps
  more than it hurts

If the real requirement is lower latency rather than cleaner operations, revisit ingress before
revisiting thread knobs:

- private raw feed
- direct validator-adjacent ingress
- better host placement

## Preferred Tuning Order

1. keep defaults
2. apply a typed `sof-gossip-tuning` preset
3. measure
4. change one advanced knob at a time

That measurement step is explicit. For the actual optimization workflow and release-level measured
results, use [Performance and Measurement](../architecture/performance-and-measurement.md).

For transaction plugins, prefer API-level fast paths before runtime knob tuning:

- use `TransactionDispatchMode::Inline` when the plugin actually benefits from earliest tx
  visibility
- use `TransactionPrefilter` for signature or account-key matching instead of heavier custom logic
  when possible

## Signs You Are Overtuning

- queue capacities keep rising but latency keeps getting worse
- worker counts exceed the host's ability to keep caches warm
- repair traffic grows faster than recovered useful data
- changes are made without any before/after capture
- pinning is applied without proving that locality improved

## Practical Advice

- prefer typed profiles for repeatable hosts
- keep a host-specific tuning log
- change one family of knobs at a time
- watch queue depth, drop counters, replay health, and repair behavior before and after each change
- do not keep a knob or code change just because it sounded faster before measurement
