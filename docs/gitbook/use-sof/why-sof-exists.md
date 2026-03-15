# Why SOF Exists

SOF was created to solve a practical problem: if you want low-latency Solana data, RPC is often
too far away from the actual traffic.

RPC is convenient, but it is not where the network starts. By the time your application reads a
response from an RPC node or a third-party stream provider, several things have already happened:

- the validator or provider saw the traffic first
- the data was decoded and repackaged for an API surface you do not control
- your service paid the extra network hop and the operational cost of that dependency

That is fine for many applications. It is not ideal when you need the earliest useful view of the
network and you care about operating the data path yourself.

## The Problem SOF Was Built To Solve

SOF fits services that need one or more of these:

- live Solana traffic closer to the source than RPC
- locally derived control-plane state such as recent blockhash and leader context
- explicit bounded behavior under pressure instead of unbounded background machinery
- a foundation for analytics, monitoring, automation, or low-latency submission

The project started from the idea that there should be a middle ground between:

- depending entirely on RPC or a managed stream provider
- operating a full validator-shaped stack just to observe and react to traffic

SOF is that middle ground.

## What SOF Is Trying To Be

SOF is not trying to replace every other Solana interface.

It is trying to be a small, explicit, low-latency runtime for services that want to observe live
traffic and do something useful with it.

That means the project optimizes for:

- direct network ingest
- predictable, bounded runtime behavior
- explicit operational tradeoffs
- modular downstream consumers such as plugins, derived state, and `sof-tx`

## Why Not Just Use RPC?

RPC is often enough. SOF exists because some services need a different tradeoff.

RPC is usually the right tool for:

- simplicity
- broad ecosystem compatibility
- request/response reads more than live ingest

SOF becomes attractive for services that need:

- lower-latency access to live traffic
- fewer external dependencies in the data path
- control over how traffic is parsed, retained, and surfaced
- direct integration into your own service architecture

## Why Not Just Run A Validator?

Running a validator gives you a broad Solana surface, but it also brings validator-shaped
operational cost and state complexity.

SOF was created for services that do not need the whole validator job. They need a focused runtime
that can:

- ingest packets
- parse and reconstruct useful data
- surface transactions and state signals
- stay bounded and tunable

That is a narrower goal than being a validator, and it is a more practical one for many backend
services.

## The Core Idea In One Sentence

SOF exists for services that need to see live Solana traffic early, derive useful local state from
it, and act without building on top of a heavy RPC-first or validator-first stack.
