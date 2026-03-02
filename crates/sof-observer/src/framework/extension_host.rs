#[cfg(test)]
use std::str::FromStr;
use std::{
    collections::HashSet,
    io::ErrorKind,
    net::SocketAddr,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::{SinkExt, StreamExt};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream, UdpSocket},
    sync::mpsc,
    task::JoinHandle,
    time::timeout,
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

use crate::framework::extension::{
    ExtensionCapability, ExtensionManifest, ExtensionResourceSpec, ExtensionShutdownContext,
    ExtensionStartupContext, PacketSubscription, RuntimeExtension, RuntimePacketEvent,
    RuntimePacketEventClass, RuntimePacketSource, RuntimePacketSourceKind, RuntimePacketTransport,
    RuntimeWebSocketFrameType, WsConnectorSpec,
};

/// Default bounded queue capacity for asynchronous extension packet dispatch.
const DEFAULT_EVENT_QUEUE_CAPACITY: usize = 8_192;
/// Number of initial dropped-event warnings emitted without sampling.
const INITIAL_DROP_LOG_LIMIT: u64 = 5;
/// Warning sample cadence after the initial dropped-event warning burst.
const DROP_LOG_SAMPLE_EVERY: u64 = 1_000;
/// Default timeout applied to extension startup hooks.
const DEFAULT_STARTUP_TIMEOUT_SECS: u64 = 5;
/// Default timeout applied to extension shutdown hooks.
const DEFAULT_SHUTDOWN_TIMEOUT_SECS: u64 = 3;
/// Per-read fallback buffer size used for extension resource sockets.
const DEFAULT_RESOURCE_READ_BUFFER_BYTES: usize = 2_048;

/// Startup failure record for one extension.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RuntimeExtensionStartupFailure {
    /// Extension identifier.
    pub extension: &'static str,
    /// Human-readable startup failure reason.
    pub reason: String,
}

/// Startup result summary for one runtime extension host.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RuntimeExtensionStartupReport {
    /// Number of extensions registered on the host.
    pub discovered_extensions: usize,
    /// Number of extensions that completed startup successfully.
    pub active_extensions: usize,
    /// Number of extensions that failed startup.
    pub failed_extensions: usize,
    /// Per-extension startup failures.
    pub failures: Vec<RuntimeExtensionStartupFailure>,
}

impl RuntimeExtensionStartupReport {
    fn empty(discovered_extensions: usize) -> Self {
        Self {
            discovered_extensions,
            active_extensions: 0,
            failed_extensions: 0,
            failures: Vec::new(),
        }
    }
}

/// Capability allowlist used by runtime extension startup validation.
#[derive(Debug, Clone)]
pub struct RuntimeExtensionCapabilityPolicy {
    allowed: HashSet<ExtensionCapability>,
}

impl Default for RuntimeExtensionCapabilityPolicy {
    fn default() -> Self {
        Self {
            allowed: ExtensionCapability::all().into_iter().collect(),
        }
    }
}

impl RuntimeExtensionCapabilityPolicy {
    /// Returns a policy that allows all known extension capabilities.
    #[must_use]
    pub fn allow_all() -> Self {
        Self::default()
    }

    /// Returns true if this policy allows the provided capability.
    #[must_use]
    pub fn allows(&self, capability: ExtensionCapability) -> bool {
        self.allowed.contains(&capability)
    }

    /// Removes one capability from the allowlist.
    #[must_use]
    pub fn without(mut self, capability: ExtensionCapability) -> Self {
        self.allowed.remove(&capability);
        self
    }
}

/// Builder for constructing an immutable [`RuntimeExtensionHost`].
pub struct RuntimeExtensionHostBuilder {
    extensions: Vec<Arc<dyn RuntimeExtension>>,
    event_queue_capacity: usize,
    startup_timeout: Duration,
    shutdown_timeout: Duration,
    capability_policy: RuntimeExtensionCapabilityPolicy,
}

impl Default for RuntimeExtensionHostBuilder {
    fn default() -> Self {
        Self {
            extensions: Vec::new(),
            event_queue_capacity: DEFAULT_EVENT_QUEUE_CAPACITY,
            startup_timeout: Duration::from_secs(DEFAULT_STARTUP_TIMEOUT_SECS),
            shutdown_timeout: Duration::from_secs(DEFAULT_SHUTDOWN_TIMEOUT_SECS),
            capability_policy: RuntimeExtensionCapabilityPolicy::default(),
        }
    }
}

impl RuntimeExtensionHostBuilder {
    /// Creates an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets bounded async event queue capacity for extension packet dispatch.
    #[must_use]
    pub fn with_event_queue_capacity(mut self, capacity: usize) -> Self {
        self.event_queue_capacity = capacity.max(1);
        self
    }

    /// Sets startup timeout for `RuntimeExtension::on_startup`.
    #[must_use]
    pub fn with_startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    /// Sets shutdown timeout for `RuntimeExtension::on_shutdown`.
    #[must_use]
    pub fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Sets the capability policy applied during extension startup validation.
    #[must_use]
    pub fn with_capability_policy(mut self, policy: RuntimeExtensionCapabilityPolicy) -> Self {
        self.capability_policy = policy;
        self
    }

    /// Adds one extension value by storing it behind `Arc`.
    #[must_use]
    pub fn add_extension<E>(mut self, extension: E) -> Self
    where
        E: RuntimeExtension,
    {
        self.extensions.push(Arc::new(extension));
        self
    }

    /// Adds one already-shared extension.
    #[must_use]
    pub fn add_shared_extension(mut self, extension: Arc<dyn RuntimeExtension>) -> Self {
        self.extensions.push(extension);
        self
    }

    /// Finalizes the builder into an immutable host.
    #[must_use]
    pub fn build(self) -> RuntimeExtensionHost {
        RuntimeExtensionHost {
            inner: Arc::new(RuntimeExtensionHostInner {
                extensions: Arc::from(self.extensions),
                event_queue_capacity: self.event_queue_capacity.max(1),
                startup_timeout: self.startup_timeout,
                shutdown_timeout: self.shutdown_timeout,
                capability_policy: self.capability_policy,
                runtime_state: RwLock::new(None),
            }),
        }
    }
}

