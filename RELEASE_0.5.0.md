# SOF v0.5.0

## Additions

- Added shared observer-ingress API on runtime extensions: `RuntimeExtensionHost::on_observer_packet_shared`.
- Added aggregate runtime-extension dispatch pressure telemetry in the runtime heartbeat log:
  - active extensions
  - dispatched/dropped totals
  - queue depth and max queue depth
  - max average and max observed dispatch lag
- Added configurable runtime-extension pressure warning thresholds:
  - `SOF_RUNTIME_EXTENSION_QUEUE_DEPTH_WARN`
  - `SOF_RUNTIME_EXTENSION_DISPATCH_LAG_WARN_US`
  - `SOF_RUNTIME_EXTENSION_DROP_WARN_DELTA`

## Improvements

- Reduced observer hook payload copying by sharing a single packet allocation across plugin raw-packet dispatch and runtime-extension observer-ingress dispatch.
- Added runtime warnings for extension dispatch pressure to catch extension-induced saturation before ingest degradation.
- Added unit coverage for extension dispatch telemetry aggregation behavior.

## Fixes

- Runtime telemetry now exposes extension dispatch pressure signals directly in the periodic ingest log stream, removing blind spots during extension-heavy deployments.

## Versioning

- Bumped `sof` crate version to `0.5.0`.
- Bumped `sof-tx` crate version to `0.5.0`.
- Updated `sof-tx` dependency on `sof` to `0.5.0`.
