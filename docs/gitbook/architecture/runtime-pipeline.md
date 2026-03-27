# Runtime Pipeline

The `sof` runtime is a bounded pipeline. Thinking in stages is the easiest way to reason about
throughput, backpressure, and where downstream events actually come from.

## Pipeline Shape

```text
ingress
  -> parse
  -> optional verify
  -> dedupe / conflict suppression
  -> recovery / reconstruction
  -> tx extraction and classification
  -> control-plane updates
  -> plugin / derived-state / extension dispatch
```

## Stage Responsibilities

### Ingress

Traffic enters through one of the ingress families:

- direct UDP
- gossip bootstrap
- external kernel-bypass or private raw feed
- processed provider stream

Processed providers enter SOF after the packet and shred stages. They feed transaction and
supported control-plane updates into the runtime boundary directly.

### Parse and verify

In raw-shred modes, SOF parses packets into shreds and can verify them.

Verification defaults depend on trust posture:

- `public_untrusted`: verification on by default
- `trusted_raw_shred_provider`: verification off by default unless explicitly overridden
- processed provider mode: no raw shred verification stage because raw packets are not entering SOF

### Dedupe and conflict suppression

Duplicate handling is a correctness boundary as much as a performance concern. SOF uses bounded
dedupe and suppression so the same observation does not cause duplicate downstream behavior.

### Recovery and reconstruction

Recovered or intact shreds are grouped into usable ranges, then reconstructed into datasets and
transactions.

### Local state and event emission

As datasets and transactions are processed, SOF updates local runtime state such as:

- slot progression
- recent blockhash observations
- leader context
- topology snapshots
- commitment tagging

Those state transitions then drive plugin events and can feed derived-state consumers or `sof-tx`
adapters.

Switching ingress mode changes which parts of this pipeline SOF actually owns:

- raw shreds: packet, shred, verify, recovery, and reconstruction
- built-in processed providers: provider adaptation and the downstream runtime boundary
- generic providers: the typed runtime boundary you feed into
