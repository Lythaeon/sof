//! Native runtime host used by the TypeScript SDK `App.run()` handoff.

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
#[path = "sof_ts_runtime_host/af_xdp.rs"]
mod af_xdp;

#[cfg(feature = "provider-grpc")]
use std::str::FromStr;
use std::sync::Arc;
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
use std::time::Duration;
use std::{collections::HashMap, env, fs, net::SocketAddr, path::PathBuf};

use async_trait::async_trait;
#[cfg(feature = "provider-grpc")]
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
use sof::provider_stream::websocket::{
    WebsocketLogsConfig, WebsocketLogsFilter, WebsocketPrimaryStream,
    WebsocketTransactionCommitment, WebsocketTransactionConfig, spawn_websocket_logs_source,
    spawn_websocket_source,
};
#[cfg(feature = "provider-grpc")]
use sof::provider_stream::yellowstone::{
    YellowstoneGrpcCommitment, YellowstoneGrpcConfig, YellowstoneGrpcSlotsConfig,
    YellowstoneGrpcStream, spawn_yellowstone_grpc_slot_source, spawn_yellowstone_grpc_source,
};
#[cfg(feature = "provider-grpc")]
use sof::provider_stream::{
    ProviderStreamMode, ProviderStreamSender, create_provider_stream_queue,
};
#[cfg(feature = "gossip-bootstrap")]
use sof::runtime::GossipRuntimeMode;
#[cfg(feature = "provider-grpc")]
use sof::{
    event::{ForkSlotStatus, TxCommitmentStatus, TxKind},
    framework::{
        AccountUpdateEvent, BlockMetaEvent, ObservedRecentBlockhashEvent, ObserverPlugin,
        PluginConfig, PluginContext, PluginHost, PluginSetupError, SlotStatusEvent,
        TransactionEvent, TransactionLogEvent, TransactionStatusEvent,
    },
    provider_stream::{
        ProviderSourceArbitrationMode, ProviderSourceReadiness, ProviderSourceRef,
        ProviderSourceRole,
    },
};
use sof::{
    framework::{
        ExtensionCapability, ExtensionContext, ExtensionManifest, ExtensionResourceSpec,
        ExtensionSetupError, ExtensionStreamVisibility, PacketSubscription, RuntimeExtension,
        RuntimeExtensionHost, RuntimePacketEvent, RuntimePacketEventClass, RuntimePacketSourceKind,
        RuntimePacketTransport, RuntimeWebSocketFrameType, TcpConnectorSpec, TcpListenerSpec,
        UdpListenerSpec, WsConnectorSpec,
    },
    runtime::{ObserverRuntime, RuntimeError, RuntimeSetup},
};
#[cfg(feature = "provider-grpc")]
use solana_pubkey::Pubkey;
#[cfg(feature = "provider-grpc")]
use solana_signature::Signature;
#[cfg(feature = "provider-grpc")]
use tokio::task::JoinHandle;
#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
use tokio::task::spawn_blocking;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

/// Wire tag for websocket ingress handoff.
const INGRESS_KIND_WEB_SOCKET: u8 = 1;
/// Wire tag for Yellowstone gRPC ingress handoff.
const INGRESS_KIND_GRPC: u8 = 2;
/// Wire tag for gossip bootstrap ingress handoff.
const INGRESS_KIND_GOSSIP: u8 = 3;
/// Wire tag for direct raw-shred ingress handoff.
const INGRESS_KIND_DIRECT_SHREDS: u8 = 4;
#[cfg(feature = "provider-grpc")]
/// Wire tag for Yellowstone transaction stream selection.
const GRPC_STREAM_TRANSACTIONS: u8 = 1;
#[cfg(feature = "provider-grpc")]
/// Wire tag for Yellowstone transaction-status stream selection.
const GRPC_STREAM_TRANSACTION_STATUS: u8 = 2;
#[cfg(feature = "provider-grpc")]
/// Wire tag for Yellowstone account stream selection.
const GRPC_STREAM_ACCOUNTS: u8 = 3;
#[cfg(feature = "provider-grpc")]
/// Wire tag for Yellowstone block-meta stream selection.
const GRPC_STREAM_BLOCK_META: u8 = 4;
#[cfg(feature = "provider-grpc")]
/// Wire tag for Yellowstone slot stream selection.
const GRPC_STREAM_SLOTS: u8 = 5;
#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
/// Wire tag for websocket `transactionSubscribe`.
const WEBSOCKET_STREAM_TRANSACTIONS: u8 = 1;
#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
/// Wire tag for websocket `logsSubscribe`.
const WEBSOCKET_STREAM_LOGS: u8 = 2;
#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
/// Wire tag for websocket `accountSubscribe`.
const WEBSOCKET_STREAM_ACCOUNT: u8 = 3;
#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
/// Wire tag for websocket `programSubscribe`.
const WEBSOCKET_STREAM_PROGRAM: u8 = 4;
#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
/// Wire tag for websocket logs `all` filter.
const WEBSOCKET_LOGS_FILTER_ALL: u8 = 1;
#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
/// Wire tag for websocket logs `allWithVotes` filter.
const WEBSOCKET_LOGS_FILTER_ALL_WITH_VOTES: u8 = 2;
#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
/// Wire tag for websocket logs `mentions` filter.
const WEBSOCKET_LOGS_FILTER_MENTIONS: u8 = 3;
#[cfg(feature = "provider-grpc")]
/// Provider-stream channel capacity used by the native host.
const PROVIDER_STREAM_QUEUE_CAPACITY: usize = 4096;
/// Worker response tag for manifest delivery.
const RESPONSE_TAG_MANIFEST: u8 = 1;
/// Worker response tag for startup acknowledgement.
const RESPONSE_TAG_STARTED: u8 = 2;
/// Worker response tag for packet delivery acknowledgement.
const RESPONSE_TAG_EVENT_HANDLED: u8 = 3;
/// Worker response tag for shutdown acknowledgement.
const RESPONSE_TAG_SHUTDOWN_COMPLETE: u8 = 4;
#[cfg(feature = "provider-grpc")]
/// Worker response tag for provider-event acknowledgement.
const RESPONSE_TAG_PROVIDER_EVENT_HANDLED: u8 = 5;
/// Successful worker result tag.
const RESULT_TAG_OK: u8 = 1;
/// Error worker result tag.
const RESULT_TAG_ERR: u8 = 2;

/// Errors returned by the native TypeScript runtime host.
#[derive(Debug, thiserror::Error)]
enum HostError {
    /// No config path argument was supplied on the command line.
    #[error("usage: sof_ts_runtime_host <config-json-path>")]
    MissingConfigPath,
    /// Reading the host config file failed.
    #[error("failed to read runtime host config {path}: {source}")]
    ReadConfig {
        /// Path that failed to load.
        path: PathBuf,
        /// Underlying I/O failure.
        source: std::io::Error,
    },
    /// Parsing the host config JSON failed.
    #[error("failed to parse runtime host config {path}: {source}")]
    ParseConfig {
        /// Path that failed to parse.
        path: PathBuf,
        /// Underlying JSON parse failure.
        source: serde_json::Error,
    },
    /// The provided config is structurally valid JSON but semantically invalid.
    #[error("{0}")]
    InvalidConfig(String),
    #[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
    /// The AF_XDP kernel-bypass producer failed.
    #[error("kernel-bypass producer failed: {0}")]
    KernelBypass(String),
    /// The observer runtime failed while running.
    #[error("runtime failed: {0}")]
    Runtime(#[from] RuntimeError),
}

/// Top-level JSON payload handed from the TypeScript SDK into the native host.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeHostConfig {
    /// Stable app name used for environment and observability.
    app_name: String,
    /// Runtime environment variables derived from the TypeScript runtime config.
    runtime_environment: HashMap<String, String>,
    /// Ingress sources delegated to the native host.
    ingress: Vec<IngressConfig>,
    #[cfg(feature = "provider-grpc")]
    /// Provider fan-in policy for multi-source provider ingress.
    fan_in: Option<FanInConfig>,
    /// Plugin workers launched through the stdio worker bridge.
    plugin_workers: Vec<PluginWorkerConfig>,
}

#[cfg(feature = "provider-grpc")]
/// JSON wire representation of provider fan-in arbitration.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FanInConfig {
    /// Arbitration strategy selected by the TypeScript SDK.
    strategy: u8,
}

/// JSON wire representation for one ingress source.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IngressConfig {
    /// Ingress kind discriminator.
    kind: u8,
    /// Stable ingress instance name.
    name: String,
    /// Optional bind address for raw ingress runtime setup.
    bind_address: Option<String>,
    #[cfg(feature = "provider-grpc")]
    /// Provider endpoint URL.
    endpoint: Option<String>,
    #[cfg(feature = "provider-grpc")]
    /// Provider stream family discriminator.
    stream: Option<u8>,
    #[cfg(feature = "provider-grpc")]
    /// Optional Yellowstone x-token.
    x_token: Option<String>,
    #[cfg(feature = "provider-grpc")]
    /// Optional provider commitment policy.
    commitment: Option<u8>,
    #[cfg(feature = "provider-grpc")]
    /// Optional Yellowstone vote filter.
    vote: Option<bool>,
    #[cfg(feature = "provider-grpc")]
    /// Optional Yellowstone failed filter.
    failed: Option<bool>,
    #[cfg(feature = "provider-grpc")]
    /// Optional signature filter.
    signature: Option<String>,
    #[cfg(feature = "provider-grpc")]
    /// Account-include filter list.
    account_include: Option<Vec<String>>,
    #[cfg(feature = "provider-grpc")]
    /// Account-exclude filter list.
    account_exclude: Option<Vec<String>>,
    #[cfg(feature = "provider-grpc")]
    /// Account-required filter list.
    account_required: Option<Vec<String>>,
    #[cfg(feature = "provider-grpc")]
    /// Explicit account selector list.
    accounts: Option<Vec<String>>,
    #[cfg(feature = "provider-grpc")]
    /// Explicit owner selector list.
    owners: Option<Vec<String>>,
    #[cfg(feature = "provider-grpc")]
    /// Whether a transaction signature must be present before dispatch.
    require_transaction_signature: Option<bool>,
    #[cfg(feature = "provider-grpc")]
    /// Source readiness policy inside provider fan-in.
    readiness: Option<u8>,
    #[cfg(feature = "provider-grpc")]
    /// Source role inside provider fan-in.
    role: Option<u8>,
    #[cfg(feature = "provider-grpc")]
    /// Explicit source priority inside provider fan-in.
    priority: Option<u16>,
    /// Websocket URL for SDK-side validation errors.
    url: Option<String>,
    #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
    /// Account pubkey for `accountSubscribe`.
    account: Option<String>,
    #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
    /// Program pubkey for `programSubscribe`.
    program_id: Option<String>,
    #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
    /// Logs filter selector for `logsSubscribe`.
    logs_filter: Option<u8>,
    #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
    /// Mention pubkey for websocket logs mention filters.
    mentions: Option<String>,
    #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
    /// Legacy custom JSON-RPC subscribe messages rejected by the native host.
    requests: Option<Vec<String>>,
    /// Gossip bootstrap entrypoints.
    entrypoints: Option<Vec<String>>,
    #[cfg(feature = "gossip-bootstrap")]
    /// Gossip runtime mode selector.
    runtime_mode: Option<u8>,
    /// Whether the active gossip entrypoint is pinned.
    entrypoint_pinned: Option<bool>,
    /// Optional kernel-bypass receive configuration.
    kernel_bypass: Option<KernelBypassConfig>,
}

