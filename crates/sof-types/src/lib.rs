#![forbid(unsafe_code)]

//! Stable SOF-owned primitive types shared across SOF crates.

/// Base58 decoding helpers shared by fixed-width SOF primitives.
mod base58;
/// Stable SOF-owned account and validator identity types.
mod pubkey;
/// Stable SOF-owned transaction signature types.
mod signature;

pub use base58::DecodeBase58Error;
pub use pubkey::{PUBKEY_BYTES, PubkeyBytes};
pub use signature::{SIGNATURE_BYTES, SignatureBytes};
