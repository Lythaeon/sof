use super::ParseError;

#[inline]
pub(super) const fn ensure_len(packet: &[u8], minimum: usize) -> Result<(), ParseError> {
    if packet.len() < minimum {
        return Err(ParseError::PacketTooShort {
            actual: packet.len(),
            minimum,
        });
    }
    Ok(())
}

#[inline]
pub(super) fn read_u8(packet: &[u8], offset: usize) -> Result<u8, ParseError> {
    packet
        .get(offset)
        .copied()
        .ok_or_else(|| ParseError::PacketTooShort {
            actual: packet.len(),
            minimum: offset.saturating_add(1),
        })
}

#[inline]
pub(super) fn read_u16_le(packet: &[u8], offset: usize) -> Result<u16, ParseError> {
    let bytes = packet
        .get(offset..offset.saturating_add(2))
        .ok_or_else(|| ParseError::PacketTooShort {
            actual: packet.len(),
            minimum: offset.saturating_add(2),
        })?;
    let bytes: [u8; 2] = bytes
        .try_into()
        .map_err(|_error| ParseError::PacketTooShort {
            actual: packet.len(),
            minimum: offset.saturating_add(2),
        })?;
    Ok(u16::from_le_bytes(bytes))
}

#[inline]
pub(super) fn read_u32_le(packet: &[u8], offset: usize) -> Result<u32, ParseError> {
    let bytes = packet
        .get(offset..offset.saturating_add(4))
        .ok_or_else(|| ParseError::PacketTooShort {
            actual: packet.len(),
            minimum: offset.saturating_add(4),
        })?;
    let bytes: [u8; 4] = bytes
        .try_into()
        .map_err(|_error| ParseError::PacketTooShort {
            actual: packet.len(),
            minimum: offset.saturating_add(4),
        })?;
    Ok(u32::from_le_bytes(bytes))
}

#[inline]
pub(super) fn read_u64_le(packet: &[u8], offset: usize) -> Result<u64, ParseError> {
    let bytes = packet
        .get(offset..offset.saturating_add(8))
        .ok_or_else(|| ParseError::PacketTooShort {
            actual: packet.len(),
            minimum: offset.saturating_add(8),
        })?;
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_error| ParseError::PacketTooShort {
            actual: packet.len(),
            minimum: offset.saturating_add(8),
        })?;
    Ok(u64::from_le_bytes(bytes))
}
