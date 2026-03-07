//! Validated newtypes used by the gossip and ingest tuning surface.

use std::{
    num::{NonZeroU32, NonZeroUsize},
    time::Duration,
};

use crate::error::TuningValueError;

/// Typed wrapper for bounded channel capacities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueueCapacity(NonZeroU32);

impl QueueCapacity {
    /// Creates a validated queue capacity.
    ///
    /// # Errors
    /// Returns [`TuningValueError::ZeroCapacity`] when `value` is zero.
    pub fn new(value: u32) -> Result<Self, TuningValueError> {
        NonZeroU32::new(value)
            .map(Self)
            .ok_or(TuningValueError::ZeroCapacity)
    }

    /// Returns the raw capacity value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }

    /// Builds one fixed queue capacity for internal presets.
    pub(crate) const fn fixed(value: u32) -> Self {
        match NonZeroU32::new(value) {
            Some(value) => Self(value),
            None => Self(NonZeroU32::MIN),
        }
    }
}

/// Typed wrapper for a millisecond coalesce window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReceiverCoalesceWindow(Duration);

impl ReceiverCoalesceWindow {
    /// Creates a validated coalesce window from milliseconds.
    ///
    /// # Errors
    /// Returns [`TuningValueError::ZeroMillis`] when `value` is zero.
    pub const fn from_millis(value: u64) -> Result<Self, TuningValueError> {
        if value == 0 {
            return Err(TuningValueError::ZeroMillis);
        }
        Ok(Self(Duration::from_millis(value)))
    }

    /// Returns the value in milliseconds.
    #[must_use]
    pub const fn as_millis_u64(self) -> u64 {
        self.0.as_millis() as u64
    }

    /// Returns the underlying duration.
    #[must_use]
    pub const fn as_duration(self) -> Duration {
        self.0
    }

    /// Builds one fixed coalesce window for internal presets.
    pub(crate) const fn fixed(value: u64) -> Self {
        Self(Duration::from_millis(if value == 0 { 1 } else { value }))
    }
}

/// Typed wrapper for a CPU core index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CpuCoreIndex(usize);

impl CpuCoreIndex {
    /// Creates a CPU core index.
    #[must_use]
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the raw zero-based index.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Number of TVU receive sockets to create during gossip bootstrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TvuReceiveSocketCount(NonZeroUsize);

impl TvuReceiveSocketCount {
    /// Creates a validated TVU socket count.
    ///
    /// # Errors
    /// Returns [`TuningValueError::ZeroSocketCount`] when `value` is zero.
    pub fn new(value: usize) -> Result<Self, TuningValueError> {
        NonZeroUsize::new(value)
            .map(Self)
            .ok_or(TuningValueError::ZeroSocketCount)
    }

    /// Returns the raw socket count.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }

    /// Builds one fixed TVU socket count for internal presets.
    pub(crate) const fn fixed(value: usize) -> Self {
        match NonZeroUsize::new(value) {
            Some(value) => Self(value),
            None => Self(NonZeroUsize::MIN),
        }
    }
}
