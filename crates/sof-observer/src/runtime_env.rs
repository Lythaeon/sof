use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

/// Global runtime setup overrides used to avoid mutating process env at runtime.
static ENV_OVERRIDES: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

/// Returns the lazily initialized runtime override map.
fn env_overrides() -> &'static RwLock<HashMap<String, String>> {
    ENV_OVERRIDES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Returns an environment variable, preferring runtime setup overrides.
pub(crate) fn read_env_var(name: &str) -> Option<String> {
    if let Ok(guard) = env_overrides().read()
        && let Some(value) = guard.get(name)
    {
        return Some(value.clone());
    }
    std::env::var(name).ok()
}

/// Replaces all runtime setup environment overrides.
pub(crate) fn set_runtime_env_overrides(overrides: impl IntoIterator<Item = (String, String)>) {
    let mut map = HashMap::new();
    map.extend(overrides);
    if let Ok(mut guard) = env_overrides().write() {
        *guard = map;
    }
}

/// Clears all runtime setup environment overrides.
pub(crate) fn clear_runtime_env_overrides() {
    if let Ok(mut guard) = env_overrides().write() {
        guard.clear();
    }
}