/// JSON wire representation of direct AF_XDP ingest settings.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KernelBypassConfig {
    /// Target interface name.
    interface: String,
    /// Receive queue id.
    queue_id: u32,
    /// Packet batch size.
    batch_size: usize,
    /// Number of UMEM frames to allocate.
    umem_frame_count: u32,
    /// RX/fill/completion ring depth.
    ring_depth: u32,
    /// Poll timeout in milliseconds.
    poll_timeout_ms: u64,
}

/// JSON wire representation of one stdio-backed plugin worker.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginWorkerConfig {
    /// Stable plugin worker name.
    name: String,
    /// Worker manifest used to register the extension or plugin bridge.
    manifest: RuntimeExtensionWorkerManifestConfig,
    /// Executable command used to spawn the worker.
    command: String,
    /// Command-line arguments passed to the worker.
    args: Vec<String>,
    /// Environment variables passed to the worker.
    environment: HashMap<String, String>,
    #[cfg(feature = "provider-grpc")]
    /// Whether the worker expects provider-event callbacks.
    provider_events: bool,
}

/// Worker manifest envelope received from the TypeScript SDK.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeExtensionWorkerManifestConfig {
    /// Extension name used by the runtime extension host.
    extension_name: String,
    /// Runtime extension manifest payload.
    manifest: ExtensionManifestConfig,
}

/// Runtime extension manifest payload serialized from TypeScript.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionManifestConfig {
    /// Extension capability discriminators.
    capabilities: Vec<u8>,
    /// Resource declarations owned by the extension.
    resources: Vec<ExtensionResourceConfig>,
    /// Packet subscriptions requested by the extension.
    subscriptions: Vec<PacketSubscriptionConfig>,
}

/// Extension resource declaration serialized from TypeScript.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionResourceConfig {
    /// Resource kind discriminator.
    kind: u8,
    /// Stable resource id.
    resource_id: String,
    /// Local bind address for listener resources.
    bind_address: Option<String>,
    /// Remote address for outbound resources.
    remote_address: Option<String>,
    /// URL for websocket resources.
    url: Option<String>,
    /// Stream visibility policy.
    visibility: ExtensionStreamVisibilityConfig,
    /// Read buffer size in bytes.
    read_buffer_bytes: usize,
}

/// Visibility policy for extension-owned streams.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionStreamVisibilityConfig {
    /// Visibility discriminator.
    tag: u8,
    /// Optional shared visibility tag.
    shared_tag: Option<String>,
}

/// Packet subscription filter serialized from TypeScript.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PacketSubscriptionConfig {
    /// Packet source kind selector.
    source_kind: Option<u8>,
    /// Transport selector.
    transport: Option<u8>,
    /// Packet event-class selector.
    event_class: Option<u8>,
    /// Local address filter.
    local_address: Option<String>,
    /// Local port filter.
    local_port: Option<u16>,
    /// Remote address filter.
    remote_address: Option<String>,
    /// Remote port filter.
    remote_port: Option<u16>,
    /// Owning extension filter.
    owner_extension: Option<String>,
    /// Resource id filter.
    resource_id: Option<String>,
    /// Shared stream tag filter.
    shared_tag: Option<String>,
    /// Websocket frame-type selector.
    web_socket_frame_type: Option<u8>,
}

/// Runtime extension adapter that forwards packet events into one TS worker.
struct TypeScriptRuntimeExtension {
    /// Shared worker bridge used for lifecycle and packet delivery.
    bridge: Arc<TypeScriptWorkerBridge>,
}

#[cfg(feature = "provider-grpc")]
/// Observer plugin adapter that forwards provider events into one TS worker.
struct TypeScriptObserverPlugin {
    /// Shared worker bridge used for lifecycle and provider-event delivery.
    bridge: Arc<TypeScriptWorkerBridge>,
    /// Hook contract derived from the active provider ingress modes.
    config: PluginConfig,
}

/// Shared worker bridge for one TypeScript plugin process.
struct TypeScriptWorkerBridge {
    /// Extension/plugin name registered with the runtime.
    name: &'static str,
    /// Worker launch configuration.
    config: PluginWorkerConfig,
    /// Live worker process handle.
    process: Mutex<Option<TypeScriptWorkerProcess>>,
}

/// Spawned stdio worker process state.
struct TypeScriptWorkerProcess {
    /// Child process handle.
    child: Child,
    /// Worker stdin used for protocol messages.
    stdin: ChildStdin,
    /// Newline-delimited worker stdout reader.
    stdout: Lines<BufReader<ChildStdout>>,
}

/// Lifecycle context passed into the TypeScript worker protocol.
#[derive(Debug, Serialize)]
struct WorkerContext<'name> {
    #[serde(rename = "extensionName")]
    /// Extension name associated with the lifecycle callback.
    extension_name: &'name str,
}

/// Starts the native TypeScript runtime host.
#[tokio::main]
async fn main() -> Result<(), HostError> {
    let config_path = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or(HostError::MissingConfigPath)?;
    let config_text = fs::read_to_string(&config_path).map_err(|source| HostError::ReadConfig {
        path: config_path.clone(),
        source,
    })?;
    let config: RuntimeHostConfig =
        serde_json::from_str(&config_text).map_err(|source| HostError::ParseConfig {
            path: config_path,
            source,
        })?;

    run_config(config).await
}

/// Routes the parsed host config into one unified runtime host composition.
async fn run_config(config: RuntimeHostConfig) -> Result<(), HostError> {
    validate_ingress(&config)?;

    let setup = runtime_setup(&config);
    let kernel_bypass = direct_shreds_ingress(&config)
        .and_then(|ingress| ingress.kernel_bypass.as_ref())
        .cloned();
    let (extension_host, worker_bridges) = build_worker_bridges(&config.plugin_workers)?;
    #[cfg(not(feature = "provider-grpc"))]
    drop(worker_bridges);

    let runtime = ObserverRuntime::new()
        .with_setup(setup)
        .with_extension_host(extension_host);

    #[cfg(feature = "provider-grpc")]
    let mut runtime = runtime;

    #[cfg(feature = "provider-grpc")]
    let mut provider_source_handles = Vec::new();

    #[cfg(feature = "provider-grpc")]
    if config
        .ingress
        .iter()
        .any(|ingress| ingress.kind == INGRESS_KIND_GRPC || ingress.kind == INGRESS_KIND_WEB_SOCKET)
    {
        let (provider_stream_tx, provider_stream_rx) =
            create_provider_stream_queue(PROVIDER_STREAM_QUEUE_CAPACITY);
        let mut modes = Vec::new();
        for ingress in config
            .ingress
            .iter()
            .filter(|ingress| ingress.kind == INGRESS_KIND_GRPC)
        {
            let source = spawn_yellowstone_ingress(
                ingress,
                config.fan_in.as_ref(),
                provider_stream_tx.clone(),
            )
            .await?;
            modes.push(source.mode);
            provider_source_handles.push(source);
        }
        #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
        for ingress in config
            .ingress
            .iter()
            .filter(|ingress| ingress.kind == INGRESS_KIND_WEB_SOCKET)
        {
            let source = spawn_websocket_provider_ingress(
                ingress,
                config.fan_in.as_ref(),
                provider_stream_tx.clone(),
            )
            .await?;
            modes.push(source.mode);
            provider_source_handles.push(source);
        }
        #[cfg(not(feature = "provider-websocket"))]
        if config
            .ingress
            .iter()
            .any(|ingress| ingress.kind == INGRESS_KIND_WEB_SOCKET)
        {
            let ingress = config
                .ingress
                .iter()
                .find(|ingress| ingress.kind == INGRESS_KIND_WEB_SOCKET)
                .map(|ingress| ingress.name.as_str())
                .unwrap_or("unknown");
            return Err(HostError::InvalidConfig(format!(
                "websocket ingress `{ingress}` requires a runtime host built with the provider-websocket feature"
            )));
        }
        drop(provider_stream_tx);

        runtime = runtime
            .with_plugin_host(plugin_host_from_worker_bridges(&worker_bridges, &modes))
            .with_provider_stream_ingress(
                provider_stream_mode(modes.as_slice()),
                provider_stream_rx,
            );
        if has_raw_ingress(&config) {
            runtime = runtime.with_raw_ingress_alongside_provider_stream();
        }
    }

    #[cfg(not(feature = "provider-grpc"))]
    if config
        .ingress
        .iter()
        .any(|ingress| ingress.kind == INGRESS_KIND_GRPC || ingress.kind == INGRESS_KIND_WEB_SOCKET)
    {
        let ingress = config
            .ingress
            .iter()
            .find(|ingress| {
                ingress.kind == INGRESS_KIND_GRPC || ingress.kind == INGRESS_KIND_WEB_SOCKET
            })
            .map(|ingress| (ingress.name.as_str(), ingress.kind))
            .unwrap_or(("unknown", INGRESS_KIND_GRPC));
        if ingress.1 == INGRESS_KIND_WEB_SOCKET {
            return Err(HostError::InvalidConfig(format!(
                "websocket ingress `{}` requires a runtime host built with the provider-grpc and provider-websocket features",
                ingress.0
            )));
        }
        return Err(HostError::InvalidConfig(format!(
            "gRPC ingress `{}` requires a runtime host built with the provider-grpc feature",
            ingress.0
        )));
    }

    let run_result = run_runtime_with_optional_kernel_bypass(runtime, kernel_bypass.as_ref()).await;

    #[cfg(feature = "provider-grpc")]
    for handle in provider_source_handles {
        handle.abort();
    }

    run_result
}

/// Builds the extension host plus one shared worker bridge per TypeScript plugin.
fn build_worker_bridges(
    workers: &[PluginWorkerConfig],
) -> Result<(RuntimeExtensionHost, Vec<Arc<TypeScriptWorkerBridge>>), HostError> {
    let mut extension_host_builder = RuntimeExtensionHost::builder();
    let mut bridges = Vec::with_capacity(workers.len());

    for worker in workers.iter().cloned() {
        let extension_name = leak_extension_name(worker.manifest.extension_name.clone())?;
        let bridge = Arc::new(TypeScriptWorkerBridge {
            name: extension_name,
            config: worker,
            process: Mutex::new(None),
        });
        extension_host_builder = extension_host_builder.add_extension(TypeScriptRuntimeExtension {
            bridge: Arc::clone(&bridge),
        });
        bridges.push(bridge);
    }

    Ok((extension_host_builder.build(), bridges))
}