struct RuntimeExtensionHostInner {
    extensions: Arc<[Arc<dyn RuntimeExtension>]>,
    event_queue_capacity: usize,
    startup_timeout: Duration,
    shutdown_timeout: Duration,
    capability_policy: RuntimeExtensionCapabilityPolicy,
    runtime_state: RwLock<Option<RuntimeExtensionRuntimeState>>,
}

struct RuntimeExtensionRuntimeState {
    active_extensions: Arc<[Arc<ActiveRuntimeExtension>]>,
}

struct ActiveRuntimeExtension {
    extension: Arc<dyn RuntimeExtension>,
    name: &'static str,
    capabilities: HashSet<ExtensionCapability>,
    subscriptions: Arc<[PacketSubscription]>,
    dispatcher: ExtensionDispatcher,
    resource_handles: Mutex<Vec<JoinHandle<()>>>,
}

impl ActiveRuntimeExtension {
    fn dropped_event_count(&self) -> u64 {
        self.dispatcher.dropped_count()
    }

    fn push_resource_handle(&self, handle: JoinHandle<()>) {
        if let Ok(mut guard) = self.resource_handles.lock() {
            guard.push(handle);
        }
    }

    fn abort_resource_handles(&self) {
        if let Ok(mut guard) = self.resource_handles.lock() {
            for handle in guard.drain(..) {
                handle.abort();
            }
        }
    }
}

#[derive(Clone)]
struct ExtensionDispatcher {
    tx: mpsc::Sender<RuntimePacketEvent>,
    dropped_events: Arc<AtomicU64>,
    worker: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl ExtensionDispatcher {
    fn spawn(
        extension: Arc<dyn RuntimeExtension>,
        extension_name: &'static str,
        queue_capacity: usize,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<RuntimePacketEvent>(queue_capacity.max(1));
        let dropped_events = Arc::new(AtomicU64::new(0));
        let worker_extension = Arc::clone(&extension);
        let worker = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let callback_extension = Arc::clone(&worker_extension);
                let callback_result = tokio::spawn(async move {
                    callback_extension.on_packet_received(event).await;
                })
                .await;
                if let Err(error) = callback_result {
                    if error.is_panic() {
                        let payload = error.into_panic();
                        let panic_message = panic_payload_to_string(payload.as_ref());
                        tracing::error!(
                            extension = extension_name,
                            panic = %panic_message,
                            "runtime extension packet callback panicked; continuing runtime"
                        );
                    } else {
                        tracing::error!(
                            extension = extension_name,
                            "runtime extension packet callback cancelled"
                        );
                    }
                }
            }
        });
        Self {
            tx,
            dropped_events,
            worker: Arc::new(Mutex::new(Some(worker))),
        }
    }

    fn dispatch(&self, extension_name: &'static str, event: RuntimePacketEvent) {
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.record_drop(extension_name, "queue full");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.record_drop(extension_name, "queue closed");
            }
        }
    }

    fn dropped_count(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    fn abort_worker(&self) {
        if let Ok(mut guard) = self.worker.lock()
            && let Some(handle) = guard.take()
        {
            handle.abort();
        }
    }

    fn record_drop(&self, extension_name: &'static str, reason: &'static str) {
        let dropped = self
            .dropped_events
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if dropped <= INITIAL_DROP_LOG_LIMIT || dropped.is_multiple_of(DROP_LOG_SAMPLE_EVERY) {
            tracing::warn!(
                extension = extension_name,
                reason,
                dropped,
                "dropping runtime extension packet event to protect ingest hot path"
            );
        }
    }
}

/// Separate runtime extension host from observer plugin host.
#[derive(Clone)]
pub struct RuntimeExtensionHost {
    inner: Arc<RuntimeExtensionHostInner>,
}

impl Default for RuntimeExtensionHost {
    fn default() -> Self {
        RuntimeExtensionHostBuilder::new().build()
    }
}

impl RuntimeExtensionHost {
    /// Starts a new host builder.
    #[must_use]
    pub fn builder() -> RuntimeExtensionHostBuilder {
        RuntimeExtensionHostBuilder::new()
    }

