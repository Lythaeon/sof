use super::*;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum RuntimeEntrypointError {
    #[error("failed to build tokio runtime: {source}")]
    BuildTokioRuntime { source: std::io::Error },
    #[error("runtime runloop failed: {reason}")]
    Runloop { reason: String },
}

pub(crate) fn run() -> Result<(), RuntimeEntrypointError> {
    run_with_plugin_host(PluginHostBuilder::new().build())
}

pub(crate) fn run_with_plugin_host(plugin_host: PluginHost) -> Result<(), RuntimeEntrypointError> {
    let worker_threads = read_worker_threads();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .max_blocking_threads(worker_threads.saturating_mul(8))
        .enable_all()
        .build()
        .map_err(|source| RuntimeEntrypointError::BuildTokioRuntime { source })?
        .block_on(run_async_with_plugin_host(plugin_host))
}

pub(crate) async fn run_async() -> Result<(), RuntimeEntrypointError> {
    run_async_with_plugin_host(PluginHostBuilder::new().build()).await
}

pub(crate) async fn run_async_with_plugin_host(
    plugin_host: PluginHost,
) -> Result<(), RuntimeEntrypointError> {
    runloop::run_async_with_plugin_host(plugin_host)
        .await
        .map_err(|source| RuntimeEntrypointError::Runloop {
            reason: source.to_string(),
        })
}
