use sof_types::{PubkeyBytes, SignatureBytes};

/// Converts one 64-byte signature slice into `SignatureBytes`.
///
/// # Errors
///
/// Returns the error produced by `on_error` when `bytes` is not exactly 64 bytes long.
pub fn signature_bytes_from_slice<E, F>(bytes: &[u8], on_error: F) -> Result<SignatureBytes, E>
where
    F: FnOnce() -> E,
{
    let raw: [u8; 64] = bytes.try_into().map_err(|_error| on_error())?;
    Ok(SignatureBytes::from(raw))
}

/// Converts one 32-byte pubkey slice into `PubkeyBytes`.
///
/// # Errors
///
/// Returns the error produced by `on_error` when `bytes` is not exactly 32 bytes long.
pub fn pubkey_bytes_from_slice<E, F>(bytes: &[u8], on_error: F) -> Result<PubkeyBytes, E>
where
    F: FnOnce() -> E,
{
    let raw: [u8; 32] = bytes.try_into().map_err(|_error| on_error())?;
    Ok(PubkeyBytes::from(raw))
}