    /// Returns true when no extensions are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.extensions.is_empty()
    }

    /// Returns number of registered extensions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.extensions.len()
    }

    /// Returns extension identifiers in registration order.
    #[must_use]
    pub fn extension_names(&self) -> Vec<&'static str> {
        self.inner
            .extensions
            .iter()
            .map(|extension| extension.name())
            .collect()
    }

    /// Returns active extension identifiers after startup.
    #[must_use]
    pub fn active_extension_names(&self) -> Vec<&'static str> {
        self.inner
            .runtime_state
            .read()
            .ok()
            .and_then(|guard| {
                guard.as_ref().map(|state| {
                    state
                        .active_extensions
                        .iter()
                        .map(|extension| extension.name)
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    /// Returns total dropped packet events across all active runtime extensions.
    #[must_use]
    pub fn dropped_event_count(&self) -> u64 {
        self.inner
            .runtime_state
            .read()
            .ok()
            .and_then(|guard| {
                guard.as_ref().map(|state| {
                    state
                        .active_extensions
                        .iter()
                        .map(|extension| extension.dropped_event_count())
                        .sum()
                })
            })
            .unwrap_or_default()
    }

    /// Returns dropped packet event counts per active runtime extension.
    #[must_use]
    pub fn dropped_event_counts_by_extension(&self) -> Vec<(&'static str, u64)> {
        self.inner
            .runtime_state
            .read()
            .ok()
            .and_then(|guard| {
                guard.as_ref().map(|state| {
                    state
                        .active_extensions
                        .iter()
                        .map(|extension| (extension.name, extension.dropped_event_count()))
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    /// Runs startup hooks, validates manifests, and provisions extension resources.
    pub async fn startup(&self) -> RuntimeExtensionStartupReport {
        if let Ok(guard) = self.inner.runtime_state.read()
            && let Some(state) = guard.as_ref()
        {
            return RuntimeExtensionStartupReport {
                discovered_extensions: self.inner.extensions.len(),
                active_extensions: state.active_extensions.len(),
                failed_extensions: 0,
                failures: Vec::new(),
            };
        }

        let mut report = RuntimeExtensionStartupReport::empty(self.inner.extensions.len());
        let mut active_extensions: Vec<Arc<ActiveRuntimeExtension>> = Vec::new();

        for extension in self.inner.extensions.iter() {
            let extension = Arc::clone(extension);
            let extension_name = extension.name();
            let startup_context = ExtensionStartupContext { extension_name };
            let manifest_result = timeout(
                self.inner.startup_timeout,
                extension.on_startup(startup_context),
            )
            .await;
            let manifest = match manifest_result {
                Ok(Ok(manifest)) => manifest,
                Ok(Err(error)) => {
                    report.failures.push(RuntimeExtensionStartupFailure {
                        extension: extension_name,
                        reason: error.to_string(),
                    });
                    continue;
                }
                Err(_elapsed) => {
                    report.failures.push(RuntimeExtensionStartupFailure {
                        extension: extension_name,
                        reason: format!(
                            "startup hook timed out after {}ms",
                            self.inner.startup_timeout.as_millis()
                        ),
                    });
                    continue;
                }
            };

            let validated =
                match validate_manifest(extension_name, &manifest, &self.inner.capability_policy) {
                    Ok(validated) => validated,
                    Err(reason) => {
                        report.failures.push(RuntimeExtensionStartupFailure {
                            extension: extension_name,
                            reason,
                        });
                        continue;
                    }
                };

            let active = Arc::new(ActiveRuntimeExtension {
                extension: Arc::clone(&extension),
                name: extension_name,
                capabilities: validated.capabilities,
                subscriptions: Arc::from(validated.subscriptions),
                dispatcher: ExtensionDispatcher::spawn(
                    extension,
                    extension_name,
                    self.inner.event_queue_capacity,
                ),
                resource_handles: Mutex::new(Vec::new()),
            });

            if let Err(reason) = self.provision_resources(&active, &manifest.resources).await {
                active.abort_resource_handles();
                active.dispatcher.abort_worker();
                report.failures.push(RuntimeExtensionStartupFailure {
                    extension: extension_name,
                    reason,
                });
                continue;
            }

            active_extensions.push(active);
        }

        report.active_extensions = active_extensions.len();
        report.failed_extensions = report.failures.len();

        if let Ok(mut guard) = self.inner.runtime_state.write() {
            *guard = Some(RuntimeExtensionRuntimeState {
                active_extensions: Arc::from(active_extensions),
            });
        }

        report
    }

    /// Emits one observer ingress packet into runtime extension dispatch.
    pub fn on_observer_packet(&self, source: SocketAddr, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let source_meta = RuntimePacketSource {
            kind: RuntimePacketSourceKind::ObserverIngress,
            transport: RuntimePacketTransport::Udp,
            event_class: RuntimePacketEventClass::Packet,
            owner_extension: None,
            resource_id: None,
            shared_tag: None,
            websocket_frame_type: None,
            local_addr: None,
            remote_addr: Some(source),
        };
        let event = RuntimePacketEvent {
            source: source_meta,
            bytes: Arc::from(bytes),
            observed_unix_ms: current_unix_ms(),
        };
        self.dispatch_runtime_packet(event);
    }

    /// Emits one extension-resource packet into runtime extension dispatch.
    pub fn emit_extension_packet(&self, source: RuntimePacketSource, bytes: Arc<[u8]>) {
        if bytes.is_empty() && source.event_class == RuntimePacketEventClass::Packet {
            return;
        }
        let event = RuntimePacketEvent {
            source,
            bytes,
            observed_unix_ms: current_unix_ms(),
        };
        self.dispatch_runtime_packet(event);
    }

    /// Runs shutdown hooks and force-cancels lingering extension tasks afterwards.
    pub async fn shutdown(&self) {
        let state = self
            .inner
            .runtime_state
            .write()
            .ok()
            .and_then(|mut guard| guard.take());
        let Some(state) = state else {
            return;
        };

        for extension in state.active_extensions.iter() {
            extension.abort_resource_handles();
        }

        for extension in state.active_extensions.iter() {
            let shutdown_context = ExtensionShutdownContext {
                extension_name: extension.name,
            };
            let shutdown_result = timeout(
                self.inner.shutdown_timeout,
                extension.extension.on_shutdown(shutdown_context),
            )
            .await;
            if shutdown_result.is_err() {
                tracing::warn!(
                    extension = extension.name,
                    timeout_ms = self.inner.shutdown_timeout.as_millis(),
                    "runtime extension shutdown timed out; force-cancelling"
                );
            }
        }

        for extension in state.active_extensions.iter() {
            extension.dispatcher.abort_worker();
        }
    }

    fn dispatch_runtime_packet(&self, event: RuntimePacketEvent) {
        let Some(active_extensions) = self.inner.runtime_state.read().ok().and_then(|guard| {
            guard
                .as_ref()
                .map(|state| Arc::clone(&state.active_extensions))
        }) else {
            return;
        };
        for extension in active_extensions.iter() {
            if !extension_can_observe_event(extension, &event) {
                continue;
            }
            if !extension
                .subscriptions
                .iter()
                .any(|subscription| subscription.matches(&event))
            {
                continue;
            }
            extension.dispatcher.dispatch(extension.name, event.clone());
        }
    }

    async fn provision_resources(
        &self,
        extension: &Arc<ActiveRuntimeExtension>,
        resources: &[ExtensionResourceSpec],
    ) -> Result<(), String> {
        for resource in resources {
            let handle = match resource {
                ExtensionResourceSpec::UdpListener(spec) => {
                    spawn_udp_listener(self.clone(), extension, spec.clone()).await?
                }
                ExtensionResourceSpec::TcpListener(spec) => {
                    spawn_tcp_listener(self.clone(), extension, spec.clone()).await?
                }
                ExtensionResourceSpec::TcpConnector(spec) => {
                    spawn_tcp_connector(self.clone(), extension, spec.clone()).await?
                }
                ExtensionResourceSpec::WsConnector(spec) => {
                    spawn_ws_connector(self.clone(), extension, spec.clone()).await?
                }
            };
            extension.push_resource_handle(handle);
        }
        Ok(())
    }
}

fn extension_can_observe_event(
    extension: &ActiveRuntimeExtension,
    event: &RuntimePacketEvent,
) -> bool {
    match event.source.kind {
        RuntimePacketSourceKind::ObserverIngress => extension
            .capabilities
            .contains(&ExtensionCapability::ObserveObserverIngress),
        RuntimePacketSourceKind::ExtensionResource => {
            let owner_name = event.source.owner_extension.as_deref();
            if owner_name == Some(extension.name) {
                return true;
            }
            event.source.shared_tag.is_some()
                && extension
                    .capabilities
                    .contains(&ExtensionCapability::ObserveSharedExtensionStream)
        }
    }
}

async fn spawn_udp_listener(
    host: RuntimeExtensionHost,
    extension: &Arc<ActiveRuntimeExtension>,
    spec: crate::framework::extension::UdpListenerSpec,
) -> Result<JoinHandle<()>, String> {
    let socket = UdpSocket::bind(spec.bind_addr)
        .await
        .map_err(|error| format!("failed to bind udp listener {}: {error}", spec.bind_addr))?;
    let local_addr = socket.local_addr().ok();
    let owner_extension = extension.name.to_owned();
    let resource_id = spec.resource_id;
    let shared_tag = visibility_tag(spec.visibility);
    let read_buffer_bytes = spec
        .read_buffer_bytes
        .max(DEFAULT_RESOURCE_READ_BUFFER_BYTES);
    let handle = tokio::spawn(async move {
        let mut buffer = vec![0_u8; read_buffer_bytes];
        loop {
            match socket.recv_from(&mut buffer).await {
                Ok((len, remote_addr)) => {
                    if len == 0 {
                        continue;
                    }
                    let source = RuntimePacketSource {
                        kind: RuntimePacketSourceKind::ExtensionResource,
                        transport: RuntimePacketTransport::Udp,
                        event_class: RuntimePacketEventClass::Packet,
                        owner_extension: Some(owner_extension.clone()),
                        resource_id: Some(resource_id.clone()),
                        shared_tag: shared_tag.clone(),
                        websocket_frame_type: None,
                        local_addr,
                        remote_addr: Some(remote_addr),
                    };
                    host.emit_extension_packet(source, Arc::from(&buffer[..len]));
                }
                Err(error) => {
                    if error.kind() != ErrorKind::Interrupted {
                        tracing::warn!(
                            extension = owner_extension,
                            resource_id,
                            error = %error,
                            "udp extension listener read loop terminated"
                        );
                    }
                    break;
                }
            }
        }
    });
    Ok(handle)
}

async fn spawn_tcp_listener(
    host: RuntimeExtensionHost,
    extension: &Arc<ActiveRuntimeExtension>,
    spec: crate::framework::extension::TcpListenerSpec,
) -> Result<JoinHandle<()>, String> {
    let listener = TcpListener::bind(spec.bind_addr)
        .await
        .map_err(|error| format!("failed to bind tcp listener {}: {error}", spec.bind_addr))?;
    let owner_extension = extension.name.to_owned();
    let resource_id = spec.resource_id;
    let shared_tag = visibility_tag(spec.visibility);
    let read_buffer_bytes = spec
        .read_buffer_bytes
        .max(DEFAULT_RESOURCE_READ_BUFFER_BYTES);
    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, remote_addr)) => {
                    let local_addr = stream.local_addr().ok();
                    read_tcp_stream_packets(
                        &host,
                        &owner_extension,
                        &resource_id,
                        shared_tag.as_deref(),
                        RuntimePacketTransport::Tcp,
                        stream,
                        local_addr,
                        Some(remote_addr),
                        read_buffer_bytes,
                    )
                    .await;
                }
                Err(error) => {
                    tracing::warn!(
                        extension = owner_extension,
                        resource_id,
                        error = %error,
                        "tcp extension listener accept loop terminated"
                    );
                    break;
                }
            }
        }
    });
    Ok(handle)
}

async fn spawn_tcp_connector(
    host: RuntimeExtensionHost,
    extension: &Arc<ActiveRuntimeExtension>,
    spec: crate::framework::extension::TcpConnectorSpec,
) -> Result<JoinHandle<()>, String> {
    let stream = TcpStream::connect(spec.remote_addr)
        .await
        .map_err(|error| format!("failed to connect tcp {}: {error}", spec.remote_addr))?;
    let local_addr = stream.local_addr().ok();
    let remote_addr = stream.peer_addr().ok();
    let owner_extension = extension.name.to_owned();
    let resource_id = spec.resource_id;
    let shared_tag = visibility_tag(spec.visibility);
    let read_buffer_bytes = spec
        .read_buffer_bytes
        .max(DEFAULT_RESOURCE_READ_BUFFER_BYTES);
    let handle = tokio::spawn(async move {
        read_tcp_stream_packets(
            &host,
            &owner_extension,
            &resource_id,
            shared_tag.as_deref(),
            RuntimePacketTransport::Tcp,
            stream,
            local_addr,
            remote_addr,
            read_buffer_bytes,
        )
        .await;
    });
    Ok(handle)
}

async fn spawn_ws_connector(
    host: RuntimeExtensionHost,
    extension: &Arc<ActiveRuntimeExtension>,
    spec: WsConnectorSpec,
) -> Result<JoinHandle<()>, String> {
    let (stream, _response) = connect_async(spec.url.as_str())
        .await
        .map_err(|error| format!("failed to connect websocket {}: {error}", spec.url))?;
    let io = stream.get_ref().get_ref();
    let local_addr = io.local_addr().ok();
    let peer_addr = io.peer_addr().ok();
    let owner_extension = extension.name.to_owned();
    let resource_id = spec.resource_id;
    let shared_tag = visibility_tag(spec.visibility);
    let max_payload_chunk_bytes = spec
        .read_buffer_bytes
        .max(DEFAULT_RESOURCE_READ_BUFFER_BYTES);
    let handle = tokio::spawn(async move {
        read_websocket_messages(
            &host,
            &owner_extension,
            &resource_id,
            shared_tag.as_deref(),
            stream,
            local_addr,
            peer_addr,
            max_payload_chunk_bytes,
        )
        .await;
    });
    Ok(handle)
}

async fn read_tcp_stream_packets(
    host: &RuntimeExtensionHost,
    owner_extension: &str,
    resource_id: &str,
    shared_tag: Option<&str>,
    transport: RuntimePacketTransport,
    mut stream: TcpStream,
    local_addr: Option<SocketAddr>,
    remote_addr: Option<SocketAddr>,
    read_buffer_bytes: usize,
) {
    let mut buffer = vec![0_u8; read_buffer_bytes.max(1)];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) => break,
            Ok(len) => {
                let source = RuntimePacketSource {
                    kind: RuntimePacketSourceKind::ExtensionResource,
                    transport,
                    event_class: RuntimePacketEventClass::Packet,
                    owner_extension: Some(owner_extension.to_owned()),
                    resource_id: Some(resource_id.to_owned()),
                    shared_tag: shared_tag.map(str::to_owned),
                    websocket_frame_type: None,
                    local_addr,
                    remote_addr,
                };
                host.emit_extension_packet(source, Arc::from(&buffer[..len]));
            }
            Err(error) => {
                if error.kind() != ErrorKind::Interrupted {
                    tracing::warn!(
                        extension = owner_extension,
                        resource_id,
                        error = %error,
                        "extension tcp stream read loop terminated"
                    );
                }
                break;
            }
        }
    }
}