#[cfg(feature = "provider-grpc")]
/// Builds a plugin host that reuses the shared TypeScript worker bridges.
fn plugin_host_from_worker_bridges(
    bridges: &[Arc<TypeScriptWorkerBridge>],
    modes: &[ProviderStreamMode],
) -> PluginHost {
    let mut plugin_host_builder = PluginHost::builder();
    for bridge in bridges {
        if !bridge.config.provider_events {
            continue;
        }

        plugin_host_builder = plugin_host_builder.add_plugin(TypeScriptObserverPlugin {
            bridge: Arc::clone(bridge),
            config: provider_plugin_config(modes),
        });
    }

    plugin_host_builder.build()
}

#[cfg(all(target_os = "linux", feature = "kernel-bypass"))]
/// Runs one observer runtime and optionally attaches AF_XDP ingest until termination.
async fn run_runtime_with_optional_kernel_bypass(
    runtime: ObserverRuntime,
    kernel_bypass: Option<&KernelBypassConfig>,
) -> Result<(), HostError> {
    let Some(kernel_bypass) = kernel_bypass else {
        runtime.run_until_termination_signal().await?;
        return Ok(());
    };

    let producer_config = af_xdp::AfXdpConfig {
        interface: kernel_bypass.interface.clone(),
        queue_id: kernel_bypass.queue_id,
        batch_size: kernel_bypass.batch_size,
        umem_frame_count: kernel_bypass.umem_frame_count,
        ring_depth: kernel_bypass.ring_depth,
        poll_timeout: Duration::from_millis(kernel_bypass.poll_timeout_ms),
        filter: af_xdp::PortFilter::default_sol(),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let producer_stop = Arc::clone(&stop);
    let (tx, rx) = sof::runtime::create_kernel_bypass_ingress_queue();
    let producer_task = spawn_blocking(move || {
        af_xdp::run_af_xdp_producer_until(&tx, &producer_config, &producer_stop)
    });
    let runtime_result = runtime
        .with_kernel_bypass_ingress(rx)
        .run_until_termination_signal()
        .await;

    stop.store(true, Ordering::Relaxed);
    let producer_result: Result<(), af_xdp::AfXdpError> = producer_task.await.map_err(|error| {
        HostError::KernelBypass(format!("AF_XDP producer task join failed: {error}"))
    })?;
    producer_result.map_err(|error| HostError::KernelBypass(error.to_string()))?;
    runtime_result?;
    Ok(())
}

#[cfg(not(all(target_os = "linux", feature = "kernel-bypass")))]
/// Runs one observer runtime when kernel bypass is unavailable.
async fn run_runtime_with_optional_kernel_bypass(
    runtime: ObserverRuntime,
    kernel_bypass: Option<&KernelBypassConfig>,
) -> Result<(), HostError> {
    if kernel_bypass.is_some() {
        return Err(HostError::InvalidConfig(
            "kernel bypass requires a Linux runtime host built with the kernel-bypass feature"
                .to_owned(),
        ));
    }

    runtime.run_until_termination_signal().await?;
    Ok(())
}

/// Validates ingress combinations accepted by the native host.
fn validate_ingress(config: &RuntimeHostConfig) -> Result<(), HostError> {
    let mut direct_shreds_count = 0_usize;
    #[cfg(feature = "gossip-bootstrap")]
    let mut gossip_count = 0_usize;
    #[cfg(not(feature = "gossip-bootstrap"))]
    let gossip_count = 0_usize;
    for ingress in &config.ingress {
        match ingress.kind {
            INGRESS_KIND_WEB_SOCKET => match ingress.url.as_deref().map(str::trim) {
                Some(url) if !url.is_empty() => {}
                _ => {
                    return Err(HostError::InvalidConfig(format!(
                        "websocket ingress `{}` is missing a valid url",
                        ingress.name
                    )));
                }
            },
            INGRESS_KIND_GRPC => {}
            INGRESS_KIND_GOSSIP => {
                #[cfg(not(feature = "gossip-bootstrap"))]
                {
                    return Err(HostError::InvalidConfig(format!(
                        "gossip ingress `{}` requires a runtime host built with the gossip-bootstrap feature",
                        ingress.name
                    )));
                }
                #[cfg(feature = "gossip-bootstrap")]
                {
                    gossip_count = gossip_count.checked_add(1).ok_or_else(|| {
                        HostError::InvalidConfig(
                            "gossip ingress count overflowed during validation".to_owned(),
                        )
                    })?;
                    if ingress.kernel_bypass.is_some() {
                        return Err(HostError::InvalidConfig(format!(
                            "gossip ingress `{}` cannot declare kernel bypass directly; attach kernel-bypass config to direct shred ingress instead",
                            ingress.name
                        )));
                    }
                    if let Some(runtime_mode) = ingress.runtime_mode
                        && gossip_runtime_mode_from_wire(runtime_mode).is_none()
                    {
                        return Err(HostError::InvalidConfig(format!(
                            "unsupported gossip runtime mode {runtime_mode}"
                        )));
                    }
                }
            }
            INGRESS_KIND_DIRECT_SHREDS => {
                direct_shreds_count = direct_shreds_count.checked_add(1).ok_or_else(|| {
                    HostError::InvalidConfig(
                        "direct shred ingress count overflowed during validation".to_owned(),
                    )
                })?;
                if let Some(kernel_bypass) = ingress.kernel_bypass.as_ref() {
                    validate_kernel_bypass_config(kernel_bypass, &ingress.name)?;
                    #[cfg(not(all(target_os = "linux", feature = "kernel-bypass")))]
                    return Err(HostError::InvalidConfig(format!(
                        "ingress `{}` requests kernel bypass; this runtime host must be built on Linux with the kernel-bypass feature",
                        ingress.name
                    )));
                }
            }
            other => {
                return Err(HostError::InvalidConfig(format!(
                    "ingress `{}` has unsupported kind {other}",
                    ingress.name
                )));
            }
        }
    }

    if direct_shreds_count > 1 {
        return Err(HostError::InvalidConfig(
            "TypeScript runtime host currently supports one direct shred ingress source per app run"
                .to_owned(),
        ));
    }
    if gossip_count > 1 {
        return Err(HostError::InvalidConfig(
            "TypeScript runtime host currently supports one gossip ingress source per app run"
                .to_owned(),
        ));
    }
    if let (Some(bind_address), Some(gossip_bind_address)) = (
        direct_shreds_ingress(config).and_then(|ingress| ingress.bind_address.as_deref()),
        gossip_ingress(config).and_then(|ingress| ingress.bind_address.as_deref()),
    ) && bind_address != gossip_bind_address
    {
        return Err(HostError::InvalidConfig(format!(
            "direct shred and gossip ingress bind addresses must match when both are configured ({bind_address} != {gossip_bind_address})"
        )));
    }

    Ok(())
}

/// Validates one kernel-bypass config block after JSON deserialization.
fn validate_kernel_bypass_config(
    config: &KernelBypassConfig,
    ingress_name: &str,
) -> Result<(), HostError> {
    if config.interface.trim().is_empty() {
        return Err(HostError::InvalidConfig(format!(
            "ingress `{ingress_name}` requests kernel bypass with an empty network interface"
        )));
    }
    if config.batch_size == 0 {
        return Err(HostError::InvalidConfig(format!(
            "ingress `{ingress_name}` requests kernel bypass with batchSize=0"
        )));
    }
    if config.umem_frame_count == 0 {
        return Err(HostError::InvalidConfig(format!(
            "ingress `{ingress_name}` requests kernel bypass with umemFrameCount=0"
        )));
    }
    if config.ring_depth == 0 {
        return Err(HostError::InvalidConfig(format!(
            "ingress `{ingress_name}` requests kernel bypass with ringDepth=0"
        )));
    }
    if config.poll_timeout_ms == 0 {
        return Err(HostError::InvalidConfig(format!(
            "ingress `{ingress_name}` requests kernel bypass with pollTimeoutMs=0"
        )));
    }
    let _queue_id = config.queue_id;
    Ok(())
}

/// Builds the runtime setup derived from the TypeScript config.
fn runtime_setup(config: &RuntimeHostConfig) -> RuntimeSetup {
    let mut setup = RuntimeSetup::new();
    for (key, value) in &config.runtime_environment {
        setup = setup.with_env(key, value);
    }
    if let Some(bind_address) = effective_bind_address(config) {
        setup = setup.with_env("SOF_BIND", bind_address);
    }
    if let Some(ingress) = gossip_ingress(config)
        && let Some(entrypoints) = ingress.entrypoints.as_ref()
    {
        setup = setup.with_gossip_entrypoints(entrypoints.iter().cloned());
        if let Some(pinned) = ingress.entrypoint_pinned {
            setup = setup.with_gossip_entrypoint_pinned(pinned);
        }
        #[cfg(feature = "gossip-bootstrap")]
        if let Some(runtime_mode) = ingress.runtime_mode.and_then(gossip_runtime_mode_from_wire) {
            setup = setup.with_gossip_runtime_mode(runtime_mode);
        }
    }
    setup.with_env("SOF_TS_APP_NAME", &config.app_name)
}

#[cfg(feature = "gossip-bootstrap")]
/// Maps the wire gossip runtime mode into the Rust runtime enum.
const fn gossip_runtime_mode_from_wire(value: u8) -> Option<GossipRuntimeMode> {
    match value {
        1 => Some(GossipRuntimeMode::Full),
        2 => Some(GossipRuntimeMode::BootstrapOnly),
        3 => Some(GossipRuntimeMode::ControlPlaneOnly),
        _ => None,
    }
}

/// Returns the direct-shreds ingress when present.
fn direct_shreds_ingress(config: &RuntimeHostConfig) -> Option<&IngressConfig> {
    config
        .ingress
        .iter()
        .find(|ingress| ingress.kind == INGRESS_KIND_DIRECT_SHREDS)
}

/// Returns the gossip ingress when present.
fn gossip_ingress(config: &RuntimeHostConfig) -> Option<&IngressConfig> {
    config
        .ingress
        .iter()
        .find(|ingress| ingress.kind == INGRESS_KIND_GOSSIP)
}

#[cfg(feature = "provider-grpc")]
/// Returns whether the config declares any raw packet ingress.
fn has_raw_ingress(config: &RuntimeHostConfig) -> bool {
    config.ingress.iter().any(|ingress| {
        ingress.kind == INGRESS_KIND_DIRECT_SHREDS || ingress.kind == INGRESS_KIND_GOSSIP
    })
}

/// Returns the bind address that should drive raw runtime setup.
fn effective_bind_address(config: &RuntimeHostConfig) -> Option<&str> {
    direct_shreds_ingress(config)
        .and_then(|ingress| ingress.bind_address.as_deref())
        .or_else(|| gossip_ingress(config).and_then(|ingress| ingress.bind_address.as_deref()))
}

#[cfg(feature = "provider-grpc")]
/// Yellowstone source task handle plus the runtime mode it drives.
struct ProviderSourceHandle {
    /// Runtime mode inferred from the configured source.
    mode: ProviderStreamMode,
    /// Abort handle for the underlying provider source task.
    abort_handle: tokio::task::AbortHandle,
    /// Join guard that logs provider source task failures.
    join_guard: JoinHandle<()>,
}

#[cfg(feature = "provider-grpc")]
impl ProviderSourceHandle {
    /// Stops the provider source task and its join guard.
    fn abort(self) {
        self.abort_handle.abort();
        self.join_guard.abort();
    }
}

#[cfg(feature = "provider-grpc")]
/// Wraps one provider source task so failures are not silently detached.
fn spawn_provider_source_join_guard<E>(
    source_name: &str,
    handle: JoinHandle<Result<(), E>>,
) -> (tokio::task::AbortHandle, JoinHandle<()>)
where
    E: std::fmt::Display + Send + 'static,
{
    let abort_handle = handle.abort_handle();
    let source_name = source_name.to_owned();
    let join_guard = tokio::spawn(async move {
        match handle.await {
            Ok(Ok(())) => {
                tracing::warn!(source = source_name, "provider source task ended");
            }
            Ok(Err(error)) => {
                tracing::warn!(source = source_name, error = %error, "provider source task failed");
            }
            Err(error) => {
                if !error.is_cancelled() {
                    tracing::warn!(source = source_name, error = %error, "provider source task join failed");
                }
            }
        }
    });
    (abort_handle, join_guard)
}

#[cfg(feature = "provider-grpc")]
/// Spawns one Yellowstone ingress source from the wire config.
async fn spawn_yellowstone_ingress(
    ingress: &IngressConfig,
    fan_in: Option<&FanInConfig>,
    sender: ProviderStreamSender,
) -> Result<ProviderSourceHandle, HostError> {
    let endpoint = ingress.endpoint.as_deref().ok_or_else(|| {
        HostError::InvalidConfig(format!(
            "gRPC ingress `{}` is missing endpoint",
            ingress.name
        ))
    })?;
    let stream = ingress.stream.unwrap_or(GRPC_STREAM_TRANSACTIONS);

    if stream == GRPC_STREAM_SLOTS {
        let mut config =
            YellowstoneGrpcSlotsConfig::new(endpoint).with_source_instance(ingress.name.clone());
        config = apply_yellowstone_slot_source_policy(config, ingress)?;
        config = apply_yellowstone_slot_fan_in(config, fan_in)?;
        if let Some(x_token) = non_empty_optional(ingress.x_token.as_deref()) {
            config = config.with_x_token(x_token.to_owned());
        }
        if let Some(commitment) = ingress.commitment {
            config = config.with_commitment(yellowstone_commitment_from_wire(commitment)?);
        }
        let mode = config.runtime_mode();
        let handle = spawn_yellowstone_grpc_slot_source(config, sender)
            .await
            .map_err(|error| HostError::InvalidConfig(error.to_string()))?;
        let (abort_handle, join_guard) = spawn_provider_source_join_guard(&ingress.name, handle);
        return Ok(ProviderSourceHandle {
            mode,
            abort_handle,
            join_guard,
        });
    }

    let mut config = YellowstoneGrpcConfig::new(endpoint)
        .with_source_instance(ingress.name.clone())
        .with_stream(yellowstone_stream_from_wire(stream)?);
    config = apply_yellowstone_source_policy(config, ingress)?;
    config = apply_yellowstone_fan_in(config, fan_in)?;
    if let Some(x_token) = non_empty_optional(ingress.x_token.as_deref()) {
        config = config.with_x_token(x_token.to_owned());
    }
    if let Some(commitment) = ingress.commitment {
        config = config.with_commitment(yellowstone_commitment_from_wire(commitment)?);
    }
    if let Some(vote) = ingress.vote {
        config = config.with_vote(vote);
    }
    if let Some(failed) = ingress.failed {
        config = config.with_failed(failed);
    }
    if let Some(signature) = non_empty_optional(ingress.signature.as_deref()) {
        config = config.with_signature(parse_signature(signature, "ingress.signature")?);
    }
    config = config
        .with_account_include(parse_pubkeys(
            ingress.account_include.as_deref().unwrap_or(&[]),
            "ingress.accountInclude",
        )?)
        .with_account_exclude(parse_pubkeys(
            ingress.account_exclude.as_deref().unwrap_or(&[]),
            "ingress.accountExclude",
        )?)
        .with_account_required(parse_pubkeys(
            ingress.account_required.as_deref().unwrap_or(&[]),
            "ingress.accountRequired",
        )?)
        .with_accounts(parse_pubkeys(
            ingress.accounts.as_deref().unwrap_or(&[]),
            "ingress.accounts",
        )?)
        .with_owners(parse_pubkeys(
            ingress.owners.as_deref().unwrap_or(&[]),
            "ingress.owners",
        )?);
    if ingress.require_transaction_signature.unwrap_or(false) {
        config = config.require_transaction_signature();
    }

    let mode = config.runtime_mode();
    let handle = spawn_yellowstone_grpc_source(config, sender)
        .await
        .map_err(|error| HostError::InvalidConfig(error.to_string()))?;
    let (abort_handle, join_guard) = spawn_provider_source_join_guard(&ingress.name, handle);
    Ok(ProviderSourceHandle {
        mode,
        abort_handle,
        join_guard,
    })
}

#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
/// Spawns one websocket provider-stream source from the wire config.
async fn spawn_websocket_provider_ingress(
    ingress: &IngressConfig,
    fan_in: Option<&FanInConfig>,
    sender: ProviderStreamSender,
) -> Result<ProviderSourceHandle, HostError> {
    if !ingress.requests.as_deref().unwrap_or(&[]).is_empty() {
        return Err(HostError::InvalidConfig(format!(
            "websocket ingress `{}` must use the native SOF provider-stream websocket adapter; custom JSON-RPC requests are not accepted by the runtime host",
            ingress.name
        )));
    }
    let endpoint = ingress.url.as_deref().ok_or_else(|| {
        HostError::InvalidConfig(format!(
            "websocket ingress `{}` is missing url",
            ingress.name
        ))
    })?;
    match ingress.stream.unwrap_or(WEBSOCKET_STREAM_TRANSACTIONS) {
        WEBSOCKET_STREAM_TRANSACTIONS => {
            let mut config = WebsocketTransactionConfig::new(endpoint)
                .with_source_instance(ingress.name.clone());
            config = apply_websocket_source_policy(config, ingress)?;
            config = apply_websocket_fan_in(config, fan_in)?;
            if let Some(commitment) = ingress.commitment {
                config = config.with_commitment(websocket_commitment_from_wire(commitment)?);
            }
            if let Some(vote) = ingress.vote {
                config = config.with_vote(vote);
            }
            if let Some(failed) = ingress.failed {
                config = config.with_failed(failed);
            }
            if let Some(signature) = non_empty_optional(ingress.signature.as_deref()) {
                config = config.with_signature(parse_signature(signature, "ingress.signature")?);
            }
            config = config
                .with_account_include(parse_pubkeys(
                    ingress.account_include.as_deref().unwrap_or(&[]),
                    "ingress.accountInclude",
                )?)
                .with_account_exclude(parse_pubkeys(
                    ingress.account_exclude.as_deref().unwrap_or(&[]),
                    "ingress.accountExclude",
                )?)
                .with_account_required(parse_pubkeys(
                    ingress.account_required.as_deref().unwrap_or(&[]),
                    "ingress.accountRequired",
                )?);

            let mode = config.runtime_mode();
            let handle = spawn_websocket_source(&config, sender)
                .await
                .map_err(|error| HostError::InvalidConfig(error.to_string()))?;
            let (abort_handle, join_guard) =
                spawn_provider_source_join_guard(&ingress.name, handle);
            Ok(ProviderSourceHandle {
                mode,
                abort_handle,
                join_guard,
            })
        }
        WEBSOCKET_STREAM_LOGS => {
            let mut config =
                WebsocketLogsConfig::new(endpoint).with_source_instance(ingress.name.clone());
            config = apply_websocket_logs_source_policy(config, ingress)?;
            config = apply_websocket_logs_fan_in(config, fan_in)?;
            if let Some(commitment) = ingress.commitment {
                config = config.with_commitment(websocket_commitment_from_wire(commitment)?);
            }
            config = config.with_filter(websocket_logs_filter_from_wire(ingress)?);

            let mode = config.runtime_mode();
            let handle = spawn_websocket_logs_source(&config, sender)
                .await
                .map_err(|error| HostError::InvalidConfig(error.to_string()))?;
            let (abort_handle, join_guard) =
                spawn_provider_source_join_guard(&ingress.name, handle);
            Ok(ProviderSourceHandle {
                mode,
                abort_handle,
                join_guard,
            })
        }
        WEBSOCKET_STREAM_ACCOUNT => {
            let account = parse_pubkey(
                ingress.account.as_deref(),
                "ingress.account",
                "websocket account stream",
            )?;
            let mut config = WebsocketTransactionConfig::new(endpoint)
                .with_source_instance(ingress.name.clone())
                .with_stream(WebsocketPrimaryStream::Account(account));
            config = apply_websocket_source_policy(config, ingress)?;
            config = apply_websocket_fan_in(config, fan_in)?;
            if let Some(commitment) = ingress.commitment {
                config = config.with_commitment(websocket_commitment_from_wire(commitment)?);
            }

            let mode = config.runtime_mode();
            let handle = spawn_websocket_source(&config, sender)
                .await
                .map_err(|error| HostError::InvalidConfig(error.to_string()))?;
            let (abort_handle, join_guard) =
                spawn_provider_source_join_guard(&ingress.name, handle);
            Ok(ProviderSourceHandle {
                mode,
                abort_handle,
                join_guard,
            })
        }
        WEBSOCKET_STREAM_PROGRAM => {
            let program_id = parse_pubkey(
                ingress.program_id.as_deref(),
                "ingress.programId",
                "websocket program stream",
            )?;
            let mut config = WebsocketTransactionConfig::new(endpoint)
                .with_source_instance(ingress.name.clone())
                .with_stream(WebsocketPrimaryStream::Program(program_id));
            config = apply_websocket_source_policy(config, ingress)?;
            config = apply_websocket_fan_in(config, fan_in)?;
            if let Some(commitment) = ingress.commitment {
                config = config.with_commitment(websocket_commitment_from_wire(commitment)?);
            }

            let mode = config.runtime_mode();
            let handle = spawn_websocket_source(&config, sender)
                .await
                .map_err(|error| HostError::InvalidConfig(error.to_string()))?;
            let (abort_handle, join_guard) =
                spawn_provider_source_join_guard(&ingress.name, handle);
            Ok(ProviderSourceHandle {
                mode,
                abort_handle,
                join_guard,
            })
        }
        other => Err(HostError::InvalidConfig(format!(
            "unsupported websocket stream kind {other}"
        ))),
    }
}

#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
/// Applies per-source readiness, role, and priority to one websocket config.
fn apply_websocket_source_policy(
    mut config: WebsocketTransactionConfig,
    ingress: &IngressConfig,
) -> Result<WebsocketTransactionConfig, HostError> {
    if let Some(readiness) = ingress.readiness {
        config = config.with_readiness(provider_readiness_from_wire(readiness)?);
    }
    if let Some(role) = ingress.role {
        config = config.with_source_role(provider_role_from_wire(role)?);
    }
    if let Some(priority) = ingress.priority {
        config = config.with_source_priority(priority);
    }
    Ok(config)
}

#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
/// Applies fan-in arbitration to one websocket config.
fn apply_websocket_fan_in(
    config: WebsocketTransactionConfig,
    fan_in: Option<&FanInConfig>,
) -> Result<WebsocketTransactionConfig, HostError> {
    Ok(config.with_source_arbitration(provider_source_arbitration(fan_in)?))
}

#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
/// Applies per-source readiness, role, and priority to one websocket logs config.
fn apply_websocket_logs_source_policy(
    mut config: WebsocketLogsConfig,
    ingress: &IngressConfig,
) -> Result<WebsocketLogsConfig, HostError> {
    if let Some(readiness) = ingress.readiness {
        config = config.with_readiness(provider_readiness_from_wire(readiness)?);
    }
    if let Some(role) = ingress.role {
        config = config.with_source_role(provider_role_from_wire(role)?);
    }
    if let Some(priority) = ingress.priority {
        config = config.with_source_priority(priority);
    }
    Ok(config)
}

#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
/// Applies fan-in arbitration to one websocket logs config.
fn apply_websocket_logs_fan_in(
    config: WebsocketLogsConfig,
    fan_in: Option<&FanInConfig>,
) -> Result<WebsocketLogsConfig, HostError> {
    Ok(config.with_source_arbitration(provider_source_arbitration(fan_in)?))
}

#[cfg(feature = "provider-grpc")]
/// Applies per-source readiness, role, and priority to one Yellowstone config.
fn apply_yellowstone_source_policy(
    mut config: YellowstoneGrpcConfig,
    ingress: &IngressConfig,
) -> Result<YellowstoneGrpcConfig, HostError> {
    if let Some(readiness) = ingress.readiness {
        config = config.with_readiness(provider_readiness_from_wire(readiness)?);
    }
    if let Some(role) = ingress.role {
        config = config.with_source_role(provider_role_from_wire(role)?);
    }
    if let Some(priority) = ingress.priority {
        config = config.with_source_priority(priority);
    }
    Ok(config)
}

#[cfg(feature = "provider-grpc")]
/// Applies per-source readiness, role, and priority to one Yellowstone slots config.
fn apply_yellowstone_slot_source_policy(
    mut config: YellowstoneGrpcSlotsConfig,
    ingress: &IngressConfig,
) -> Result<YellowstoneGrpcSlotsConfig, HostError> {
    if let Some(readiness) = ingress.readiness {
        config = config.with_readiness(provider_readiness_from_wire(readiness)?);
    }
    if let Some(role) = ingress.role {
        config = config.with_source_role(provider_role_from_wire(role)?);
    }
    if let Some(priority) = ingress.priority {
        config = config.with_source_priority(priority);
    }
    Ok(config)
}

#[cfg(feature = "provider-grpc")]
/// Applies fan-in arbitration to one Yellowstone config.
fn apply_yellowstone_fan_in(
    config: YellowstoneGrpcConfig,
    fan_in: Option<&FanInConfig>,
) -> Result<YellowstoneGrpcConfig, HostError> {
    Ok(config.with_source_arbitration(provider_source_arbitration(fan_in)?))
}

#[cfg(feature = "provider-grpc")]
/// Applies fan-in arbitration to one Yellowstone slots config.
fn apply_yellowstone_slot_fan_in(
    config: YellowstoneGrpcSlotsConfig,
    fan_in: Option<&FanInConfig>,
) -> Result<YellowstoneGrpcSlotsConfig, HostError> {
    Ok(config.with_source_arbitration(provider_source_arbitration(fan_in)?))
}

#[cfg(feature = "provider-grpc")]
/// Maps the wire fan-in strategy into the Rust arbitration enum.
fn provider_source_arbitration(
    fan_in: Option<&FanInConfig>,
) -> Result<ProviderSourceArbitrationMode, HostError> {
    let Some(fan_in) = fan_in else {
        return Ok(ProviderSourceArbitrationMode::FirstSeen);
    };
    match fan_in.strategy {
        1 => Ok(ProviderSourceArbitrationMode::EmitAll),
        2 => Ok(ProviderSourceArbitrationMode::FirstSeen),
        3 => Ok(ProviderSourceArbitrationMode::FirstSeenThenPromote),
        other => Err(HostError::InvalidConfig(format!(
            "unsupported fan-in arbitration mode {other}"
        ))),
    }
}

#[cfg(feature = "provider-grpc")]
/// Maps the wire provider readiness selector into the Rust enum.
fn provider_readiness_from_wire(value: u8) -> Result<ProviderSourceReadiness, HostError> {
    match value {
        1 => Ok(ProviderSourceReadiness::Required),
        2 => Ok(ProviderSourceReadiness::Optional),
        other => Err(HostError::InvalidConfig(format!(
            "unsupported provider ingress readiness {other}"
        ))),
    }
}

#[cfg(feature = "provider-grpc")]
/// Maps the wire provider role selector into the Rust enum.
fn provider_role_from_wire(value: u8) -> Result<ProviderSourceRole, HostError> {
    match value {
        1 => Ok(ProviderSourceRole::Primary),
        2 => Ok(ProviderSourceRole::Secondary),
        3 => Ok(ProviderSourceRole::Fallback),
        4 => Ok(ProviderSourceRole::ConfirmOnly),
        other => Err(HostError::InvalidConfig(format!(
            "unsupported provider ingress role {other}"
        ))),
    }
}

#[cfg(feature = "provider-grpc")]
/// Collapses one or more source runtime modes into the runtime ingress mode.
fn provider_stream_mode(modes: &[ProviderStreamMode]) -> ProviderStreamMode {
    match modes {
        [mode] => *mode,
        _ => ProviderStreamMode::Generic,
    }
}

#[cfg(feature = "provider-grpc")]
/// Derives one observer hook mask from the active provider ingress modes.
fn provider_plugin_config(modes: &[ProviderStreamMode]) -> PluginConfig {
    let mut config = PluginConfig::new();
    for mode in modes {
        match mode {
            ProviderStreamMode::Generic => {
                config = config
                    .with_transaction()
                    .with_transaction_status()
                    .with_account_update()
                    .with_block_meta()
                    .with_slot_status()
                    .with_recent_blockhash();
                config.transaction_log = true;
            }
            ProviderStreamMode::YellowstoneGrpc => {
                config = config.with_transaction().with_recent_blockhash();
            }
            ProviderStreamMode::YellowstoneGrpcTransactionStatus => {
                config = config.with_transaction_status();
            }
            ProviderStreamMode::YellowstoneGrpcAccounts => {
                config = config.with_account_update();
            }
            ProviderStreamMode::YellowstoneGrpcBlockMeta => {
                config = config.with_block_meta();
            }
            ProviderStreamMode::YellowstoneGrpcSlots => {
                config = config.with_slot_status();
            }
            ProviderStreamMode::LaserStream => {
                config = config.with_transaction().with_recent_blockhash();
            }
            ProviderStreamMode::LaserStreamTransactionStatus => {
                config = config.with_transaction_status();
            }
            ProviderStreamMode::LaserStreamAccounts => {
                config = config.with_account_update();
            }
            ProviderStreamMode::LaserStreamBlockMeta => {
                config = config.with_block_meta();
            }
            ProviderStreamMode::LaserStreamSlots => {
                config = config.with_slot_status();
            }
            #[cfg(feature = "provider-websocket")]
            ProviderStreamMode::WebsocketTransaction => {
                config = config.with_transaction().with_recent_blockhash();
            }
            #[cfg(feature = "provider-websocket")]
            ProviderStreamMode::WebsocketLogs => {
                config.transaction_log = true;
            }
            #[cfg(feature = "provider-websocket")]
            ProviderStreamMode::WebsocketAccount | ProviderStreamMode::WebsocketProgram => {
                config = config.with_account_update();
            }
        }
    }

    config
}

#[cfg(feature = "provider-grpc")]
/// Maps the wire Yellowstone stream selector into the Rust enum.
fn yellowstone_stream_from_wire(value: u8) -> Result<YellowstoneGrpcStream, HostError> {
    match value {
        GRPC_STREAM_TRANSACTIONS => Ok(YellowstoneGrpcStream::Transaction),
        GRPC_STREAM_TRANSACTION_STATUS => Ok(YellowstoneGrpcStream::TransactionStatus),
        GRPC_STREAM_ACCOUNTS => Ok(YellowstoneGrpcStream::Accounts),
        GRPC_STREAM_BLOCK_META => Ok(YellowstoneGrpcStream::BlockMeta),
        other => Err(HostError::InvalidConfig(format!(
            "unsupported gRPC stream kind {other}"
        ))),
    }
}

#[cfg(feature = "provider-grpc")]
/// Maps the wire Yellowstone commitment selector into the Rust enum.
fn yellowstone_commitment_from_wire(value: u8) -> Result<YellowstoneGrpcCommitment, HostError> {
    match value {
        1 => Ok(YellowstoneGrpcCommitment::Processed),
        2 => Ok(YellowstoneGrpcCommitment::Confirmed),
        3 => Ok(YellowstoneGrpcCommitment::Finalized),
        other => Err(HostError::InvalidConfig(format!(
            "unsupported gRPC commitment {other}"
        ))),
    }
}

#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
/// Maps the wire websocket commitment selector into the Rust enum.
fn websocket_commitment_from_wire(value: u8) -> Result<WebsocketTransactionCommitment, HostError> {
    match value {
        1 => Ok(WebsocketTransactionCommitment::Processed),
        2 => Ok(WebsocketTransactionCommitment::Confirmed),
        3 => Ok(WebsocketTransactionCommitment::Finalized),
        other => Err(HostError::InvalidConfig(format!(
            "unsupported websocket commitment {other}"
        ))),
    }
}

#[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
/// Maps the wire websocket logs filter selector into the Rust enum.
fn websocket_logs_filter_from_wire(
    ingress: &IngressConfig,
) -> Result<WebsocketLogsFilter, HostError> {
    match ingress.logs_filter.unwrap_or(WEBSOCKET_LOGS_FILTER_ALL) {
        WEBSOCKET_LOGS_FILTER_ALL => Ok(WebsocketLogsFilter::All),
        WEBSOCKET_LOGS_FILTER_ALL_WITH_VOTES => Ok(WebsocketLogsFilter::AllWithVotes),
        WEBSOCKET_LOGS_FILTER_MENTIONS => Ok(WebsocketLogsFilter::Mentions(parse_pubkey(
            ingress.mentions.as_deref(),
            "ingress.mentions",
            "websocket logs mention filter",
        )?)),
        other => Err(HostError::InvalidConfig(format!(
            "unsupported websocket logs filter {other}"
        ))),
    }
}

#[cfg(feature = "provider-grpc")]
/// Trims and filters empty optional string values.
fn non_empty_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(feature = "provider-grpc")]
/// Parses one base58 transaction signature from the wire config.
fn parse_signature(value: &str, field: &str) -> Result<Signature, HostError> {
    Signature::from_str(value).map_err(|error| {
        HostError::InvalidConfig(format!("{field} is not a valid signature: {error}"))
    })
}

