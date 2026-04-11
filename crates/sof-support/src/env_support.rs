use std::env;

/// Reads one positive `usize` from an environment variable, or returns `default`.
#[must_use]
pub fn read_positive_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}
