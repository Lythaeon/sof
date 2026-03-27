# sof-solana-compat

Explicit Solana-coupled compatibility layer for `sof` and `sof-tx`.

Use this crate when you want convenience APIs built directly on upstream
`solana-*` types like:

- `solana_instruction::Instruction`
- `solana_signer::Signer`
- `solana_transaction::versioned::VersionedTransaction`

Use `sof-tx` directly when you want the version-agnostic SOF-owned core surface.
