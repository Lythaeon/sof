Local SOF patch of `helius-laserstream` `0.1.9`.

This keeps the same crate API surface used by SOF while trimming unused direct
dependencies from the published crate, especially the old `tonic 0.12` path
that is not required by SOF's build.
