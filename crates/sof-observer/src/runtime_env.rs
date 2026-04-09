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

    let _guard = acquire_test_runtime_env_lock_blocking();
    set_runtime_env_overrides(overrides);
    let result = catch_unwind(AssertUnwindSafe(f));
    clear_runtime_env_overrides();
    match result {
        Ok(value) => value,
        Err(payload) => resume_unwind(payload),
    }
}

#[cfg(test)]
fn test_runtime_env_lock() -> &'static tokio::sync::Mutex<()> {
    static TEST_ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

    TEST_ENV_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(test)]
pub(crate) fn acquire_test_runtime_env_lock_blocking() -> tokio::sync::MutexGuard<'static, ()> {
    test_runtime_env_lock().blocking_lock()
}

#[cfg(test)]
pub(crate) async fn with_runtime_env_overrides_for_test_async<T, Fut>(
    overrides: impl IntoIterator<Item = (String, String)>,
    f: impl FnOnce() -> Fut,
) -> T
where
    Fut: std::future::Future<Output = T>,
{
    struct ClearOnDrop;

    impl Drop for ClearOnDrop {
        fn drop(&mut self) {
            clear_runtime_env_overrides();
        }
    }

    let _guard = test_runtime_env_lock().lock().await;
    set_runtime_env_overrides(overrides);
    let _clear = ClearOnDrop;
    f().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[tokio::test]
    async fn sync_and_async_override_helpers_share_the_same_lock() {
        let (guard_ready_tx, guard_ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let guard_thread = thread::spawn(move || {
            let _guard = acquire_test_runtime_env_lock_blocking();
            guard_ready_tx
                .send(())
                .expect("guard-ready signal should send");
            release_rx.recv().expect("release signal should arrive");
        });

        guard_ready_rx
            .recv()
            .expect("guard-ready signal should arrive");

        let started = std::sync::Arc::new(tokio::sync::Notify::new());
        let finished = std::sync::Arc::new(tokio::sync::Notify::new());
        let started_wait = std::sync::Arc::clone(&started);
        let finished_wait = std::sync::Arc::clone(&finished);

        let waiter = tokio::spawn(async move {
            started_wait.notify_one();
            with_runtime_env_overrides_for_test_async(
                [(
                    String::from("SOF_PROVIDER_STREAM_ALLOW_EOF"),
                    String::from("true"),
                )],
                || async {},
            )
            .await;
            finished_wait.notify_one();
        });

        started.notified().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), finished.notified())
                .await
                .is_err(),
            "async helper should wait while the sync helper holds the shared lock"
        );

        release_tx.send(()).expect("release signal should send");
        waiter.await.expect("waiter task should join");
        guard_thread.join().expect("guard thread should join");
    }
}
