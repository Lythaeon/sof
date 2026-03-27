# sof-types

Stable SOF-owned primitive types shared across SOF crates.

This crate exists to keep `sof` and `sof-tx` public APIs from exposing upstream
`solana-*` crate versions directly on every boundary.

Use `sof-solana-compat` when you explicitly want Solana crate interop.
