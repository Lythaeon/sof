# Common Recipes

These are the product shapes you will usually end up choosing between when you deploy SOF.

## Observer Host Only

Use:

- `sof`

Use this when:

- you need local ingest
- you want plugin events, datasets, slot state, or topology observation
- transaction submission lives elsewhere

You will usually start with:

- direct UDP for controlled bring-up
- `gossip-bootstrap` once you need richer cluster context

Implementation shape:

- `sof::runtime::run_async()` for first bring-up
- `PluginHost` once your service starts consuming transactions, slots, or topology

## Submitter With External Control Plane

Use:

- `sof-tx`

Use this when:

- your organization already has blockhash and leader sources
- the service only needs to build and send transactions

You will usually start with:

- RPC transport first
- `Hybrid` once direct routing inputs are proven trustworthy

Implementation shape:

- `TxBuilder`
- `TxSubmitClient`
- external `LeaderProvider` and `RecentBlockhashProvider`

## One Process: Observe And Submit

Use:

- `sof`
- `sof-tx` with `sof-adapters`
- `PluginHostTxProviderAdapter`

Use this when:

- you want one low-latency service owning both observation and submission
- local freshness matters more than replay and restart semantics

This is the normal product shape for local execution services that want live TPU and leader state
without a separate internal control-plane service.

Implementation shape:

- `PluginHostTxProviderAdapter`
- `PluginHost::builder().add_shared_plugin(...)`
- `TxSubmitClient::new(adapter.clone(), adapter.clone())`

## Restart-Safe Stateful Execution Service

Use:

- `sof`
- derived-state host
- `sof-tx` with `sof-adapters`
- `DerivedStateTxProviderAdapter`

Use this when:

- you need replay and checkpoint recovery
- the service must keep a trustworthy local baseline across restarts

This is the heavier but more durable path.

## Specialized Network Front End

Use:

- `sof` with `kernel-bypass`
- your own AF_XDP or other front-end ingress
- optional `sof-tx` if the same service also submits

Use this when:

- you already own the NIC path
- the built-in UDP ingress is not the final network shape you need

Do not start here unless you have already measured why the standard ingress path is insufficient.
