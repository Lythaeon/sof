use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use crate::base58::DecodeBase58Error;

/// Length in bytes of one transaction signature.
pub const SIGNATURE_BYTES: usize = 64;

/// Stable SOF-owned 64-byte transaction signature wrapper.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SignatureBytes([u8; SIGNATURE_BYTES]);

impl SignatureBytes {
    /// Creates one wrapper from raw bytes.
    #[must_use]
    pub const fn new(bytes: [u8; SIGNATURE_BYTES]) -> Self {
        Self(bytes)
    }

    /// Returns the inner fixed-width byte array by value.
    #[must_use]
    pub const fn into_array(self) -> [u8; SIGNATURE_BYTES] {
        self.0
    }

    /// Returns the inner fixed-width byte array by reference.
    #[must_use]
    pub const fn as_array(&self) -> &[u8; SIGNATURE_BYTES] {
        &self.0
    }

    /// Decodes one base58-encoded signature string.
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
        let bytes: [u8; SIGNATURE_BYTES] =
            decoded
                .try_into()
                .map_err(|decoded: Vec<u8>| DecodeBase58Error::WrongLength {
                    expected: SIGNATURE_BYTES,
                    actual: decoded.len(),
                })?;
        Ok(Self(bytes))
    }

    /// Encodes this signature as a base58 string.
    #[must_use]
    pub fn to_base58(self) -> String {
        bs58::encode(self.0).into_string()
    }
}

impl From<[u8; SIGNATURE_BYTES]> for SignatureBytes {
    fn from(value: [u8; SIGNATURE_BYTES]) -> Self {
        Self::new(value)
    }
}

impl From<SignatureBytes> for [u8; SIGNATURE_BYTES] {
    fn from(value: SignatureBytes) -> Self {
        value.into_array()
    }
}

impl fmt::Display for SignatureBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base58())
    }
}

impl Serialize for SignatureBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for SignatureBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        let signature: [u8; SIGNATURE_BYTES] = bytes
            .try_into()
            .map_err(|bytes: Vec<u8>| D::Error::invalid_length(bytes.len(), &"64 bytes"))?;
        Ok(Self::new(signature))
    }
}

#[cfg(feature = "solana-compat")]
impl SignatureBytes {
    /// Converts one Solana signature into the SOF-owned wrapper.
    #[must_use]
    pub fn from_solana(value: solana_signature::Signature) -> Self {
        Self::new(value.into())
    }

    /// Converts this SOF-owned wrapper into one Solana signature.
    #[must_use]
    pub fn to_solana(self) -> solana_signature::Signature {
        solana_signature::Signature::from(self.into_array())
    }
}

#[cfg(feature = "solana-compat")]
impl From<solana_signature::Signature> for SignatureBytes {
    fn from(value: solana_signature::Signature) -> Self {
        Self::from_solana(value)
    }
}

#[cfg(feature = "solana-compat")]
impl From<SignatureBytes> for solana_signature::Signature {
    fn from(value: SignatureBytes) -> Self {
        value.to_solana()
    }
}
