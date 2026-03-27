# Build A Live-Only Stream Service

Start here if you want to build a service on top of SOF that streams live events to external
clients.

The focus here is the first version:

- live only
- no replay
- no catch-up
- no historical backfill

That is a valid product shape, and it is much easier to operate than a retained-stream platform.

## Use This When

- you want to expose live SOF-derived events to other services or customers
- you do not want to store stream history yet
- your value is better filtering, better local control, or earlier local handling than an API-first
  stack

## What SOF Gives You

SOF gives you the live runtime side:

- ingest or provider adaptation
- parsing or runtime event generation
- plugin events
- local control-plane signals where the ingress supports them

Your stream service still needs to add:

- client connections
- auth and quotas
- filtering
- fanout
- backpressure policy

## The First Sensible Architecture

For a first version, keep the split simple:

1. `sof` ingests and emits plugin events
2. your service maps those events into your own stable stream schema
3. your service fans them out to connected clients

SOF is the runtime foundation, not the whole stream product.

## What To Keep Explicit In A Live-Only Product

Say these things clearly in your own docs and API contract:

- subscribers only receive events observed after subscription starts
- disconnects can lose events
- there is no replay or catch-up yet
- slow consumers will be dropped or rate-limited

## A Good First Event Surface

Most stream services should start with only a few stream classes:

- transaction events
- slot or commitment progression
- topology or leader changes
- optional derived control-plane snapshots

Do not start by exposing every internal hook directly.