async fn read_websocket_messages(
    host: &RuntimeExtensionHost,
    owner_extension: &str,
    resource_id: &str,
    shared_tag: Option<&str>,
    mut stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    local_addr: Option<SocketAddr>,
    remote_addr: Option<SocketAddr>,
    max_payload_chunk_bytes: usize,
) {
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => {
                emit_extension_resource_payload(
                    host,
                    owner_extension,
                    resource_id,
                    shared_tag,
                    RuntimePacketTransport::WebSocket,
                    Some(RuntimeWebSocketFrameType::Text),
                    local_addr,
                    remote_addr,
                    text.as_str().as_bytes(),
                    max_payload_chunk_bytes,
                );
            }
            Some(Ok(Message::Binary(bytes))) => {
                emit_extension_resource_payload(
                    host,
                    owner_extension,
                    resource_id,
                    shared_tag,
                    RuntimePacketTransport::WebSocket,
                    Some(RuntimeWebSocketFrameType::Binary),
                    local_addr,
                    remote_addr,
                    bytes.as_ref(),
                    max_payload_chunk_bytes,
                );
            }
            Some(Ok(Message::Ping(payload))) => {
                emit_extension_resource_payload(
                    host,
                    owner_extension,
                    resource_id,
                    shared_tag,
                    RuntimePacketTransport::WebSocket,
                    Some(RuntimeWebSocketFrameType::Ping),
                    local_addr,
                    remote_addr,
                    payload.as_ref(),
                    max_payload_chunk_bytes,
                );
                if let Err(error) = stream.send(Message::Pong(payload)).await {
                    tracing::warn!(
                        extension = owner_extension,
                        resource_id,
                        error = %error,
                        "failed to send websocket pong frame; stopping connector"
                    );
                    break;
                }
            }
            Some(Ok(Message::Pong(payload))) => {
                emit_extension_resource_payload(
                    host,
                    owner_extension,
                    resource_id,
                    shared_tag,
                    RuntimePacketTransport::WebSocket,
                    Some(RuntimeWebSocketFrameType::Pong),
                    local_addr,
                    remote_addr,
                    payload.as_ref(),
                    max_payload_chunk_bytes,
                );
            }
            Some(Ok(Message::Close(frame))) => {
                let close_payload = frame
                    .as_ref()
                    .map(|frame| frame.reason.as_bytes())
                    .unwrap_or_default();
                emit_extension_resource_event(
                    host,
                    owner_extension,
                    resource_id,
                    shared_tag,
                    RuntimePacketTransport::WebSocket,
                    RuntimePacketEventClass::ConnectionClosed,
                    None,
                    local_addr,
                    remote_addr,
                    Arc::from(close_payload),
                );
                tracing::info!(
                    extension = owner_extension,
                    resource_id,
                    close_code = frame.as_ref().map(|frame| u16::from(frame.code)),
                    close_reason = frame
                        .as_ref()
                        .map(|frame| frame.reason.to_string())
                        .unwrap_or_default(),
                    "websocket connector closed by remote peer"
                );
                break;
            }
            Some(Ok(Message::Frame(_))) => {
                // Internal tungstenite frame detail; user-facing callbacks receive decoded messages.
            }
            Some(Err(error)) => {
                tracing::warn!(
                    extension = owner_extension,
                    resource_id,
                    error = %error,
                    "websocket connector read loop terminated"
                );
                break;
            }
            None => break,
        }
    }
}

