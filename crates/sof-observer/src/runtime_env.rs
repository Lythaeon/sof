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

#[cfg(test)]
pub(crate) fn with_runtime_env_overrides_for_test<T>(
    overrides: impl IntoIterator<Item = (String, String)>,
    f: impl FnOnce() -> T,
) -> T {
    use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

    let _guard = acquire_test_runtime_env_lock();
    set_runtime_env_overrides(overrides);
    let result = catch_unwind(AssertUnwindSafe(f));
    clear_runtime_env_overrides();
    match result {
        Ok(value) => value,
        Err(payload) => resume_unwind(payload),
    }
}

#[cfg(test)]
pub(crate) fn acquire_test_runtime_env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::Mutex;

    static TEST_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    TEST_ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("test runtime env lock should not be poisoned")
}

#[cfg(test)]
pub(crate) async fn with_runtime_env_overrides_for_test_async<T, Fut>(
    overrides: impl IntoIterator<Item = (String, String)>,
    f: impl FnOnce() -> Fut,
) -> T
where
    Fut: std::future::Future<Output = T>,
{
    use tokio::sync::Mutex;

    static TEST_ENV_ASYNC_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    struct ClearOnDrop;

    impl Drop for ClearOnDrop {
        fn drop(&mut self) {
            clear_runtime_env_overrides();
        }
    }

    let _guard = TEST_ENV_ASYNC_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .await;
    set_runtime_env_overrides(overrides);
    let _clear = ClearOnDrop;
    f().await
}
