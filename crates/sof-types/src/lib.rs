#![forbid(unsafe_code)]

//! Stable SOF-owned primitive types shared across SOF crates.

mod base58;
mod pubkey;
mod signature;

pub use base58::DecodeBase58Error;
pub use pubkey::{PUBKEY_BYTES, PubkeyBytes};
pub use signature::{SIGNATURE_BYTES, SignatureBytes};