fn emit_extension_resource_payload(
    host: &RuntimeExtensionHost,
    owner_extension: &str,
    resource_id: &str,
    shared_tag: Option<&str>,
    transport: RuntimePacketTransport,
    websocket_frame_type: Option<RuntimeWebSocketFrameType>,
    local_addr: Option<SocketAddr>,
    remote_addr: Option<SocketAddr>,
    payload: &[u8],
    max_payload_chunk_bytes: usize,
) {
    let chunk_size = max_payload_chunk_bytes.max(1);
    for chunk in payload.chunks(chunk_size) {
        emit_extension_resource_event(
            host,
            owner_extension,
            resource_id,
            shared_tag,
            transport,
            RuntimePacketEventClass::Packet,
            websocket_frame_type,
            local_addr,
            remote_addr,
            Arc::from(chunk),
        );
    }
}

fn emit_extension_resource_event(
    host: &RuntimeExtensionHost,
    owner_extension: &str,
    resource_id: &str,
    shared_tag: Option<&str>,
    transport: RuntimePacketTransport,
    event_class: RuntimePacketEventClass,
    websocket_frame_type: Option<RuntimeWebSocketFrameType>,
    local_addr: Option<SocketAddr>,
    remote_addr: Option<SocketAddr>,
    bytes: Arc<[u8]>,
) {
    let source = RuntimePacketSource {
        kind: RuntimePacketSourceKind::ExtensionResource,
        transport,
        event_class,
        owner_extension: Some(owner_extension.to_owned()),
        resource_id: Some(resource_id.to_owned()),
        shared_tag: shared_tag.map(str::to_owned),
        websocket_frame_type,
        local_addr,
        remote_addr,
    };
    host.emit_extension_packet(source, bytes);
}

fn visibility_tag(
    visibility: crate::framework::extension::ExtensionStreamVisibility,
) -> Option<String> {
    match visibility {
        crate::framework::extension::ExtensionStreamVisibility::Private => None,
        crate::framework::extension::ExtensionStreamVisibility::Shared { tag } => Some(tag),
    }
}

struct ValidatedManifest {
    capabilities: HashSet<ExtensionCapability>,
    subscriptions: Vec<PacketSubscription>,
}

