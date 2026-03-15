# Build A Live-Only Stream Service

Start here if you want to build a service on top of SOF that streams live events to external
clients.

This page is intentionally about the first version:

- live only
- no replay
- no catch-up
- no historical backfill

That is a valid product shape, and it is much easier to operate than a full retained-stream
platform.

## Use This When

- you want to expose live SOF-derived events to other services or customers
- you do not want to store stream history yet
- your value is low latency, richer event surface, or better filtering

## What SOF Gives You

SOF gives you the live observer/runtime side:

- packet ingest
- shred and dataset reconstruction
- plugin events
- local control-plane signals

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

That means SOF stays the runtime foundation, not the whole SaaS by itself.

## What To Keep Explicit In A Live-Only Product

Say these things clearly in your own docs and API contract:

- subscribers only receive events observed after subscription starts
- disconnects can lose events
- there is no replay or catch-up yet
- slow consumers will be dropped or rate-limited

That clarity matters more than pretending you already have retained-stream semantics.

## A Good First Event Surface

Most stream services should start with only a few stream classes:

- transaction events
- slot or commitment progression
- topology or leader changes
- optional derived control-plane snapshots

Do not start with every internal hook exposed directly.

Instead:

- define your own public stream schema
- keep SOF internals behind that stable boundary

## Filtering Guidance

The attractive early filters are:

- one program id
- one address
- one event class

Keep the first filter model narrow and cheap.

What usually hurts first:

- arbitrary predicate filtering
- many-address fanout for one client
- per-client expensive decode work

## What To Build Before Replay

Before you spend time on replay or durable retention, make sure you already have:

- bounded per-client buffers
- clear slow-consumer handling
- health and lag metrics
- stable stream schemas
- clear auth and rate policy

If those are weak, replay will just make the system more complex, not more useful.

## Where SOF Still Fits Later

If you later add replay or catch-up, SOF still stays the live ingest/runtime core.

What changes is the layer above it:

- append events to durable storage
- assign cursors
- add resume semantics

That is a service-layer concern, not something `sof` needs to become internally.
