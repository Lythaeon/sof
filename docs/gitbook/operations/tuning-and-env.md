# Tuning and Environment Controls

SOF exposes many environment variables, but most operators should not touch most of them.

## Safe Baseline

For first deployments, keep to:

- `RUST_LOG`
- `SOF_BIND`
- `SOF_GOSSIP_ENTRYPOINT` when using gossip bootstrap

Enable SOF's runtime-owned probe/scrape listener only when you actually need it:

- `SOF_OBSERVABILITY_BIND`

When enabled, SOF exposes:

- `/metrics`
- `/healthz`
- `/readyz`

If you configure the runtime in code, prefer `RuntimeSetup` and `sof-gossip-tuning` instead of raw
string overrides.

The typed `Vps` preset now reflects the validated public-host profile:

- `SOF_UDP_BATCH_SIZE=96`
- `SOF_TVU_SOCKETS=2`
- `SOF_UDP_RECEIVER_PIN_BY_PORT=true`
- `SOF_GOSSIP_RECEIVER_CHANNEL_CAPACITY=131072`
- `SOF_GOSSIP_SOCKET_CONSUME_CHANNEL_CAPACITY=65536`
- `SOF_GOSSIP_RESPONSE_CHANNEL_CAPACITY=65536`
- `SOF_GOSSIP_CHANNEL_CONSUME_CAPACITY=4096`
- `SOF_GOSSIP_CONSUME_THREADS=4`
- `SOF_GOSSIP_LISTEN_THREADS=4`
- `SOF_GOSSIP_RUN_THREADS=4`
- `SOF_SHRED_DEDUP_CAPACITY=524288`

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

Useful bundled gossip telemetry while tuning:

- `gossip_receiver channel_len` and `num_packets_dropped`
- `gossip_socket_consume_verify_queue current_len`, `max_len`, and `dropped_packets`
- `gossip_socket_consume_output_queue current_len`, `max_len`, and `dropped_packets`
- `cluster_info_stats2 gossip_packets_dropped_count`

Useful runtime-owned endpoint metrics while tuning:

- `sof_ingest_packets_seen_total`
- `sof_ingest_dropped_packets_total`
- `sof_dataset_queue_depth`
- `sof_packet_worker_queue_depth`
- `sof_shred_dedupe_capacity_evictions_total`
- `sof_latest_shred_age_ms`
- `sof_gossip_runtime_stall_age_ms`

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