fn validate_manifest(
    extension_name: &'static str,
    manifest: &ExtensionManifest,
    policy: &RuntimeExtensionCapabilityPolicy,
) -> Result<ValidatedManifest, String> {
    let capabilities: HashSet<ExtensionCapability> =
        manifest.capabilities.iter().copied().collect();
    for capability in &capabilities {
        if !policy.allows(*capability) {
            return Err(format!(
                "capability `{capability:?}` is not allowed by runtime policy"
            ));
        }
    }

    let mut resource_ids = HashSet::<String>::new();
    for resource in &manifest.resources {
        let (resource_id, required_capability) = match resource {
            ExtensionResourceSpec::UdpListener(spec) => {
                (&spec.resource_id, ExtensionCapability::BindUdp)
            }
            ExtensionResourceSpec::TcpListener(spec) => {
                (&spec.resource_id, ExtensionCapability::BindTcp)
            }
            ExtensionResourceSpec::TcpConnector(spec) => {
                (&spec.resource_id, ExtensionCapability::ConnectTcp)
            }
            ExtensionResourceSpec::WsConnector(spec) => {
                (&spec.resource_id, ExtensionCapability::ConnectWebSocket)
            }
        };
        if !resource_ids.insert(resource_id.clone()) {
            return Err(format!(
                "duplicate resource_id `{resource_id}` in startup manifest for extension `{extension_name}`"
            ));
        }
        if !capabilities.contains(&required_capability) {
            return Err(format!(
                "resource `{resource_id}` requires undeclared capability `{required_capability:?}`"
            ));
        }
    }

    Ok(ValidatedManifest {
        capabilities,
        subscriptions: manifest.subscriptions.clone(),
    })
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn panic_payload_to_string(payload: &(dyn std::any::Any + Send)) -> String {
    payload.downcast_ref::<&str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "non-string panic payload".to_owned())
        },
        |message| (*message).to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    use async_trait::async_trait;

    struct CounterExtension {
        name: &'static str,
        startup_manifest: ExtensionManifest,
        packet_count: Arc<AtomicUsize>,
        shutdown_wait: Duration,
        shutdown_called: Arc<AtomicBool>,
    }

    #[async_trait]
    impl RuntimeExtension for CounterExtension {
        fn name(&self) -> &'static str {
            self.name
        }

        async fn on_startup(
            &self,
            _ctx: ExtensionStartupContext,
        ) -> Result<ExtensionManifest, crate::framework::extension::ExtensionStartupError> {
            Ok(self.startup_manifest.clone())
        }

        async fn on_packet_received(&self, _event: RuntimePacketEvent) {
            self.packet_count.fetch_add(1, Ordering::Relaxed);
        }

        async fn on_shutdown(&self, _ctx: ExtensionShutdownContext) {
            self.shutdown_called.store(true, Ordering::Relaxed);
            if !self.shutdown_wait.is_zero() {
                tokio::time::sleep(self.shutdown_wait).await;
            }
        }
    }

    struct StartupFailExtension;

    #[async_trait]
    impl RuntimeExtension for StartupFailExtension {
        fn name(&self) -> &'static str {
            "startup-fail"
        }

        async fn on_startup(
            &self,
            _ctx: ExtensionStartupContext,
        ) -> Result<ExtensionManifest, crate::framework::extension::ExtensionStartupError> {
            Err(crate::framework::extension::ExtensionStartupError::new(
                "intentional startup fail",
            ))
        }
    }

    #[tokio::test]
    async fn startup_failure_isolated_per_extension() {
        let ok_counter = Arc::new(AtomicUsize::new(0));
        let host = RuntimeExtensionHost::builder()
            .add_extension(StartupFailExtension)
            .add_extension(CounterExtension {
                name: "ok-extension",
                startup_manifest: ExtensionManifest {
                    capabilities: vec![ExtensionCapability::ObserveObserverIngress],
                    resources: Vec::new(),
                    subscriptions: vec![PacketSubscription {
                        source_kind: Some(RuntimePacketSourceKind::ObserverIngress),
                        ..PacketSubscription::default()
                    }],
                },
                packet_count: Arc::clone(&ok_counter),
                shutdown_wait: Duration::ZERO,
                shutdown_called: Arc::new(AtomicBool::new(false)),
            })
            .build();
        let report = host.startup().await;
        assert_eq!(report.discovered_extensions, 2);
        assert_eq!(report.active_extensions, 1);
        assert_eq!(report.failed_extensions, 1);
        host.on_observer_packet(
            SocketAddr::from_str("127.0.0.1:8001").expect("valid addr"),
            &[1, 2, 3],
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(ok_counter.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn capability_policy_denies_resource_startup() {
        let host = RuntimeExtensionHost::builder()
            .with_capability_policy(
                RuntimeExtensionCapabilityPolicy::allow_all().without(ExtensionCapability::BindUdp),
            )
            .add_extension(CounterExtension {
                name: "bind-udp-extension",
                startup_manifest: ExtensionManifest {
                    capabilities: vec![ExtensionCapability::BindUdp],
                    resources: vec![ExtensionResourceSpec::UdpListener(
                        crate::framework::extension::UdpListenerSpec {
                            resource_id: "udp-1".to_owned(),
                            bind_addr: SocketAddr::from_str("127.0.0.1:0").expect("valid addr"),
                            visibility:
                                crate::framework::extension::ExtensionStreamVisibility::Private,
                            read_buffer_bytes: 128,
                        },
                    )],
                    subscriptions: Vec::new(),
                },
                packet_count: Arc::new(AtomicUsize::new(0)),
                shutdown_wait: Duration::ZERO,
                shutdown_called: Arc::new(AtomicBool::new(false)),
            })
            .build();
        let report = host.startup().await;
        assert_eq!(report.active_extensions, 0);
        assert_eq!(report.failed_extensions, 1);
    }

    #[tokio::test]
    async fn owner_only_and_shared_stream_visibility() {
        let owner_counter = Arc::new(AtomicUsize::new(0));
        let shared_counter = Arc::new(AtomicUsize::new(0));
        let host = RuntimeExtensionHost::builder()
            .add_extension(CounterExtension {
                name: "owner",
                startup_manifest: ExtensionManifest {
                    capabilities: vec![],
                    resources: Vec::new(),
                    subscriptions: vec![PacketSubscription {
                        source_kind: Some(RuntimePacketSourceKind::ExtensionResource),
                        owner_extension: Some("owner".to_owned()),
                        ..PacketSubscription::default()
                    }],
                },
                packet_count: Arc::clone(&owner_counter),
                shutdown_wait: Duration::ZERO,
                shutdown_called: Arc::new(AtomicBool::new(false)),
            })
            .add_extension(CounterExtension {
                name: "shared-reader",
                startup_manifest: ExtensionManifest {
                    capabilities: vec![ExtensionCapability::ObserveSharedExtensionStream],
                    resources: Vec::new(),
                    subscriptions: vec![PacketSubscription {
                        source_kind: Some(RuntimePacketSourceKind::ExtensionResource),
                        shared_tag: Some("shared-feed".to_owned()),
                        ..PacketSubscription::default()
                    }],
                },
                packet_count: Arc::clone(&shared_counter),
                shutdown_wait: Duration::ZERO,
                shutdown_called: Arc::new(AtomicBool::new(false)),
            })
            .build();
        let report = host.startup().await;
        assert_eq!(report.active_extensions, 2);

        host.emit_extension_packet(
            RuntimePacketSource {
                kind: RuntimePacketSourceKind::ExtensionResource,
                transport: RuntimePacketTransport::Tcp,
                event_class: RuntimePacketEventClass::Packet,
                owner_extension: Some("owner".to_owned()),
                resource_id: Some("feed-1".to_owned()),
                shared_tag: None,
                websocket_frame_type: None,
                local_addr: None,
                remote_addr: None,
            },
            Arc::from(&[1_u8][..]),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(owner_counter.load(Ordering::Relaxed), 1);
        assert_eq!(shared_counter.load(Ordering::Relaxed), 0);

        host.emit_extension_packet(
            RuntimePacketSource {
                kind: RuntimePacketSourceKind::ExtensionResource,
                transport: RuntimePacketTransport::Tcp,
                event_class: RuntimePacketEventClass::Packet,
                owner_extension: Some("owner".to_owned()),
                resource_id: Some("feed-1".to_owned()),
                shared_tag: Some("shared-feed".to_owned()),
                websocket_frame_type: None,
                local_addr: None,
                remote_addr: None,
            },
            Arc::from(&[2_u8][..]),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(owner_counter.load(Ordering::Relaxed), 2);
        assert_eq!(shared_counter.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn websocket_frame_type_subscription_filters_dispatch() {
        let text_counter = Arc::new(AtomicUsize::new(0));
        let any_counter = Arc::new(AtomicUsize::new(0));
        let host = RuntimeExtensionHost::builder()
            .add_extension(CounterExtension {
                name: "text-only",
                startup_manifest: ExtensionManifest {
                    capabilities: vec![ExtensionCapability::ObserveSharedExtensionStream],
                    resources: Vec::new(),
                    subscriptions: vec![PacketSubscription {
                        source_kind: Some(RuntimePacketSourceKind::ExtensionResource),
                        transport: Some(RuntimePacketTransport::WebSocket),
                        shared_tag: Some("ws-shared".to_owned()),
                        websocket_frame_type: Some(RuntimeWebSocketFrameType::Text),
                        ..PacketSubscription::default()
                    }],
                },
                packet_count: Arc::clone(&text_counter),
                shutdown_wait: Duration::ZERO,
                shutdown_called: Arc::new(AtomicBool::new(false)),
            })
            .add_extension(CounterExtension {
                name: "any-frame",
                startup_manifest: ExtensionManifest {
                    capabilities: vec![ExtensionCapability::ObserveSharedExtensionStream],
                    resources: Vec::new(),
                    subscriptions: vec![PacketSubscription {
                        source_kind: Some(RuntimePacketSourceKind::ExtensionResource),
                        transport: Some(RuntimePacketTransport::WebSocket),
                        shared_tag: Some("ws-shared".to_owned()),
                        ..PacketSubscription::default()
                    }],
                },
                packet_count: Arc::clone(&any_counter),
                shutdown_wait: Duration::ZERO,
                shutdown_called: Arc::new(AtomicBool::new(false)),
            })
            .build();
        let report = host.startup().await;
        assert_eq!(report.active_extensions, 2);

        host.emit_extension_packet(
            RuntimePacketSource {
                kind: RuntimePacketSourceKind::ExtensionResource,
                transport: RuntimePacketTransport::WebSocket,
                event_class: RuntimePacketEventClass::Packet,
                owner_extension: Some("owner".to_owned()),
                resource_id: Some("ws-feed".to_owned()),
                shared_tag: Some("ws-shared".to_owned()),
                websocket_frame_type: Some(RuntimeWebSocketFrameType::Text),
                local_addr: None,
                remote_addr: None,
            },
            Arc::from(&[1_u8][..]),
        );
        host.emit_extension_packet(
            RuntimePacketSource {
                kind: RuntimePacketSourceKind::ExtensionResource,
                transport: RuntimePacketTransport::WebSocket,
                event_class: RuntimePacketEventClass::Packet,
                owner_extension: Some("owner".to_owned()),
                resource_id: Some("ws-feed".to_owned()),
                shared_tag: Some("ws-shared".to_owned()),
                websocket_frame_type: Some(RuntimeWebSocketFrameType::Binary),
                local_addr: None,
                remote_addr: None,
            },
            Arc::from(&[2_u8][..]),
        );

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(text_counter.load(Ordering::Relaxed), 1);
        assert_eq!(any_counter.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn connection_closed_event_dispatches_with_empty_payload() {
        let close_counter = Arc::new(AtomicUsize::new(0));
        let host = RuntimeExtensionHost::builder()
            .add_extension(CounterExtension {
                name: "close-reader",
                startup_manifest: ExtensionManifest {
                    capabilities: vec![ExtensionCapability::ObserveSharedExtensionStream],
                    resources: Vec::new(),
                    subscriptions: vec![PacketSubscription {
                        source_kind: Some(RuntimePacketSourceKind::ExtensionResource),
                        transport: Some(RuntimePacketTransport::WebSocket),
                        event_class: Some(RuntimePacketEventClass::ConnectionClosed),
                        shared_tag: Some("ws-close".to_owned()),
                        ..PacketSubscription::default()
                    }],
                },
                packet_count: Arc::clone(&close_counter),
                shutdown_wait: Duration::ZERO,
                shutdown_called: Arc::new(AtomicBool::new(false)),
            })
            .build();
        let report = host.startup().await;
        assert_eq!(report.active_extensions, 1);

        host.emit_extension_packet(
            RuntimePacketSource {
                kind: RuntimePacketSourceKind::ExtensionResource,
                transport: RuntimePacketTransport::WebSocket,
                event_class: RuntimePacketEventClass::ConnectionClosed,
                owner_extension: Some("owner".to_owned()),
                resource_id: Some("ws-feed".to_owned()),
                shared_tag: Some("ws-close".to_owned()),
                websocket_frame_type: None,
                local_addr: None,
                remote_addr: None,
            },
            Arc::from(&[][..]),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(close_counter.load(Ordering::Relaxed), 1);
    }

    struct SlowExtension {
        counter: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl RuntimeExtension for SlowExtension {
        fn name(&self) -> &'static str {
            "slow-extension"
        }

        async fn on_startup(
            &self,
            _ctx: ExtensionStartupContext,
        ) -> Result<ExtensionManifest, crate::framework::extension::ExtensionStartupError> {
            Ok(ExtensionManifest {
                capabilities: vec![ExtensionCapability::ObserveObserverIngress],
                resources: Vec::new(),
                subscriptions: vec![PacketSubscription {
                    source_kind: Some(RuntimePacketSourceKind::ObserverIngress),
                    ..PacketSubscription::default()
                }],
            })
        }

        async fn on_packet_received(&self, _event: RuntimePacketEvent) {
            tokio::time::sleep(Duration::from_millis(120)).await;
            self.counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[tokio::test]
    async fn queue_pressure_drops_events_without_blocking() {
        let counter = Arc::new(AtomicUsize::new(0));
        let host = RuntimeExtensionHost::builder()
            .with_event_queue_capacity(1)
            .add_extension(SlowExtension {
                counter: Arc::clone(&counter),
            })
            .build();
        let report = host.startup().await;
        assert_eq!(report.active_extensions, 1);

        let source = SocketAddr::from_str("127.0.0.1:9001").expect("valid addr");
        for _ in 0..16 {
            host.on_observer_packet(source, &[7_u8; 32]);
        }
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert!(counter.load(Ordering::Relaxed) < 16);
        assert!(host.dropped_event_count() > 0);
    }

    #[tokio::test]
    async fn shutdown_deadline_then_cancel() {
        let shutdown_called = Arc::new(AtomicBool::new(false));
        let host = RuntimeExtensionHost::builder()
            .with_shutdown_timeout(Duration::from_millis(25))
            .add_extension(CounterExtension {
                name: "slow-shutdown",
                startup_manifest: ExtensionManifest {
                    capabilities: vec![ExtensionCapability::ObserveObserverIngress],
                    resources: Vec::new(),
                    subscriptions: vec![PacketSubscription {
                        source_kind: Some(RuntimePacketSourceKind::ObserverIngress),
                        ..PacketSubscription::default()
                    }],
                },
                packet_count: Arc::new(AtomicUsize::new(0)),
                shutdown_wait: Duration::from_millis(250),
                shutdown_called: Arc::clone(&shutdown_called),
            })
            .build();
        let report = host.startup().await;
        assert_eq!(report.active_extensions, 1);

        let started = tokio::time::Instant::now();
        host.shutdown().await;
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_millis(150));
        assert!(shutdown_called.load(Ordering::Relaxed));
    }

    #[tokio::test]
    #[ignore = "requires local socket bind/connect permissions"]
    async fn startup_provisions_udp_tcp_and_ws_resources() {
        let tcp_server = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind tcp server");
        let tcp_server_addr = tcp_server.local_addr().expect("tcp local addr");
        let tcp_server_task = tokio::spawn(async move {
            if let Ok((mut stream, _)) = tcp_server.accept().await {
                let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, b"tcp").await;
            }
        });

        let ws_server = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ws server");
        let ws_server_addr = ws_server.local_addr().expect("ws local addr");
        let ws_server_task = tokio::spawn(async move {
            if let Ok((stream, _)) = ws_server.accept().await
                && let Ok(mut websocket) = tokio_tungstenite::accept_async(stream).await
            {
                let _ = websocket.send(Message::Text("ws".into())).await;
            }
        });

        let host = RuntimeExtensionHost::builder()
            .add_extension(CounterExtension {
                name: "resource-extension",
                startup_manifest: ExtensionManifest {
                    capabilities: vec![
                        ExtensionCapability::BindUdp,
                        ExtensionCapability::BindTcp,
                        ExtensionCapability::ConnectTcp,
                        ExtensionCapability::ConnectWebSocket,
                    ],
                    resources: vec![
                        ExtensionResourceSpec::UdpListener(
                            crate::framework::extension::UdpListenerSpec {
                                resource_id: "udp-listener".to_owned(),
                                bind_addr: SocketAddr::from_str("127.0.0.1:0").expect("addr"),
                                visibility:
                                    crate::framework::extension::ExtensionStreamVisibility::Private,
                                read_buffer_bytes: 128,
                            },
                        ),
                        ExtensionResourceSpec::TcpListener(
                            crate::framework::extension::TcpListenerSpec {
                                resource_id: "tcp-listener".to_owned(),
                                bind_addr: SocketAddr::from_str("127.0.0.1:0").expect("addr"),
                                visibility:
                                    crate::framework::extension::ExtensionStreamVisibility::Private,
                                read_buffer_bytes: 128,
                            },
                        ),
                        ExtensionResourceSpec::TcpConnector(
                            crate::framework::extension::TcpConnectorSpec {
                                resource_id: "tcp-connector".to_owned(),
                                remote_addr: tcp_server_addr,
                                visibility:
                                    crate::framework::extension::ExtensionStreamVisibility::Private,
                                read_buffer_bytes: 128,
                            },
                        ),
                        ExtensionResourceSpec::WsConnector(WsConnectorSpec {
                            resource_id: "ws-connector".to_owned(),
                            url: format!("ws://{ws_server_addr}/feed"),
                            visibility:
                                crate::framework::extension::ExtensionStreamVisibility::Private,
                            read_buffer_bytes: 128,
                        }),
                    ],
                    subscriptions: vec![PacketSubscription {
                        source_kind: Some(RuntimePacketSourceKind::ExtensionResource),
                        ..PacketSubscription::default()
                    }],
                },
                packet_count: Arc::new(AtomicUsize::new(0)),
                shutdown_wait: Duration::ZERO,
                shutdown_called: Arc::new(AtomicBool::new(false)),
            })
            .build();
        let report = host.startup().await;
        assert_eq!(report.active_extensions, 1);
        assert_eq!(report.failed_extensions, 0);
        let _ = tcp_server_task.await;
        let _ = ws_server_task.await;
        host.shutdown().await;
    }
}
