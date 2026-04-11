/// Partial short-vector decode failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortVecDecodeError {
    /// The payload ended before the short-vector length was fully decoded.
    Incomplete,
    /// The payload encoded an invalid short-vector length.
    Invalid,
}

/// Decodes one Solana short-vector length from `payload`.
#[must_use]
pub fn decode_short_u16_len(payload: &[u8], offset: &mut usize) -> Option<usize> {
    let mut value = 0_usize;
    let mut shift = 0_u32;
    for byte_index in 0..3 {
        let byte = usize::from(*payload.get(*offset)?);
        *offset = (*offset).saturating_add(1);
        value |= (byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift = shift.saturating_add(7);
        if byte_index == 2 {
            return None;
        }
    }
    None
}

/// Decodes one Solana short-vector length prefix from start of `payload`.
///
/// Returns decoded length together with payload offset immediately
/// after prefix bytes.
#[must_use]
pub fn decode_short_u16_len_prefix(payload: &[u8]) -> Option<(usize, usize)> {
    let mut offset = 0;
    let value = decode_short_u16_len(payload, &mut offset)?;
    Some((value, offset))
}

/// Decodes one Solana short-vector length from possibly partial payload.
///
/// # Errors
///
/// Returns [`ShortVecDecodeError::Incomplete`] when payload ends before
/// length is fully decoded, and [`ShortVecDecodeError::Invalid`] for invalid
/// short-vector encoding.
pub fn decode_short_u16_len_partial(
    payload: &[u8],
    offset: &mut usize,
) -> Result<usize, ShortVecDecodeError> {
    let mut value = 0_usize;
    let mut shift = 0_u32;
    for byte_index in 0..3 {
        let byte = usize::from(
            *payload
                .get(*offset)
                .ok_or(ShortVecDecodeError::Incomplete)?,
        );
        *offset = (*offset)
            .checked_add(1)
            .ok_or(ShortVecDecodeError::Invalid)?;
        value |= (byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift = shift.saturating_add(7);
        if byte_index == 2 {
            return Err(ShortVecDecodeError::Invalid);
        }
    }
    Err(ShortVecDecodeError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::{
        ShortVecDecodeError, decode_short_u16_len, decode_short_u16_len_partial,
        decode_short_u16_len_prefix,
    };

    #[test]
    fn short_vec_decode_matches_compact_lengths() {
        let mut single_byte_offset = 0;
        assert_eq!(
            decode_short_u16_len(&[0x7f], &mut single_byte_offset),
            Some(127)
        );
        assert_eq!(single_byte_offset, 1);

        let mut two_byte_offset = 0;
        assert_eq!(
            decode_short_u16_len(&[0x80, 0x01], &mut two_byte_offset),
            Some(128)
        );
        assert_eq!(two_byte_offset, 2);
    }

    #[test]
    fn short_vec_decode_partial_distinguishes_incomplete_and_invalid() {
        let mut incomplete_offset = 0;
        assert_eq!(
            decode_short_u16_len_partial(&[0x80], &mut incomplete_offset),
            Err(ShortVecDecodeError::Incomplete)
        );

        let mut invalid_offset = 0;
        assert_eq!(
            decode_short_u16_len_partial(&[0x80, 0x80, 0x80], &mut invalid_offset),
            Err(ShortVecDecodeError::Invalid)
        );
    }

    #[test]
    fn short_vec_decode_prefix_returns_offset() {
        assert_eq!(decode_short_u16_len_prefix(&[0x7f]), Some((127, 1)));
        assert_eq!(decode_short_u16_len_prefix(&[0x80, 0x01]), Some((128, 2)));
    }
}
