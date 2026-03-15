# Crates

SOF is not one wide crate with mixed concerns. It is a small product family split by job:

- observe and derive local runtime state
- build and submit transactions
- apply typed host tuning

That split is intentional because most services do not need the full surface at once.

## Why The Split Exists

- `sof` owns ingest, runtime composition, and downstream event surfaces
- `sof-tx` owns transaction construction and submission policy
- `sof-gossip-tuning` owns typed tuning profiles instead of stringly runtime presets
- `sof-solana-gossip` stays vendored and internal to preserve a tighter public API boundary

## Typical Adoption Patterns

### Observer-only or local market-data service

Start with [`sof`](sof.md).

### Execution service that only needs a send pipeline

Start with [`sof-tx`](sof-tx.md).

This is the right fit when:

- you already have a blockhash source
- you already have a leader or TPU routing source
- you only need transaction construction, routing, and submission

### Execution service that wants locally observed control-plane state

Use [`sof`](sof.md) together with [`sof-tx`](sof-tx.md).

This is the right fit when:

- `sof` is your local source of recent blockhash, leader schedule, and cluster topology state
- `sof-tx` is your submit path
- you want direct or hybrid sending based on locally observed traffic rather than only external RPC
  lookups

### Embedded host with repeatable deployment presets

Add [`sof-gossip-tuning`](sof-gossip-tuning.md) on top of `sof`.

### I am debugging or tuning the bundled gossip backend

Most external users can skip this. Read [`sof-solana-gossip`](sof-solana-gossip.md) only if you
are debugging the optional bootstrap backend or working on SOF internals.