#[cfg(feature = "provider-grpc")]
/// Parses one required pubkey from the wire config.
fn parse_pubkey(value: Option<&str>, field: &str, context: &str) -> Result<Pubkey, HostError> {
    let value = non_empty_optional(value)
        .ok_or_else(|| HostError::InvalidConfig(format!("{field} is required for {context}")))?;
    Pubkey::from_str(value).map_err(|error| {
        HostError::InvalidConfig(format!("{field} is not a valid pubkey `{value}`: {error}"))
    })
}

#[cfg(feature = "provider-grpc")]
/// Parses one list of pubkeys from the wire config.
fn parse_pubkeys(values: &[String], field: &str) -> Result<Vec<Pubkey>, HostError> {
    values
        .iter()
        .map(|value| {
            Pubkey::from_str(value).map_err(|error| {
                HostError::InvalidConfig(format!(
                    "{field} contains invalid pubkey `{value}`: {error}"
                ))
            })
        })
        .collect()
}

/// Leaks one extension name for the lifetime expected by the host traits.
fn leak_extension_name(name: String) -> Result<&'static str, HostError> {
    if name.trim().is_empty() {
        return Err(HostError::InvalidConfig(
            "plugin worker declares an empty extension name".to_owned(),
        ));
    }

    Ok(Box::leak(name.into_boxed_str()))
}

