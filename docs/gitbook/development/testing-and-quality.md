# Testing and Quality Gates

SOF treats test strategy as part of the architecture, not just part of CI.

## Test Pyramid

1. unit tests in the owning slice
2. integration tests at composition boundaries
3. fuzz targets for parser and state-machine edge cases

## Required Local Checks

The main contributor gate is:

```bash
cargo make ci
```

That runs:

- formatting check
- architecture boundary check
- clippy matrix
- test matrix

For dependency-policy checks as well:

```bash
cargo make ci-full
```

## Fast Commands During Development

```bash
cargo check -p sof
cargo test -p sof-tx
cargo make arch-check
```

## Fuzzing Expectations

Fuzz targets exist for both `sof` and `sof-gossip-tuning`.

Run the bounded workspace smoke suites with:

```bash
cargo make fuzz-smoke
cargo make fuzz-smoke-gossip-tuning
```

For deeper campaigns:

```bash
cd crates/sof-observer
cargo +nightly fuzz run <target>
```

When fuzzing finds a crash:

- reproduce it deterministically
- add a regression test in the owning module
- minimize and save the corpus seed

## Policy For Behavior Changes

- every bug fix gets a regression test
- invariant-bearing constructors need positive and negative tests
- flaky tests are defects and should be fixed before merge
