use std::fmt;

use serde::{Deserialize, Serialize};

use crate::base58::DecodeBase58Error;

/// Length in bytes of one validator identity / account key.
pub const PUBKEY_BYTES: usize = 32;

/// Stable SOF-owned 32-byte validator identity / account key wrapper.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct PubkeyBytes([u8; PUBKEY_BYTES]);

impl PubkeyBytes {
    /// Creates one wrapper from raw bytes.
    #[must_use]
    pub const fn new(bytes: [u8; PUBKEY_BYTES]) -> Self {
        Self(bytes)
    }

    /// Returns the inner fixed-width byte array by value.
    #[must_use]
    pub const fn into_array(self) -> [u8; PUBKEY_BYTES] {
        self.0
    }

    /// Returns the inner fixed-width byte array by reference.
    #[must_use]
    pub const fn as_array(&self) -> &[u8; PUBKEY_BYTES] {
        &self.0
    }

    /// Decodes one base58-encoded pubkey string.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeBase58Error`] when the string is not valid base58 or has the wrong width.
    pub fn from_base58(value: &str) -> Result<Self, DecodeBase58Error> {
        let decoded =
            bs58::decode(value)
                .into_vec()
                .map_err(|error| DecodeBase58Error::InvalidBase58 {
                    message: error.to_string(),
                })?;
        let bytes: [u8; PUBKEY_BYTES] =
            decoded
                .try_into()
                .map_err(|decoded: Vec<u8>| DecodeBase58Error::WrongLength {
                    expected: PUBKEY_BYTES,
                    actual: decoded.len(),
                })?;
        Ok(Self(bytes))
    }

    /// Encodes this pubkey as a base58 string.
    #[must_use]
    pub fn to_base58(self) -> String {
        bs58::encode(self.0).into_string()
    }
}

impl From<[u8; PUBKEY_BYTES]> for PubkeyBytes {
    fn from(value: [u8; PUBKEY_BYTES]) -> Self {
        Self::new(value)
    }
}

impl From<PubkeyBytes> for [u8; PUBKEY_BYTES] {
    fn from(value: PubkeyBytes) -> Self {
        value.into_array()
    }
}

impl fmt::Display for PubkeyBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base58())
    }
}

#[cfg(feature = "solana-compat")]
impl PubkeyBytes {
    /// Converts one Solana pubkey into the SOF-owned wrapper.
    #[must_use]
    pub const fn from_solana(value: solana_pubkey::Pubkey) -> Self {
        Self::new(value.to_bytes())
    }

    /// Converts this SOF-owned wrapper into one Solana pubkey.
    #[must_use]
    pub fn to_solana(self) -> solana_pubkey::Pubkey {
        solana_pubkey::Pubkey::new_from_array(self.into_array())
    }
}

#[cfg(feature = "solana-compat")]
impl From<solana_pubkey::Pubkey> for PubkeyBytes {
    fn from(value: solana_pubkey::Pubkey) -> Self {
        Self::from_solana(value)
    }
}

#[cfg(feature = "solana-compat")]
impl From<PubkeyBytes> for solana_pubkey::Pubkey {
    fn from(value: PubkeyBytes) -> Self {
        value.to_solana()
    }
}