#[async_trait]
impl RuntimeExtension for TypeScriptRuntimeExtension {
    fn name(&self) -> &'static str {
        self.bridge.name
    }

    async fn setup(
        &self,
        _ctx: ExtensionContext,
    ) -> Result<ExtensionManifest, ExtensionSetupError> {
        self.bridge
            .ensure_started()
            .await
            .map_err(ExtensionSetupError::new)?;
        extension_manifest_from_config(&self.bridge.config.manifest.manifest)
            .map_err(ExtensionSetupError::new)
    }

    async fn on_packet_received(&self, event: RuntimePacketEvent) {
        self.bridge.deliver_packet(event).await;
    }

    async fn shutdown(&self, _ctx: ExtensionContext) {
        self.bridge.shutdown().await;
    }
}

#[async_trait]
#[cfg(feature = "provider-grpc")]
impl ObserverPlugin for TypeScriptObserverPlugin {
    fn name(&self) -> &'static str {
        self.bridge.name
    }

    fn config(&self) -> PluginConfig {
        PluginConfig {
            transaction_log: self.config.transaction_log,
            raw_packet: self.config.raw_packet,
            shred: self.config.shred,
            dataset: self.config.dataset,
            transaction: self.config.transaction,
            transaction_status: self.config.transaction_status,
            transaction_commitment: self.config.transaction_commitment,
            transaction_dispatch_mode: self.config.transaction_dispatch_mode,
            transaction_batch: self.config.transaction_batch,
            transaction_batch_dispatch_mode: self.config.transaction_batch_dispatch_mode,
            transaction_view_batch: self.config.transaction_view_batch,
            transaction_view_batch_dispatch_mode: self.config.transaction_view_batch_dispatch_mode,
            account_touch: self.config.account_touch,
            account_update: self.config.account_update,
            block_meta: self.config.block_meta,
            slot_status: self.config.slot_status,
            reorg: self.config.reorg,
            recent_blockhash: self.config.recent_blockhash,
            cluster_topology: self.config.cluster_topology,
            leader_schedule: self.config.leader_schedule,
        }
    }

    async fn setup(&self, _ctx: PluginContext) -> Result<(), PluginSetupError> {
        self.bridge
            .ensure_started()
            .await
            .map_err(PluginSetupError::new)
    }

    async fn on_transaction(&self, event: &TransactionEvent) {
        self.bridge
            .deliver_provider_event(provider_transaction_event_wire(event))
            .await;
    }

    async fn on_transaction_log(&self, event: &TransactionLogEvent) {
        self.bridge
            .deliver_provider_event(provider_transaction_log_event_wire(event))
            .await;
    }

    async fn on_transaction_status(&self, event: &TransactionStatusEvent) {
        self.bridge
            .deliver_provider_event(provider_transaction_status_event_wire(event))
            .await;
    }

    async fn on_account_update(&self, event: &AccountUpdateEvent) {
        self.bridge
            .deliver_provider_event(provider_account_update_event_wire(event))
            .await;
    }

    async fn on_block_meta(&self, event: &BlockMetaEvent) {
        self.bridge
            .deliver_provider_event(provider_block_meta_event_wire(event))
            .await;
    }

    async fn on_slot_status(&self, event: SlotStatusEvent) {
        self.bridge
            .deliver_provider_event(provider_slot_status_event_wire(&event))
            .await;
    }

    async fn on_recent_blockhash(&self, event: ObservedRecentBlockhashEvent) {
        self.bridge
            .deliver_provider_event(provider_recent_blockhash_event_wire(&event))
            .await;
    }

    async fn shutdown(&self, _ctx: PluginContext) {
        self.bridge.shutdown().await;
    }
}

