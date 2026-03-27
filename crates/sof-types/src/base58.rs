use thiserror::Error;

/// Error returned when decoding a base58 string into a fixed-width SOF type.
#[derive(Debug, Clone, Eq, Error, PartialEq)]
pub enum DecodeBase58Error {
    /// Base58 payload did not decode to the expected width.
    #[error("expected {expected} bytes after base58 decode, got {actual}")]
    WrongLength {
        /// Expected decoded width.
        expected: usize,
        /// Actual decoded width.
        actual: usize,
    },
    /// Base58 payload itself was invalid.
    #[error("invalid base58 payload: {message}")]
    InvalidBase58 {
        /// Decoder error details.
        message: String,
    },
}
