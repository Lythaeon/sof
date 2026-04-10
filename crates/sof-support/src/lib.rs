//! Shared internal support helpers for SOF workspace crates.

use std::{env, time::Duration};

use sof_types::{PubkeyBytes, SignatureBytes};

/// Benchmark helper utilities reused across SOF profiling fixtures.
pub mod bench {
    use super::Duration;

    /// Reads a positive profiling iteration count from `SOF_PROFILE_ITERATIONS`.
    #[must_use]
    pub fn profile_iterations(default: usize) -> usize {
        super::env_support::read_positive_usize("SOF_PROFILE_ITERATIONS", default)
    }

    /// Returns the average nanoseconds spent per iteration.
    #[must_use]
    pub fn avg_ns_per_iteration<I>(elapsed: Duration, iterations: I) -> u128
    where
        I: TryInto<u128>,
    {
        let iterations = iterations
            .try_into()
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(1);
        elapsed.as_nanos().checked_div(iterations).unwrap_or(0)
    }
}

/// Environment parsing helpers reused across profiling fixtures and tests.
pub mod env_support {
    use super::env;

    /// Reads one positive `usize` from an environment variable, or returns `default`.
    #[must_use]
    pub fn read_positive_usize(name: &str, default: usize) -> usize {
        env::var(name)
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(default)
    }
}

/// Collection helpers reused across runtime caches and provider adapters.
pub mod collections_support {
    use std::collections::HashMap;

    /// Prunes one slot-keyed map down to the retained recent window once the threshold is crossed.
    pub fn prune_recent_slots<T>(
        slot_states: &mut HashMap<u64, T>,
        slot: u64,
        retained_lag: u64,
        prune_threshold: usize,
    ) {
        if slot_states.len() <= prune_threshold {
            return;
        }
        let slot_floor = slot.saturating_sub(retained_lag);
        slot_states.retain(|tracked_slot, _| *tracked_slot >= slot_floor);
    }
}

/// Typed byte-slice conversion helpers reused across provider adapters.
pub mod bytes {
    use super::{PubkeyBytes, SignatureBytes};

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
}

/// Short-vector parsing helpers reused across serialized Solana payload readers.
pub mod short_vec {
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

    /// Decodes one Solana short-vector length prefix from the start of `payload`.
    ///
    /// Returns the decoded length together with the payload offset immediately
    /// after the prefix bytes.
    #[must_use]
    pub fn decode_short_u16_len_prefix(payload: &[u8]) -> Option<(usize, usize)> {
        let mut offset = 0;
        let value = decode_short_u16_len(payload, &mut offset)?;
        Some((value, offset))
    }

    /// Decodes one Solana short-vector length from a possibly partial payload.
    ///
    /// # Errors
    ///
    /// Returns [`ShortVecDecodeError::Incomplete`] when the payload ends before
    /// the length is fully decoded, and [`ShortVecDecodeError::Invalid`] for an
    /// invalid short-vector encoding.
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
}

/// Duration helpers reused across transport adapters.
pub mod time_support {
    use super::Duration;

    /// Returns whole seconds rounded up, preserving non-zero sub-second values.
    #[must_use]
    pub const fn duration_secs_ceil(duration: Duration) -> u64 {
        let secs = duration.as_secs();
        if duration.subsec_nanos() == 0 {
            secs
        } else {
            secs.saturating_add(1)
        }
    }

    /// Returns one duration in whole milliseconds, saturating at `u64::MAX`.
    #[must_use]
    pub fn duration_millis_u64(duration: Duration) -> u64 {
        duration.as_millis().min(u128::from(u64::MAX)) as u64
    }

    /// Returns `duration` unless it is zero, in which case returns `fallback`.
    #[must_use]
    pub const fn nonzero_duration_or(duration: Duration, fallback: Duration) -> Duration {
        if duration.is_zero() {
            fallback
        } else {
            duration
        }
    }

    /// Returns the current Unix timestamp in milliseconds, saturating at `u64::MAX`.
    #[must_use]
    pub fn current_unix_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, duration_millis_u64)
    }

    /// Returns the current Unix timestamp in whole seconds.
    #[must_use]
    pub fn current_unix_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs())
    }

    /// Returns the current Unix timestamp in nanoseconds as one `u128`.
    #[must_use]
    pub fn current_unix_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::collections_support::prune_recent_slots;
    use super::short_vec::{
        ShortVecDecodeError, decode_short_u16_len, decode_short_u16_len_partial,
        decode_short_u16_len_prefix,
    };
    use super::time_support::{
        current_unix_ms, current_unix_nanos, current_unix_secs, duration_millis_u64,
        duration_secs_ceil, nonzero_duration_or,
    };

    #[test]
    fn duration_secs_ceil_rounds_subsecond_values_up() {
        assert_eq!(duration_secs_ceil(Duration::from_secs(2)), 2);
        assert_eq!(duration_secs_ceil(Duration::from_millis(1)), 1);
        assert_eq!(duration_secs_ceil(Duration::from_millis(1500)), 2);
    }

    #[test]
    fn current_unix_ms_is_monotonic_enough_for_smoke_check() {
        let first = current_unix_ms();
        let second = current_unix_ms();
        assert!(second >= first);
    }

    #[test]
    fn duration_millis_u64_saturates() {
        assert_eq!(duration_millis_u64(Duration::from_millis(7)), 7);
        assert_eq!(duration_millis_u64(Duration::MAX), u64::MAX);
    }

    #[test]
    fn nonzero_duration_or_clamps_zero() {
        assert_eq!(
            nonzero_duration_or(Duration::ZERO, Duration::from_secs(7)),
            Duration::from_secs(7)
        );
        assert_eq!(
            nonzero_duration_or(Duration::from_millis(5), Duration::from_secs(7)),
            Duration::from_millis(5)
        );
    }

    #[test]
    fn current_unix_time_helpers_are_nonzero_or_zero_safely() {
        assert!(current_unix_secs() <= current_unix_ms() / 1_000 + 1);
        assert!(current_unix_nanos() / 1_000_000 <= u128::from(current_unix_ms()) + 1);
    }

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

    #[test]
    fn prune_recent_slots_drops_old_entries_after_threshold() {
        let mut slot_states = (0_u64..10_u64).map(|slot| (slot, slot)).collect();

        prune_recent_slots(&mut slot_states, 9, 3, 4);

        assert_eq!(slot_states.len(), 4);
        assert!(!slot_states.contains_key(&5));
        assert!(slot_states.contains_key(&6));
        assert!(slot_states.contains_key(&9));
    }
}