impl TypeScriptWorkerBridge {
    /// Starts the shared worker process when it has not been started yet.
    async fn ensure_started(&self) -> Result<(), String> {
        let mut guard = self.process.lock().await;
        if guard.is_some() {
            return Ok(());
        }

        *guard = Some(start_worker_process(self.name, &self.config).await?);
        Ok(())
    }

    /// Delivers one packet event into the bound TypeScript worker.
    async fn deliver_packet(&self, event: RuntimePacketEvent) {
        let mut guard = self.process.lock().await;
        let Some(process) = guard.as_mut() else {
            tracing::warn!(
                extension = self.name,
                "worker process is not available for packet"
            );
            return;
        };

        if let Err(error) = send_worker_message(
            process,
            json!({
                "tag": 3,
                "event": runtime_packet_event_wire(&event),
            }),
        )
        .await
        {
            tracing::warn!(extension = self.name, error = %error, "failed to deliver packet to worker");
        } else {
            match read_worker_response(process, RESPONSE_TAG_EVENT_HANDLED).await {
                Ok(response) => {
                    if let Err(error) = response_result_ok(self.name, &response) {
                        tracing::warn!(extension = self.name, error = %error, "worker rejected packet");
                    }
                }
                Err(error) => {
                    tracing::warn!(extension = self.name, error = %error, "worker did not acknowledge packet");
                }
            }
        }
    }

    /// Delivers one provider event into the bound TypeScript worker.
    #[cfg(feature = "provider-grpc")]
    async fn deliver_provider_event(&self, event: Value) {
        let mut guard = self.process.lock().await;
        let Some(process) = guard.as_mut() else {
            tracing::warn!(
                plugin = self.name,
                "worker process is not available for provider event"
            );
            return;
        };

        if let Err(error) = send_worker_message(
            process,
            json!({
                "tag": 5,
                "event": event,
            }),
        )
        .await
        {
            tracing::warn!(plugin = self.name, error = %error, "failed to deliver provider event to worker");
        } else {
            match read_worker_response(process, RESPONSE_TAG_PROVIDER_EVENT_HANDLED).await {
                Ok(response) => {
                    if let Err(error) = response_result_ok(self.name, &response) {
                        tracing::warn!(plugin = self.name, error = %error, "worker rejected provider event");
                    }
                }
                Err(error) => {
                    tracing::warn!(plugin = self.name, error = %error, "worker did not acknowledge provider event");
                }
            }
        }
    }

    /// Requests shutdown for the worker process when it is still running.
    async fn shutdown(&self) {
        let mut guard = self.process.lock().await;
        let Some(process) = guard.as_mut() else {
            return;
        };

        if let Err(error) = send_worker_message(
            process,
            json!({
                "tag": 4,
                "context": WorkerContext {
                    extension_name: self.name,
                },
            }),
        )
        .await
        {
            tracing::warn!(extension = self.name, error = %error, "failed to request worker shutdown");
        } else if let Err(error) = read_worker_response(process, RESPONSE_TAG_SHUTDOWN_COMPLETE)
            .await
            .and_then(|response| response_result_ok(self.name, &response))
        {
            tracing::warn!(extension = self.name, error = %error, "worker shutdown was not acknowledged");
        }

        if let Err(error) = process.child.wait().await {
            tracing::warn!(extension = self.name, error = %error, "failed to wait for worker process");
        }
        *guard = None;
    }
}

/// Spawns one TypeScript worker and completes its manifest and startup handshake.
async fn start_worker_process(
    extension_name: &str,
    config: &PluginWorkerConfig,
) -> Result<TypeScriptWorkerProcess, String> {
    let mut process = spawn_worker(extension_name, config).await?;
    send_worker_message(&mut process, json!({ "tag": 1 })).await?;
    let manifest_response = read_worker_response(&mut process, RESPONSE_TAG_MANIFEST).await?;
    response_result_ok(extension_name, &manifest_response)?;

    send_worker_message(
        &mut process,
        json!({
            "tag": 2,
            "context": WorkerContext {
                extension_name,
            },
        }),
    )
    .await?;
    let start_response = read_worker_response(&mut process, RESPONSE_TAG_STARTED).await?;
    response_result_ok(extension_name, &start_response)?;

    Ok(process)
}

