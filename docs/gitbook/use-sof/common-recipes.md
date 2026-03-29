# Common Recipes

These are the service shapes you will usually end up choosing between when you deploy SOF.

If you want fuller walkthroughs, open these too:

- [Build An Observer Service](observer-service.md)
- [Build One Process That Observes And Submits](observe-and-submit-service.md)
- [Build A Live-Only Stream Service](live-only-stream-service.md)

## Observer Host Only

Use:

- `sof`

Use this when:

- you need local ingest
- you want plugin events, datasets, slot state, or topology observation
- transaction submission lives elsewhere

You will usually start with:

- direct UDP for controlled bring-up
- `gossip-bootstrap` only once you need richer cluster context
- `gossip-bootstrap` with `SOF_GOSSIP_RUNTIME_MODE=control_plane_only` when you only need
  topology/leader inputs for `sof-tx` and your actual ingress or blockhash source lives elsewhere
  ; in typed setup, use `RuntimeSetup::with_gossip_runtime_mode(GossipRuntimeMode::ControlPlaneOnly)`
- built-in websocket or Yellowstone/LaserStream transaction feeds when you want SOF to keep
  recent blockhash fresh from observed provider transactions without using gossip for data ingress
  ; combine this with gossip topology/leaders only in a custom embedding, not in one packaged
  built-in runtime mode today

## Submitter With External Control Plane

Use:

- `sof-tx`

Use this when:

- your organization already has blockhash and leader sources
- the service only needs to build and send transactions

You will usually start with:

- RPC transport first
- Jito if block-engine submission is already part of the product
- `submit_signed(...)` if signing already happens elsewhere
- `Hybrid` only once direct routing inputs are proven trustworthy

## One Process: Observe And Submit

Use:

- `sof`
- `sof-tx` with `sof-adapters`
- `PluginHostTxProviderAdapter`

Use this when:

- you want one service owning both observation and submission
- local coupling matters more than replay and restart semantics

For this shape, the biggest prerequisite is early ingress. If the host gets shreds late, keeping
`sof` and `sof-tx` together still removes internal overhead, but it does not create early
visibility by itself.

## Restart-Safe Stateful Execution Service

Use:

- `sof`
- derived-state host
- `sof-tx` with `sof-adapters`
- `DerivedStateTxProviderAdapter`

Use this when:

- you need replay and checkpoint recovery
- the service must keep a trustworthy local baseline across restarts

## Specialized Network Front End

Use:

- `sof` with `kernel-bypass`
- your own AF_XDP or other front-end ingress
- optional `sof-tx` if the same service also submits

Use this when:

- you already own the NIC path
- the built-in UDP ingress is not the final network shape you need

This is also the recipe to care about when your real constraint is getting earlier shred visibility
onto the host, not shaving a small amount of local runtime overhead.

## Live-Only Stream Product

Use:

- `sof`
- your own fanout, auth, and filtering layer

Use this when:

- you want to expose live events to external users or internal services
- you do not want replay or catch-up yet
