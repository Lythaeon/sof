use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// Returns current Unix timestamp in milliseconds, saturating at `u64::MAX`.
#[must_use]
pub fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, duration_millis_u64)
}

/// Returns current Unix timestamp in whole seconds.
#[must_use]
pub fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// Returns current Unix timestamp in nanoseconds as one `u128`.
#[must_use]
pub fn current_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
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
}
