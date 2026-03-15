# Architecture

The architecture documents explain why SOF is shaped like infrastructure software rather than a
generic crypto SDK.

These pages are mainly for maintainers and for users who need a deeper view of runtime design or
integration behavior.

## Core Themes

- bounded queues and explicit failure surfaces
- local control-plane state instead of blind RPC dependency
- replayable and restart-safe stateful consumer paths
- narrow slice boundaries and type-driven modeling
- performance decisions documented as architecture, not hidden as implementation trivia

## Read This Section For

- a one-screen architecture picture before deeper pipeline details
- runtime data flow and stage ownership
- the distinction between plugins, runtime extensions, and derived-state consumers
- how `sof` and `sof-tx` share local control-plane information
- where the ADRs and ARDs fit into day-to-day engineering decisions

Start with [System Overview](system-overview.md) if you want the shortest possible diagram-first
view of how the pieces fit together.
