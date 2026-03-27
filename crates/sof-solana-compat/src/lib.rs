#![forbid(unsafe_code)]

//! Explicit Solana-coupled compatibility layer for `sof` and `sof-tx`.

/// Solana-native submit helpers layered on top of `sof-tx`.
mod submit_ext;
/// Solana-native transaction builder and signing helpers.
mod tx_builder;

pub use sof_tx;
pub use sof_types;
pub use submit_ext::{SolanaCompatSubmitError, TxSubmitClientSolanaExt};
pub use tx_builder::{
    BuilderError, DEFAULT_DEVELOPER_TIP_LAMPORTS, DEFAULT_DEVELOPER_TIP_RECIPIENT,
    MAX_TRANSACTION_ACCOUNT_LOCKS, MAX_TRANSACTION_WIRE_BYTES, SignerRef, TxBuilder,
    TxMessageVersion, UnsignedTx,
};
