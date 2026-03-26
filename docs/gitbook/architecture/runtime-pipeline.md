# Runtime Pipeline

The `sof` runtime is a bounded, staged pipeline. Thinking in stages is the easiest way to reason
about throughput, backpressure, and where downstream events actually come from.

## Pipeline Shape

```text
ingress
  -> parse
  -> optional verify
  -> dedupe / conflict suppression
  -> FEC recovery
  -> dataset reconstruction
  -> tx extraction and classification
  -> slot / reorg / control-plane updates
  -> plugin and extension dispatch
```

## Stage Responsibilities

### Ingress

Traffic enters through one of the raw-ingest sources:

- direct UDP listener
- gossip bootstrap path
- external kernel-bypass ingress queue

Processed provider mode is a fourth ingest family at the runtime boundary:

- Yellowstone gRPC
- LaserStream gRPC
- websocket `transactionSubscribe`
- `ProviderStreamMode::Generic` for custom producers

Those providers enter SOF after the packet/shred stages. They feed
transaction, transaction-view, and supported control-plane updates directly into
the runtime.

### Parse and verify

SOF parses raw packets into shreds and can optionally perform shred
verification.

Verification defaults depend on trust posture:

- `public_untrusted`: verification on by default
- `trusted_raw_shred_provider`: verification off by default unless you override
  it explicitly
- processed provider mode: no raw shred verification stage, because packets and
  shreds are not entering SOF there

### Dedupe and conflict suppression

Duplicate handling is not just a performance optimization. SOF uses semantic suppression so the
same shred observation does not cause duplicate downstream datasets or transaction callbacks.

### FEC and reassembly

Recovered or intact shreds are grouped into contiguous data ranges, then reconstructed into
datasets that can be decoded into transactions.

### Local state and event emission

As datasets and transactions are processed, SOF updates:

- local slot and fork state
- recent blockhash observations
- leader schedule context
- cluster topology snapshots
- local transaction commitment tagging

Those state transitions then drive plugin events and can feed derived-state consumers or `sof-tx`
adapters.

## Why Boundedness Matters

Every stage has explicit queueing and retention decisions so the runtime fails in known ways under
pressure:

- drop instead of unbounded buffering on hot paths
- retain only bounded relay and repair state
- keep parallelism explicit instead of accidental

That makes the runtime easier to tune for low-latency hosts than systems that quietly turn every
problem into more queued memory.