/// Spawns one stdio worker process from the provided launch config.
async fn spawn_worker(
    extension_name: &str,
    config: &PluginWorkerConfig,
) -> Result<TypeScriptWorkerProcess, String> {
    if config.name != extension_name {
        return Err(format!(
            "worker `{}` does not match extension manifest name `{extension_name}`",
            config.name
        ));
    }

    let mut command = Command::new(&config.command);
    command
        .args(&config.args)
        .envs(&config.environment)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to spawn worker `{extension_name}`: {error}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("worker `{extension_name}` did not expose stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("worker `{extension_name}` did not expose stdout"))?;

    Ok(TypeScriptWorkerProcess {
        child,
        stdin,
        stdout: BufReader::new(stdout).lines(),
    })
}

/// Serializes and writes one protocol message to the worker stdin.
async fn send_worker_message(
    process: &mut TypeScriptWorkerProcess,
    message: Value,
) -> Result<(), String> {
    let mut line = serde_json::to_vec(&message)
        .map_err(|error| format!("failed to encode worker message: {error}"))?;
    line.push(b'\n');
    process
        .stdin
        .write_all(&line)
        .await
        .map_err(|error| format!("failed to write worker message: {error}"))?;
    process
        .stdin
        .flush()
        .await
        .map_err(|error| format!("failed to flush worker message: {error}"))
}

/// Reads one worker response line and validates its protocol tag.
async fn read_worker_response(
    process: &mut TypeScriptWorkerProcess,
    expected_tag: u8,
) -> Result<Value, String> {
    let line = process
        .stdout
        .next_line()
        .await
        .map_err(|error| format!("failed to read worker response: {error}"))?
        .ok_or_else(|| "worker stdout closed before response".to_owned())?;
    let response: Value = serde_json::from_str(&line)
        .map_err(|error| format!("worker response was invalid JSON: {error}; line={line}"))?;
    let received_tag = response
        .get("tag")
        .and_then(Value::as_u64)
        .ok_or_else(|| "worker response did not contain numeric tag".to_owned())?;
    if received_tag != u64::from(expected_tag) {
        return Err(format!(
            "worker response tag {received_tag} did not match expected tag {expected_tag}",
        ));
    }

    Ok(response)
}

/// Checks whether one worker response contains an OK result tag.
fn response_result_ok(extension_name: &str, response: &Value) -> Result<(), String> {
    let result = response
        .get("result")
        .ok_or_else(|| format!("worker `{extension_name}` response omitted result"))?;
    match result.get("tag").and_then(Value::as_u64) {
        Some(value) if value == u64::from(RESULT_TAG_OK) => Ok(()),
        Some(value) if value == u64::from(RESULT_TAG_ERR) => {
            let message = result
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("worker returned an error result");
            Err(format!(
                "worker `{extension_name}` returned error: {message}"
            ))
        }
        Some(other) => Err(format!(
            "worker `{extension_name}` response used unsupported result tag {other}",
        )),
        None => Err(format!(
            "worker `{extension_name}` response did not contain result tag",
        )),
    }
}

/// Converts the TypeScript manifest wire config into a runtime manifest.
fn extension_manifest_from_config(
    config: &ExtensionManifestConfig,
) -> Result<ExtensionManifest, String> {
    Ok(ExtensionManifest {
        capabilities: config
            .capabilities
            .iter()
            .copied()
            .map(extension_capability_from_wire)
            .collect::<Result<Vec<_>, _>>()?,
        resources: config
            .resources
            .iter()
            .map(extension_resource_from_config)
            .collect::<Result<Vec<_>, _>>()?,
        subscriptions: config
            .subscriptions
            .iter()
            .map(packet_subscription_from_config)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

/// Converts one wire resource declaration into a runtime resource spec.
fn extension_resource_from_config(
    config: &ExtensionResourceConfig,
) -> Result<ExtensionResourceSpec, String> {
    match config.kind {
        1 => Ok(ExtensionResourceSpec::UdpListener(UdpListenerSpec {
            resource_id: config.resource_id.clone(),
            bind_addr: parse_required_addr(config.bind_address.as_deref(), "bindAddress")?,
            visibility: stream_visibility_from_config(&config.visibility)?,
            read_buffer_bytes: config.read_buffer_bytes,
        })),
        2 => Ok(ExtensionResourceSpec::TcpListener(TcpListenerSpec {
            resource_id: config.resource_id.clone(),
            bind_addr: parse_required_addr(config.bind_address.as_deref(), "bindAddress")?,
            visibility: stream_visibility_from_config(&config.visibility)?,
            read_buffer_bytes: config.read_buffer_bytes,
        })),
        3 => Ok(ExtensionResourceSpec::TcpConnector(TcpConnectorSpec {
            resource_id: config.resource_id.clone(),
            remote_addr: parse_required_addr(config.remote_address.as_deref(), "remoteAddress")?,
            visibility: stream_visibility_from_config(&config.visibility)?,
            read_buffer_bytes: config.read_buffer_bytes,
        })),
        4 => Ok(ExtensionResourceSpec::WsConnector(WsConnectorSpec {
            resource_id: config.resource_id.clone(),
            url: config
                .url
                .clone()
                .ok_or_else(|| "websocket connector resource missing url".to_owned())?,
            visibility: stream_visibility_from_config(&config.visibility)?,
            read_buffer_bytes: config.read_buffer_bytes,
        })),
        other => Err(format!("unsupported extension resource kind {other}")),
    }
}

/// Converts the wire visibility policy into the runtime visibility enum.
fn stream_visibility_from_config(
    config: &ExtensionStreamVisibilityConfig,
) -> Result<ExtensionStreamVisibility, String> {
    match config.tag {
        1 => Ok(ExtensionStreamVisibility::Private),
        2 => Ok(ExtensionStreamVisibility::Shared {
            tag: config
                .shared_tag
                .clone()
                .ok_or_else(|| "shared stream visibility missing sharedTag".to_owned())?,
        }),
        other => Err(format!("unsupported stream visibility tag {other}")),
    }
}

/// Converts the wire packet subscription into the runtime subscription filter.
fn packet_subscription_from_config(
    config: &PacketSubscriptionConfig,
) -> Result<PacketSubscription, String> {
    Ok(PacketSubscription {
        source_kind: config
            .source_kind
            .map(runtime_packet_source_kind_from_wire)
            .transpose()?,
        transport: config
            .transport
            .map(runtime_packet_transport_from_wire)
            .transpose()?,
        event_class: config
            .event_class
            .map(runtime_packet_event_class_from_wire)
            .transpose()?,
        local_addr: config
            .local_address
            .as_deref()
            .map(parse_addr)
            .transpose()?,
        local_port: config.local_port,
        remote_addr: config
            .remote_address
            .as_deref()
            .map(parse_addr)
            .transpose()?,
        remote_port: config.remote_port,
        owner_extension: config.owner_extension.clone(),
        resource_id: config.resource_id.clone(),
        shared_tag: config.shared_tag.clone(),
        websocket_frame_type: config
            .web_socket_frame_type
            .map(runtime_websocket_frame_type_from_wire)
            .transpose()?,
    })
}

/// Parses one required socket address field from a resource config.
fn parse_required_addr(value: Option<&str>, field: &str) -> Result<SocketAddr, String> {
    value
        .ok_or_else(|| format!("resource missing {field}"))
        .and_then(parse_addr)
}

/// Parses one socket address string used by extension resources.
fn parse_addr(value: &str) -> Result<SocketAddr, String> {
    value
        .parse()
        .map_err(|error| format!("invalid socket address `{value}`: {error}"))
}

/// Maps the wire extension capability into the runtime enum.
fn extension_capability_from_wire(value: u8) -> Result<ExtensionCapability, String> {
    match value {
        1 => Ok(ExtensionCapability::BindUdp),
        2 => Ok(ExtensionCapability::BindTcp),
        3 => Ok(ExtensionCapability::ConnectTcp),
        4 => Ok(ExtensionCapability::ConnectWebSocket),
        5 => Ok(ExtensionCapability::ObserveObserverIngress),
        6 => Ok(ExtensionCapability::ObserveSharedExtensionStream),
        other => Err(format!("unsupported extension capability {other}")),
    }
}

/// Maps the wire packet source-kind selector into the runtime enum.
fn runtime_packet_source_kind_from_wire(value: u8) -> Result<RuntimePacketSourceKind, String> {
    match value {
        1 => Ok(RuntimePacketSourceKind::ObserverIngress),
        2 => Ok(RuntimePacketSourceKind::ExtensionResource),
        other => Err(format!("unsupported runtime packet source kind {other}")),
    }
}

/// Maps the wire packet transport selector into the runtime enum.
fn runtime_packet_transport_from_wire(value: u8) -> Result<RuntimePacketTransport, String> {
    match value {
        1 => Ok(RuntimePacketTransport::Udp),
        2 => Ok(RuntimePacketTransport::Tcp),
        3 => Ok(RuntimePacketTransport::WebSocket),
        other => Err(format!("unsupported runtime packet transport {other}")),
    }
}

/// Maps the wire packet event-class selector into the runtime enum.
fn runtime_packet_event_class_from_wire(value: u8) -> Result<RuntimePacketEventClass, String> {
    match value {
        1 => Ok(RuntimePacketEventClass::Packet),
        2 => Ok(RuntimePacketEventClass::ConnectionClosed),
        other => Err(format!("unsupported runtime packet event class {other}")),
    }
}

/// Maps the wire websocket frame selector into the runtime enum.
fn runtime_websocket_frame_type_from_wire(value: u8) -> Result<RuntimeWebSocketFrameType, String> {
    match value {
        1 => Ok(RuntimeWebSocketFrameType::Text),
        2 => Ok(RuntimeWebSocketFrameType::Binary),
        3 => Ok(RuntimeWebSocketFrameType::Ping),
        4 => Ok(RuntimeWebSocketFrameType::Pong),
        other => Err(format!("unsupported websocket frame type {other}")),
    }
}

/// Serializes one runtime packet event for the worker protocol.
fn runtime_packet_event_wire(event: &RuntimePacketEvent) -> Value {
    json!({
        "source": {
            "kind": runtime_packet_source_kind_to_wire(event.source.kind),
            "transport": runtime_packet_transport_to_wire(event.source.transport),
            "eventClass": runtime_packet_event_class_to_wire(event.source.event_class),
            "ownerExtension": event.source.owner_extension,
            "resourceId": event.source.resource_id,
            "sharedTag": event.source.shared_tag,
            "webSocketFrameType": event.source.websocket_frame_type.map(runtime_websocket_frame_type_to_wire),
            "localAddress": event.source.local_addr.map(|addr| addr.to_string()),
            "remoteAddress": event.source.remote_addr.map(|addr| addr.to_string()),
        },
        "bytes": event.bytes.as_ref(),
        "observedUnixMs": event.observed_unix_ms,
    })
}

#[cfg(feature = "provider-grpc")]
/// Serializes one transaction event for the worker protocol.
fn provider_transaction_event_wire(event: &TransactionEvent) -> Value {
    json!({
        "kind": 1,
        "slot": event.slot,
        "commitmentStatus": commitment_status_to_wire(event.commitment_status),
        "confirmedSlot": event.confirmed_slot,
        "finalizedSlot": event.finalized_slot,
        "signature": event.signature.map(|signature| signature.to_base58()),
        "providerSource": provider_source_wire(event.provider_source.as_ref()),
        "transactionKind": tx_kind_to_wire(event.kind),
        "transactionBase64": bincode::serialize(event.tx.as_ref())
            .ok()
            .map(|bytes| BASE64_STANDARD.encode(bytes)),
    })
}

#[cfg(feature = "provider-grpc")]
/// Serializes one transaction-log event for the worker protocol.
fn provider_transaction_log_event_wire(event: &TransactionLogEvent) -> Value {
    json!({
        "kind": 2,
        "slot": event.slot,
        "commitmentStatus": commitment_status_to_wire(event.commitment_status),
        "signature": event.signature.to_base58(),
        "err": event.err,
        "logs": event.logs.as_ref(),
        "matchedFilter": event.matched_filter.map(|pubkey| pubkey.to_base58()),
        "providerSource": provider_source_wire(event.provider_source.as_ref()),
    })
}

#[cfg(feature = "provider-grpc")]
/// Serializes one transaction-status event for the worker protocol.
fn provider_transaction_status_event_wire(event: &TransactionStatusEvent) -> Value {
    json!({
        "kind": 3,
        "slot": event.slot,
        "commitmentStatus": commitment_status_to_wire(event.commitment_status),
        "confirmedSlot": event.confirmed_slot,
        "finalizedSlot": event.finalized_slot,
        "signature": event.signature.to_base58(),
        "isVote": event.is_vote,
        "index": event.index,
        "err": event.err,
        "providerSource": provider_source_wire(event.provider_source.as_ref()),
    })
}

#[cfg(feature = "provider-grpc")]
/// Serializes one account-update event for the worker protocol.
fn provider_account_update_event_wire(event: &AccountUpdateEvent) -> Value {
    json!({
        "kind": 4,
        "slot": event.slot,
        "commitmentStatus": commitment_status_to_wire(event.commitment_status),
        "confirmedSlot": event.confirmed_slot,
        "finalizedSlot": event.finalized_slot,
        "pubkey": event.pubkey.to_base58(),
        "owner": event.owner.to_base58(),
        "lamports": event.lamports,
        "executable": event.executable,
        "rentEpoch": event.rent_epoch,
        "dataBase64": BASE64_STANDARD.encode(event.data.as_ref()),
        "writeVersion": event.write_version,
        "txnSignature": event.txn_signature.map(|signature| signature.to_base58()),
        "isStartup": event.is_startup,
        "matchedFilter": event.matched_filter.map(|pubkey| pubkey.to_base58()),
        "providerSource": provider_source_wire(event.provider_source.as_ref()),
    })
}

#[cfg(feature = "provider-grpc")]
/// Serializes one block-meta event for the worker protocol.
fn provider_block_meta_event_wire(event: &BlockMetaEvent) -> Value {
    json!({
        "kind": 5,
        "slot": event.slot,
        "commitmentStatus": commitment_status_to_wire(event.commitment_status),
        "confirmedSlot": event.confirmed_slot,
        "finalizedSlot": event.finalized_slot,
        "blockhash": solana_hash::Hash::new_from_array(event.blockhash).to_string(),
        "parentSlot": event.parent_slot,
        "parentBlockhash": solana_hash::Hash::new_from_array(event.parent_blockhash).to_string(),
        "blockTime": event.block_time,
        "blockHeight": event.block_height,
        "executedTransactionCount": event.executed_transaction_count,
        "entriesCount": event.entries_count,
        "providerSource": provider_source_wire(event.provider_source.as_ref()),
    })
}

#[cfg(feature = "provider-grpc")]
/// Serializes one slot-status event for the worker protocol.
fn provider_slot_status_event_wire(event: &SlotStatusEvent) -> Value {
    json!({
        "kind": 6,
        "slot": event.slot,
        "parentSlot": event.parent_slot,
        "previousStatus": event.previous_status.map(fork_slot_status_to_wire),
        "status": fork_slot_status_to_wire(event.status),
        "tipSlot": event.tip_slot,
        "confirmedSlot": event.confirmed_slot,
        "finalizedSlot": event.finalized_slot,
        "providerSource": provider_source_wire(event.provider_source.as_ref()),
    })
}

#[cfg(feature = "provider-grpc")]
/// Serializes one recent-blockhash event for the worker protocol.
fn provider_recent_blockhash_event_wire(event: &ObservedRecentBlockhashEvent) -> Value {
    json!({
        "kind": 7,
        "slot": event.slot,
        "recentBlockhash": solana_hash::Hash::new_from_array(event.recent_blockhash).to_string(),
        "datasetTxCount": event.dataset_tx_count,
        "providerSource": provider_source_wire(event.provider_source.as_ref()),
    })
}

#[cfg(feature = "provider-grpc")]
/// Serializes one provider-source reference for the worker protocol.
fn provider_source_wire(source: Option<&ProviderSourceRef>) -> Option<Value> {
    source.map(|source| {
        json!({
            "kind": source.kind_str(),
            "instance": source.instance_str(),
            "priority": source.priority(),
            "role": source.role().as_str(),
            "arbitration": source.arbitration().as_str(),
        })
    })
}

#[cfg(feature = "provider-grpc")]
/// Maps one commitment status into the worker wire tag.
const fn commitment_status_to_wire(status: TxCommitmentStatus) -> u8 {
    match status {
        TxCommitmentStatus::Processed => 1,
        TxCommitmentStatus::Confirmed => 2,
        TxCommitmentStatus::Finalized => 3,
    }
}

#[cfg(feature = "provider-grpc")]
/// Maps one transaction kind into the worker wire tag.
const fn tx_kind_to_wire(kind: TxKind) -> u8 {
    match kind {
        TxKind::VoteOnly => 1,
        TxKind::Mixed => 2,
        TxKind::NonVote => 3,
    }
}

#[cfg(feature = "provider-grpc")]
/// Maps one fork slot status into the worker wire tag.
const fn fork_slot_status_to_wire(status: ForkSlotStatus) -> u8 {
    match status {
        ForkSlotStatus::Processed => 1,
        ForkSlotStatus::Confirmed => 2,
        ForkSlotStatus::Finalized => 3,
        ForkSlotStatus::Orphaned => 4,
    }
}

/// Maps one runtime packet source kind into the worker wire tag.
const fn runtime_packet_source_kind_to_wire(value: RuntimePacketSourceKind) -> u8 {
    match value {
        RuntimePacketSourceKind::ObserverIngress => 1,
        RuntimePacketSourceKind::ExtensionResource => 2,
    }
}

/// Maps one runtime packet transport into the worker wire tag.
const fn runtime_packet_transport_to_wire(value: RuntimePacketTransport) -> u8 {
    match value {
        RuntimePacketTransport::Udp => 1,
        RuntimePacketTransport::Tcp => 2,
        RuntimePacketTransport::WebSocket => 3,
    }
}

/// Maps one runtime packet event class into the worker wire tag.
const fn runtime_packet_event_class_to_wire(value: RuntimePacketEventClass) -> u8 {
    match value {
        RuntimePacketEventClass::Packet => 1,
        RuntimePacketEventClass::ConnectionClosed => 2,
    }
}

/// Maps one websocket frame type into the worker wire tag.
const fn runtime_websocket_frame_type_to_wire(value: RuntimeWebSocketFrameType) -> u8 {
    match value {
        RuntimeWebSocketFrameType::Text => 1,
        RuntimeWebSocketFrameType::Binary => 2,
        RuntimeWebSocketFrameType::Ping => 3,
        RuntimeWebSocketFrameType::Pong => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn websocket_ingress(name: &str) -> IngressConfig {
        IngressConfig {
            kind: INGRESS_KIND_WEB_SOCKET,
            name: name.to_owned(),
            bind_address: None,
            #[cfg(feature = "provider-grpc")]
            endpoint: None,
            #[cfg(feature = "provider-grpc")]
            stream: None,
            #[cfg(feature = "provider-grpc")]
            x_token: None,
            #[cfg(feature = "provider-grpc")]
            commitment: None,
            #[cfg(feature = "provider-grpc")]
            vote: None,
            #[cfg(feature = "provider-grpc")]
            failed: None,
            #[cfg(feature = "provider-grpc")]
            signature: None,
            #[cfg(feature = "provider-grpc")]
            account_include: None,
            #[cfg(feature = "provider-grpc")]
            account_exclude: None,
            #[cfg(feature = "provider-grpc")]
            account_required: None,
            #[cfg(feature = "provider-grpc")]
            accounts: None,
            #[cfg(feature = "provider-grpc")]
            owners: None,
            #[cfg(feature = "provider-grpc")]
            require_transaction_signature: None,
            #[cfg(feature = "provider-grpc")]
            readiness: None,
            #[cfg(feature = "provider-grpc")]
            role: None,
            #[cfg(feature = "provider-grpc")]
            priority: None,
            url: Some("wss://example.invalid".to_owned()),
            #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
            account: None,
            #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
            program_id: None,
            #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
            logs_filter: None,
            #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
            mentions: None,
            #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
            requests: Some(vec![]),
            entrypoints: None,
            #[cfg(feature = "gossip-bootstrap")]
            runtime_mode: None,
            entrypoint_pinned: None,
            kernel_bypass: None,
        }
    }

    fn direct_shreds_ingress(name: &str) -> IngressConfig {
        IngressConfig {
            kind: INGRESS_KIND_DIRECT_SHREDS,
            name: name.to_owned(),
            bind_address: Some("127.0.0.1:20000".to_owned()),
            #[cfg(feature = "provider-grpc")]
            endpoint: None,
            #[cfg(feature = "provider-grpc")]
            stream: None,
            #[cfg(feature = "provider-grpc")]
            x_token: None,
            #[cfg(feature = "provider-grpc")]
            commitment: None,
            #[cfg(feature = "provider-grpc")]
            vote: None,
            #[cfg(feature = "provider-grpc")]
            failed: None,
            #[cfg(feature = "provider-grpc")]
            signature: None,
            #[cfg(feature = "provider-grpc")]
            account_include: None,
            #[cfg(feature = "provider-grpc")]
            account_exclude: None,
            #[cfg(feature = "provider-grpc")]
            account_required: None,
            #[cfg(feature = "provider-grpc")]
            accounts: None,
            #[cfg(feature = "provider-grpc")]
            owners: None,
            #[cfg(feature = "provider-grpc")]
            require_transaction_signature: None,
            #[cfg(feature = "provider-grpc")]
            readiness: None,
            #[cfg(feature = "provider-grpc")]
            role: None,
            #[cfg(feature = "provider-grpc")]
            priority: None,
            url: None,
            #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
            account: None,
            #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
            program_id: None,
            #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
            logs_filter: None,
            #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
            mentions: None,
            #[cfg(all(feature = "provider-grpc", feature = "provider-websocket"))]
            requests: None,
            entrypoints: None,
            #[cfg(feature = "gossip-bootstrap")]
            runtime_mode: None,
            entrypoint_pinned: None,
            kernel_bypass: None,
        }
    }

    #[test]
    fn validate_ingress_accepts_mixed_websocket_and_raw_runtime_config() {
        let config = RuntimeHostConfig {
            app_name: "mixed-app".to_owned(),
            runtime_environment: HashMap::new(),
            ingress: vec![websocket_ingress("ws-a"), direct_shreds_ingress("direct-a")],
            #[cfg(feature = "provider-grpc")]
            fan_in: Some(FanInConfig { strategy: 2 }),
            plugin_workers: vec![],
        };

        let result = validate_ingress(&config);
        assert!(result.is_ok());
    }

    #[cfg(feature = "provider-grpc")]
    #[test]
    fn provider_plugin_config_matches_websocket_logs_mode() {
        let config = provider_plugin_config(&[ProviderStreamMode::WebsocketLogs]);

        assert!(config.transaction_log);
        assert!(!config.transaction);
        assert!(!config.transaction_status);
        assert!(!config.account_update);
        assert!(!config.block_meta);
        assert!(!config.slot_status);
        assert!(!config.recent_blockhash);
    }

    #[cfg(feature = "provider-grpc")]
    #[test]
    fn provider_plugin_config_unions_multiple_provider_modes() {
        let config = provider_plugin_config(&[
            ProviderStreamMode::YellowstoneGrpcTransactionStatus,
            ProviderStreamMode::YellowstoneGrpcAccounts,
        ]);

        assert!(!config.transaction);
        assert!(config.transaction_status);
        assert!(config.account_update);
        assert!(!config.block_meta);
        assert!(!config.slot_status);
        assert!(!config.recent_blockhash);
        assert!(!config.transaction_log);
    }
}
