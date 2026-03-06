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
    run_with_hosts(
        PluginHostBuilder::new().build(),
        RuntimeExtensionHostBuilder::new().build(),
        DerivedStateHost::builder().build(),
    )
}

pub(crate) fn run_with_plugin_host(plugin_host: PluginHost) -> Result<(), RuntimeEntrypointError> {
    run_with_hosts(
        plugin_host,
        RuntimeExtensionHostBuilder::new().build(),
        DerivedStateHost::builder().build(),
    )
}

pub(crate) fn run_with_extension_host(
    extension_host: RuntimeExtensionHost,
) -> Result<(), RuntimeEntrypointError> {
    run_with_hosts(
        PluginHostBuilder::new().build(),
        extension_host,
        DerivedStateHost::builder().build(),
    )
}

pub(crate) fn run_with_derived_state_host(
    derived_state_host: DerivedStateHost,
) -> Result<(), RuntimeEntrypointError> {
    run_with_hosts(
        PluginHostBuilder::new().build(),
        RuntimeExtensionHostBuilder::new().build(),
        derived_state_host,
    )
}

pub(crate) fn run_with_hosts(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
) -> Result<(), RuntimeEntrypointError> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        return match handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(|| {
                handle.block_on(run_async_with_hosts(
                    plugin_host,
                    extension_host,
                    derived_state_host,
                ))
            }),
            tokio::runtime::RuntimeFlavor::CurrentThread => {
                // Current-thread runtimes cannot use `block_in_place`. Avoid nested-runtime
                // panics by running this sync wrapper on a dedicated OS thread.
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                let spawn_result = std::thread::Builder::new()
                    .name("sof-runtime-entrypoint".to_owned())
                    .spawn(move || {
                        let result =
                            run_with_hosts(plugin_host, extension_host, derived_state_host);
                        drop(tx.send(result));
                    });
                if let Err(source) = spawn_result {
                    return Err(RuntimeEntrypointError::BuildTokioRuntime { source });
                }
                rx.recv()
                    .map_err(|source| RuntimeEntrypointError::Runloop {
                        reason: format!(
                            "failed to receive runtime result from helper thread: {source}"
                        ),
                    })?
            }
            _ => tokio::task::block_in_place(|| {
                handle.block_on(run_async_with_hosts(
                    plugin_host,
                    extension_host,
                    derived_state_host,
                ))
            }),
        };
    }

    let worker_threads = read_worker_threads();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .max_blocking_threads(worker_threads.saturating_mul(8))
        .enable_all()
        .build()
        .map_err(|source| RuntimeEntrypointError::BuildTokioRuntime { source })?
        .block_on(run_async_with_hosts(
            plugin_host,
            extension_host,
            derived_state_host,
        ))
}

pub(crate) async fn run_async() -> Result<(), RuntimeEntrypointError> {
    run_async_with_hosts(
        PluginHostBuilder::new().build(),
        RuntimeExtensionHostBuilder::new().build(),
        DerivedStateHost::builder().build(),
    )
    .await
}

pub(crate) async fn run_async_with_plugin_host(
    plugin_host: PluginHost,
) -> Result<(), RuntimeEntrypointError> {
    run_async_with_hosts(
        plugin_host,
        RuntimeExtensionHostBuilder::new().build(),
        DerivedStateHost::builder().build(),
    )
    .await
}

pub(crate) async fn run_async_with_extension_host(
    extension_host: RuntimeExtensionHost,
) -> Result<(), RuntimeEntrypointError> {
    run_async_with_hosts(
        PluginHostBuilder::new().build(),
        extension_host,
        DerivedStateHost::builder().build(),
    )
    .await
}

pub(crate) async fn run_async_with_derived_state_host(
    derived_state_host: DerivedStateHost,
) -> Result<(), RuntimeEntrypointError> {
    run_async_with_hosts(
        PluginHostBuilder::new().build(),
        RuntimeExtensionHostBuilder::new().build(),
        derived_state_host,
    )
    .await
}

pub(crate) async fn run_async_with_hosts(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
) -> Result<(), RuntimeEntrypointError> {
    runloop::run_async_with_hosts(plugin_host, extension_host, derived_state_host)
        .await
        .map_err(|source| RuntimeEntrypointError::Runloop {
            reason: source.to_string(),
        })
}

#[cfg(feature = "kernel-bypass")]
pub(crate) async fn run_async_with_kernel_bypass_ingress(
    packet_ingest_rx: ingest::RawPacketBatchReceiver,
) -> Result<(), RuntimeEntrypointError> {
    run_async_with_hosts_and_kernel_bypass_ingress(
        PluginHostBuilder::new().build(),
        RuntimeExtensionHostBuilder::new().build(),
        DerivedStateHost::builder().build(),
        packet_ingest_rx,
    )
    .await
}

#[cfg(feature = "kernel-bypass")]
pub(crate) async fn run_async_with_plugin_host_and_kernel_bypass_ingress(
    plugin_host: PluginHost,
    packet_ingest_rx: ingest::RawPacketBatchReceiver,
) -> Result<(), RuntimeEntrypointError> {
    run_async_with_hosts_and_kernel_bypass_ingress(
        plugin_host,
        RuntimeExtensionHostBuilder::new().build(),
        DerivedStateHost::builder().build(),
        packet_ingest_rx,
    )
    .await
}

#[cfg(feature = "kernel-bypass")]
pub(crate) async fn run_async_with_extension_host_and_kernel_bypass_ingress(
    extension_host: RuntimeExtensionHost,
    packet_ingest_rx: ingest::RawPacketBatchReceiver,
) -> Result<(), RuntimeEntrypointError> {
    run_async_with_hosts_and_kernel_bypass_ingress(
        PluginHostBuilder::new().build(),
        extension_host,
        DerivedStateHost::builder().build(),
        packet_ingest_rx,
    )
    .await
}

#[cfg(feature = "kernel-bypass")]
pub(crate) async fn run_async_with_derived_state_host_and_kernel_bypass_ingress(
    derived_state_host: DerivedStateHost,
    packet_ingest_rx: ingest::RawPacketBatchReceiver,
) -> Result<(), RuntimeEntrypointError> {
    run_async_with_hosts_and_kernel_bypass_ingress(
        PluginHostBuilder::new().build(),
        RuntimeExtensionHostBuilder::new().build(),
        derived_state_host,
        packet_ingest_rx,
    )
    .await
}

#[cfg(feature = "kernel-bypass")]
pub(crate) async fn run_async_with_hosts_and_kernel_bypass_ingress(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
    packet_ingest_rx: ingest::RawPacketBatchReceiver,
) -> Result<(), RuntimeEntrypointError> {
    runloop::run_async_with_hosts_and_kernel_bypass_ingress(
        plugin_host,
        extension_host,
        derived_state_host,
        packet_ingest_rx,
    )
    .await
    .map_err(|source| RuntimeEntrypointError::Runloop {
        reason: source.to_string(),
    })
}
