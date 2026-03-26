use std::{future::Future, pin::Pin};

use super::*;
use thiserror::Error;

type ShutdownSignal = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

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
    let runtime_current_thread = read_runtime_current_thread();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if runtime_current_thread {
            return run_with_fresh_runtime_thread(plugin_host, extension_host, derived_state_host);
        }
        return match handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(|| {
                handle.block_on(run_async_with_hosts(
                    plugin_host,
                    extension_host,
                    derived_state_host,
                    None,
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
                    None,
                ))
            }),
        };
    }

    if runtime_current_thread {
        pin_runtime_thread_if_requested();
        let worker_threads = read_worker_threads();
        return tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(worker_threads.saturating_mul(8))
            .enable_all()
            .build()
            .map_err(|source| RuntimeEntrypointError::BuildTokioRuntime { source })?
            .block_on(run_async_with_hosts(
                plugin_host,
                extension_host,
                derived_state_host,
                None,
            ));
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
            None,
        ))
}

fn run_with_fresh_runtime_thread(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
) -> Result<(), RuntimeEntrypointError> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let spawn_result = std::thread::Builder::new()
        .name("sof-runtime-main".to_owned())
        .spawn(move || {
            pin_runtime_thread_if_requested();
            let worker_threads = read_worker_threads();
            let result = tokio::runtime::Builder::new_current_thread()
                .max_blocking_threads(worker_threads.saturating_mul(8))
                .enable_all()
                .build()
                .map_err(|source| RuntimeEntrypointError::BuildTokioRuntime { source })
                .and_then(|runtime| {
                    runtime.block_on(run_async_with_hosts(
                        plugin_host,
                        extension_host,
                        derived_state_host,
                        None,
                    ))
                });
            drop(tx.send(result));
        });
    if let Err(source) = spawn_result {
        return Err(RuntimeEntrypointError::BuildTokioRuntime { source });
    }
    rx.recv()
        .map_err(|source| RuntimeEntrypointError::Runloop {
            reason: format!("failed to receive runtime result from helper thread: {source}"),
        })?
}

fn pin_runtime_thread_if_requested() {
    let Some(core_index) = read_runtime_core() else {
        return;
    };
    let Some(core_ids) = core_affinity::get_core_ids() else {
        tracing::warn!(
            core_index,
            "failed to query CPU core ids for runtime pinning"
        );
        return;
    };
    let Some(core_slot) = core_index.checked_rem(core_ids.len()) else {
        tracing::warn!(
            core_index,
            "runtime core index modulo failed for selected core set"
        );
        return;
    };
    let Some(core_id) = core_ids.get(core_slot).copied() else {
        tracing::warn!(core_index, "runtime core index resolved to empty core set");
        return;
    };
    if core_affinity::set_for_current(core_id) {
        tracing::info!(
            core_index,
            assigned_core = core_id.id,
            "pinned runtime thread to CPU core"
        );
    } else {
        tracing::warn!(
            core_index,
            assigned_core = core_id.id,
            "failed to pin runtime thread to CPU core"
        );
    }
}

pub(crate) async fn run_async() -> Result<(), RuntimeEntrypointError> {
    run_async_with_hosts(
        PluginHostBuilder::new().build(),
        RuntimeExtensionHostBuilder::new().build(),
        DerivedStateHost::builder().build(),
        None,
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
        None,
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
        None,
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
        None,
    )
    .await
}

pub(crate) async fn run_async_with_hosts(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
    shutdown_signal: Option<ShutdownSignal>,
) -> Result<(), RuntimeEntrypointError> {
    run_async_with_hosts_and_optional_shutdown(
        plugin_host,
        extension_host,
        derived_state_host,
        shutdown_signal,
    )
    .await
}

async fn run_async_with_hosts_and_optional_shutdown(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
    shutdown_signal: Option<ShutdownSignal>,
) -> Result<(), RuntimeEntrypointError> {
    let observability = if let Some(bind_addr) = read_observability_bind_addr() {
        let service = RuntimeObservabilityService::start(
            bind_addr,
            plugin_host.clone(),
            extension_host.clone(),
            derived_state_host.clone(),
        )
        .await
        .map_err(|source| RuntimeEntrypointError::Runloop {
            reason: format!("failed to start observability endpoint on {bind_addr}: {source}"),
        })?;
        tracing::info!(
            bind_addr = %service.local_addr(),
            "runtime observability endpoint enabled"
        );
        Some(service)
    } else {
        None
    };
    let observability_handle = observability
        .as_ref()
        .map(RuntimeObservabilityService::handle)
        .cloned();
    let result = runloop::run_async_with_hosts(
        plugin_host,
        extension_host,
        derived_state_host,
        shutdown_signal,
        observability_handle,
    )
    .await;
    if let Some(service) = observability {
        service.shutdown().await;
    }
    result.map_err(|source| RuntimeEntrypointError::Runloop {
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
        None,
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
        None,
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
        None,
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
        None,
        packet_ingest_rx,
    )
    .await
}

#[cfg(feature = "kernel-bypass")]
pub(crate) async fn run_async_with_hosts_and_kernel_bypass_ingress(
    plugin_host: PluginHost,
    extension_host: RuntimeExtensionHost,
    derived_state_host: DerivedStateHost,
    shutdown_signal: Option<ShutdownSignal>,
    packet_ingest_rx: ingest::RawPacketBatchReceiver,
) -> Result<(), RuntimeEntrypointError> {
    let observability = if let Some(bind_addr) = read_observability_bind_addr() {
        let service = RuntimeObservabilityService::start(
            bind_addr,
            plugin_host.clone(),
            extension_host.clone(),
            derived_state_host.clone(),
        )
        .await
        .map_err(|source| RuntimeEntrypointError::Runloop {
            reason: format!("failed to start observability endpoint on {bind_addr}: {source}"),
        })?;
        tracing::info!(
            bind_addr = %service.local_addr(),
            "runtime observability endpoint enabled"
        );
        Some(service)
    } else {
        None
    };
    let observability_handle = observability
        .as_ref()
        .map(RuntimeObservabilityService::handle)
        .cloned();
    let result = runloop::run_async_with_hosts_and_kernel_bypass_ingress(
        plugin_host,
        extension_host,
        derived_state_host,
        shutdown_signal,
        observability_handle,
        packet_ingest_rx,
    )
    .await;
    if let Some(service) = observability {
        service.shutdown().await;
    }
    result.map_err(|source| RuntimeEntrypointError::Runloop {
        reason: source.to_string(),
    })
}
