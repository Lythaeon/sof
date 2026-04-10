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
