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

If you are starting in processed provider mode instead of raw-shred ingest, the
safe baseline is different:

- keep the provider endpoint and auth config explicit in code
- keep replay and durability settings at their defaults first
- do not tune packet-worker, dataset-worker, FEC, or relay/repair knobs before
  you have measured a raw-shred runtime, because built-in provider mode does not
  use those packet/shred stages the same way

If you configure the runtime in code, prefer `RuntimeSetup` and `sof-gossip-tuning` instead of raw
string overrides.

The typed `Vps` preset now reflects the validated public-host profile:

- `SOF_UDP_BATCH_SIZE=96`
- `SOF_TVU_SOCKETS=4`
- `SOF_UDP_RECEIVER_PIN_BY_PORT=false`
- `SOF_UDP_BUSY_POLL_US` / `SOF_UDP_BUSY_POLL_BUDGET` / `SOF_UDP_PREFER_BUSY_POLL` (Linux-only, off by default; use only when you want lower interrupt jitter and accept higher CPU burn)
- `SOF_GOSSIP_RECEIVER_CHANNEL_CAPACITY=131072`
- `SOF_GOSSIP_SOCKET_CONSUME_CHANNEL_CAPACITY=65536`
- `SOF_GOSSIP_RESPONSE_CHANNEL_CAPACITY=65536`
- `SOF_GOSSIP_CHANNEL_CONSUME_CAPACITY=4096`
- `SOF_GOSSIP_CONSUME_THREADS=4`
- `SOF_GOSSIP_LISTEN_THREADS=4`
- `SOF_GOSSIP_RUN_THREADS=4`
- `SOF_SHRED_DEDUP_CAPACITY=524288`

Two latency-oriented knobs are intentionally left out of the typed preset because
they are too host-specific to recommend as defaults:

- `SOF_RUNTIME_CURRENT_THREAD=true` with `SOF_RUNTIME_CORE=<n>` isolates the main
  SOF runtime thread onto one core. This can improve `ready -> plugin` latency on
  some one-box deployments and hurt throughput on others.
- `SOF_PACKET_WORKER_BATCH_MAX_PACKETS` changes how much accepted-shred work one
  packet-worker burst can hand back at once. Smaller values reduce burst
  head-of-line delay but add more queue churn.

Measured public-VPS note from the current 4-core sweep:

- default `SOF_TVU_SOCKETS=4` beat both `2` and `8` on tx-availability latency
- `SOF_TVU_SOCKETS=2` improved throughput but made tx visibility slower
- Linux busy-poll also made tx visibility slower on that host
- larger host kernel receive buffers improved throughput but still hurt tx-availability latency

So tune these only with before/after captures on the actual host you care about.

## Preferred Tuning Order

1. keep defaults
2. apply a typed `sof-gossip-tuning` preset
3. measure
4. change one advanced knob at a time

For transaction plugins, prefer API-level fast paths before runtime knob tuning:

- use `TransactionDispatchMode::Inline` when the plugin actually benefits from
  earliest tx visibility
- use `TransactionPrefilter` for signature/account-key matching instead of
  custom `transaction_interest_ref` logic when possible

That combination lets SOF reject irrelevant inline traffic from sanitized
transaction views and skip full owned tx decode on misses.

This order matters because many queue and worker controls simply trade one failure mode for
another.

## Advanced Control Families

### Runtime and dataset controls

These affect thread counts, queue capacities, and reconstruction fanout. Oversizing them can add
contention and memory pressure instead of throughput.

Special-case controls in this family:

- `SOF_RUNTIME_CURRENT_THREAD`
- `SOF_RUNTIME_CORE`
- `SOF_PACKET_WORKER_BATCH_MAX_PACKETS`

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
- `sof_udp_relay_forwarded_packets_total`
- `sof_udp_relay_source_filtered_packets_total`
- `sof_repair_requests_sent_total`
- `sof_repair_outstanding_entries`
- `sof_repair_peer_active`
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
