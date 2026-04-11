use std::time::Duration;

/// Reads a positive profiling iteration count from `SOF_PROFILE_ITERATIONS`.
#[must_use]
pub fn profile_iterations(default: usize) -> usize {
    crate::env_support::read_positive_usize("SOF_PROFILE_ITERATIONS", default)
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
