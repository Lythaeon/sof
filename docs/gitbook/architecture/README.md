# Architecture

The architecture documents explain why SOF is shaped like infrastructure software rather than a
generic crypto SDK.

This section is primarily maintainer-facing. External users should read it only when they need to
understand internal design tradeoffs or debug integration behavior at a deeper level.

## Core Themes

- bounded queues and explicit failure surfaces
- local control-plane state instead of blind RPC dependency
- replayable and restart-safe stateful consumer paths
- narrow slice boundaries and type-driven modeling
- performance decisions documented as architecture, not hidden as implementation trivia

## Read This Section For

- runtime data flow and stage ownership
- the distinction between plugins, runtime extensions, and derived-state consumers
- how `sof` and `sof-tx` share local control-plane information
- where the ADRs and ARDs fit into day-to-day engineering decisions
