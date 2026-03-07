# sof-gossip-tuning fuzz targets

This directory contains `cargo-fuzz` targets for the tuning domain/application contract.

## Targets

- `profile_runtime_port`: applies built-in host presets through the runtime tuning port and checks projection invariants.

## Run

```bash
cd crates/sof-gossip-tuning
cargo +nightly fuzz run profile_runtime_port
```
