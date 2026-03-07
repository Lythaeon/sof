//! Error types for validated gossip and ingest tuning values.

use std::fmt;

/// Error returned when a tuning value violates a basic invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuningValueError {
    /// Capacity values must be non-zero.
    ZeroCapacity,
    /// Millisecond values must be positive.
    ZeroMillis,
    /// TVU socket count must be non-zero.
    ZeroSocketCount,
}

impl fmt::Display for TuningValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCapacity => f.write_str("capacity must be non-zero"),
            Self::ZeroMillis => f.write_str("duration must be positive"),
            Self::ZeroSocketCount => f.write_str("socket count must be non-zero"),
        }
    }
}

impl std::error::Error for TuningValueError {}
