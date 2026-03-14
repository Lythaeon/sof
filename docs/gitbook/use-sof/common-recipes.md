# Common Recipes

These are the common product shapes people actually deploy with SOF.

## Observer Host Only

Use:

- `sof`

Good fit when:

- you need local ingest
- you want plugin events, datasets, slot state, or topology observation
- transaction submission lives elsewhere

Start with:

- direct UDP for controlled bring-up
- `gossip-bootstrap` once you need richer cluster context

## Submitter With External Control Plane

Use:

- `sof-tx`

Good fit when:

- your organization already has blockhash and leader sources
- the service only needs to build and send transactions

Start with:

- RPC transport first
- `Hybrid` once direct routing inputs are proven trustworthy

## One Process: Observe And Submit

Use:

- `sof`
- `sof-tx` with `sof-adapters`
- `PluginHostTxProviderAdapter`

Good fit when:

- you want one low-latency service owning both observation and submission
- local freshness matters more than replay and restart semantics

This is the normal product shape for local execution services that want live TPU and leader state
without a separate internal control-plane service.

## Restart-Safe Stateful Execution Service

Use:

- `sof`
- derived-state host
- `sof-tx` with `sof-adapters`
- `DerivedStateTxProviderAdapter`

Good fit when:

- you need replay and checkpoint recovery
- the service must keep a trustworthy local baseline across restarts

This is the heavier but more durable path.

## Specialized Network Front End

Use:

- `sof` with `kernel-bypass`
- your own AF_XDP or other front-end ingress
- optional `sof-tx` if the same service also submits

Good fit when:

- you already own the NIC path
- the built-in UDP ingress is not the final network shape you need

Do not start here unless you have already measured why the standard ingress path is insufficient.
