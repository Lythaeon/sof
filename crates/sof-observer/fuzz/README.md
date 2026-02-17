# SOF fuzz targets

This directory contains `cargo-fuzz` targets for parser and state-machine hardening.

## Targets

- `parse_wire`: shred wire parser/header consistency checks.
- `verify_packet`: packet verification state-machine robustness.
- `dataset_reassembly`: contiguous dataset reconstruction invariants.
- `slot_reassembly`: slot-level reassembly invariants.
- `shred_fec_recover`: FEC ingest/recovery resilience.
- `plugin_dispatch_events`: plugin host dispatch + control-plane state updates.
- `runtime_setup_builder`: runtime setup/env-builder API hardening.
- `verify_leader_diff`: slot-leader map/diff invariants.

## List targets

```bash
cd crates/sof-observer
cargo +nightly fuzz list
```

## Run one target

```bash
cd crates/sof-observer
cargo +nightly fuzz run parse_wire
```

## Corpus seeds

Checked-in seed corpora live under `corpus/<target>/`.

Replay from corpus:

```bash
cd crates/sof-observer
cargo +nightly fuzz run parse_wire corpus/parse_wire
```

## Smoke suite

Run bounded smoke coverage across all targets:

```bash
cargo make fuzz-smoke
```

## Promote fuzz crashes to regression tests

1. Reproduce crash with the saved artifact:
   - `cd crates/sof-observer`
   - `cargo +nightly fuzz run <target> artifacts/<target>/crash-...`
2. Add a deterministic regression test in the owning module (`src/.../tests.rs`).
3. Add the minimized reproducer to `corpus/<target>/`.
4. Re-run `cargo make fuzz-smoke` and `cargo make ci`.

From repository root, the same command can be run via:

```bash
cargo make fuzz -- run parse_wire
```
