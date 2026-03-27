# Architecture

The architecture documents explain how SOF is shaped as infrastructure software rather than as a
generic Solana SDK.

These pages are mainly for maintainers and for users who need a deeper view of runtime design or
integration behavior.

## Core Themes

- ingress, runtime, and consumption are separate layers
- queues and failure surfaces are explicit
- local control-plane state is part of the runtime model
- replayable and restart-safe stateful consumers are separate from observational plugins
- performance decisions are documented as architecture, not hidden as trivia
- performance changes are expected to be measured, not assumed

For the actual performance lineage and measured examples across releases, use
[Performance and Measurement](performance-and-measurement.md).

Start with [System Overview](system-overview.md) if you want the shortest diagram-first view.
