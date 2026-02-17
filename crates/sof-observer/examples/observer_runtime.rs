//! Minimal SOF runtime example without plugins.
#![doc(hidden)]

use thiserror::Error;

#[derive(Debug, Error)]
enum ObserverRuntimeExampleError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error(transparent)]
    Runtime(#[from] sof::runtime::RuntimeError),
}

const fn require_release_mode() -> Result<(), ObserverRuntimeExampleError> {
    if cfg!(debug_assertions) {
        return Err(ObserverRuntimeExampleError::ReleaseModeRequired {
            command: "cargo run --release -p sof --example observer_runtime",
        });
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), ObserverRuntimeExampleError> {
    require_release_mode()?;
    Ok(sof::runtime::run_async().await?)
}
