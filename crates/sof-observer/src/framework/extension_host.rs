//! Runtime host for packet-oriented extensions and their resource adapters.

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
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures_util::{SinkExt, StreamExt};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream, UdpSocket},
    sync::mpsc,
    task::JoinHandle,
    time::timeout,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

use crate::framework::extension::{
    ExtensionCapability, ExtensionContext, ExtensionManifest, ExtensionResourceSpec,
    ExtensionStreamVisibility, PacketSubscription, RuntimeExtension, RuntimePacketEvent,
    RuntimePacketEventClass, RuntimePacketSource, RuntimePacketSourceKind, RuntimePacketTransport,
    RuntimeWebSocketFrameType, TcpConnectorSpec, TcpListenerSpec, UdpListenerSpec, WsConnectorSpec,
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
/// Maximum extension resource read buffer accepted from one startup manifest.
const MAX_RESOURCE_READ_BUFFER_BYTES: usize = 1024 * 1024;
/// Multiplier used to cap extension websocket frames/messages relative to chunk size.
const EXTENSION_WEBSOCKET_MESSAGE_LIMIT_MULTIPLIER: usize = 64;

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
    /// Returns an empty startup report sized to the current extension set.
    const fn empty(discovered_extensions: usize) -> Self {
        Self {
            discovered_extensions,
            active_extensions: 0,
            failed_extensions: 0,
            failures: Vec::new(),
        }
    }
}

/// Runtime dispatch telemetry snapshot for one active extension.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RuntimeExtensionDispatchMetrics {
    /// Extension identifier.
    pub extension: &'static str,
    /// Number of events dropped due to queue pressure or closed dispatcher.
    pub dropped_events: u64,
    /// Current in-memory dispatch queue depth.
    pub queue_depth: u64,
    /// Maximum observed queue depth since startup.
    pub max_queue_depth: u64,
    /// Number of events delivered to `on_packet_received`.
    pub dispatched_events: u64,
    /// Mean queue wait time before callback dispatch in microseconds.
    pub avg_dispatch_lag_us: u64,
    /// Maximum queue wait time before callback dispatch in microseconds.
    pub max_dispatch_lag_us: u64,
}

/// Capability allowlist used by runtime extension startup validation.
#[derive(Debug, Clone)]
pub struct RuntimeExtensionCapabilityPolicy {
    /// Capabilities currently allowed for extension startup.
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

    /// Returns a policy that denies all capabilities.
    #[must_use]
    pub fn deny_all() -> Self {
        Self {
            allowed: HashSet::new(),
        }
    }

    /// Returns a hardened baseline policy.
    ///
    /// This allows observer ingress and local bind capabilities while denying
    /// outbound connector capabilities unless explicitly enabled.
    #[must_use]
    pub fn production_defaults() -> Self {
        Self::deny_all()
            .with(ExtensionCapability::BindUdp)
            .with(ExtensionCapability::BindTcp)
            .with(ExtensionCapability::ObserveObserverIngress)
            .with(ExtensionCapability::ObserveSharedExtensionStream)
    }

    /// Returns true if this policy allows the provided capability.
    #[must_use]
    pub fn allows(&self, capability: ExtensionCapability) -> bool {
        self.allowed.contains(&capability)
    }

    /// Adds one capability to the allowlist.
    #[must_use]
    pub fn with(mut self, capability: ExtensionCapability) -> Self {
        self.allowed.insert(capability);
        self
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
    /// Extensions that will be registered on the host.
    extensions: Vec<Arc<dyn RuntimeExtension>>,
    /// Bounded queue depth used by each extension dispatcher.
    event_queue_capacity: usize,
    /// Deadline applied to extension startup hooks.
    startup_timeout: Duration,
    /// Deadline applied to extension shutdown hooks.
    shutdown_timeout: Duration,
    /// Capability policy enforced during startup validation.
    capability_policy: RuntimeExtensionCapabilityPolicy,
    /// Whether extensions must override the default type-name identifier.
    require_explicit_extension_names: bool,
}

impl Default for RuntimeExtensionHostBuilder {
    fn default() -> Self {
        Self {
            extensions: Vec::new(),
            event_queue_capacity: DEFAULT_EVENT_QUEUE_CAPACITY,
            startup_timeout: Duration::from_secs(DEFAULT_STARTUP_TIMEOUT_SECS),
            shutdown_timeout: Duration::from_secs(DEFAULT_SHUTDOWN_TIMEOUT_SECS),
            capability_policy: RuntimeExtensionCapabilityPolicy::default(),
            require_explicit_extension_names: false,
        }
    }
}

impl RuntimeExtensionHostBuilder {
    /// Creates an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a hardened builder profile for production environments.
    ///
    /// Enables a restrictive capability policy and requires extensions to
    /// override `RuntimeExtension::name` with a stable identifier.
    #[must_use]
    pub fn production_defaults() -> Self {
        Self {
            capability_policy: RuntimeExtensionCapabilityPolicy::production_defaults(),
            require_explicit_extension_names: true,
            ..Self::default()
        }
    }

    /// Sets bounded async event queue capacity for extension packet dispatch.
    #[must_use]
    pub fn with_event_queue_capacity(mut self, capacity: usize) -> Self {
        self.event_queue_capacity = capacity.max(1);
        self
    }

    /// Sets startup timeout for `RuntimeExtension::setup`.
    #[must_use]
    pub const fn with_startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    /// Sets shutdown timeout for `RuntimeExtension::shutdown`.
    #[must_use]
    pub const fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Sets the capability policy applied during extension startup validation.
    #[must_use]
    pub fn with_capability_policy(mut self, policy: RuntimeExtensionCapabilityPolicy) -> Self {
        self.capability_policy = policy;
        self
    }

    /// Enables or disables strict validation for explicit extension names.
    #[must_use]
    pub const fn with_require_explicit_extension_names(mut self, require: bool) -> Self {
        self.require_explicit_extension_names = require;
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
                require_explicit_extension_names: self.require_explicit_extension_names,
                runtime_state: RwLock::new(None),
            }),
        }
    }
}

/// Immutable host configuration plus the currently active runtime state.
struct RuntimeExtensionHostInner {
    /// Extensions registered on the host in registration order.
    extensions: Arc<[Arc<dyn RuntimeExtension>]>,
    /// Bounded queue depth used by each extension dispatcher.
    event_queue_capacity: usize,
    /// Deadline applied to extension startup hooks.
    startup_timeout: Duration,
    /// Deadline applied to extension shutdown hooks.
    shutdown_timeout: Duration,
    /// Capability policy enforced during startup validation.
    capability_policy: RuntimeExtensionCapabilityPolicy,
    /// Whether extensions must override the default type-name identifier.
    require_explicit_extension_names: bool,
    /// Active runtime state populated after successful startup.
    runtime_state: RwLock<Option<RuntimeExtensionRuntimeState>>,
}

/// Active extension set installed after startup succeeds.
struct RuntimeExtensionRuntimeState {
    /// Extensions that passed validation and completed startup.
    active_extensions: Arc<[Arc<ActiveRuntimeExtension>]>,
}

/// Runtime metadata and worker handles for one active extension.
struct ActiveRuntimeExtension {
    /// Shared extension implementation.
    extension: Arc<dyn RuntimeExtension>,
    /// Stable extension identifier.
    name: &'static str,
    /// Manifest capabilities granted to the extension.
    capabilities: HashSet<ExtensionCapability>,
    /// Packet subscriptions accepted by the extension.
    subscriptions: Arc<[PacketSubscription]>,
    /// Async dispatcher responsible for packet delivery.
    dispatcher: ExtensionDispatcher,
    /// Background resource tasks owned by this extension.
    resource_handles: Mutex<Vec<JoinHandle<()>>>,
}

impl ActiveRuntimeExtension {
    /// Returns the number of packet events dropped for this extension.
    fn dropped_event_count(&self) -> u64 {
        self.dispatcher.dropped_count()
    }

    /// Builds a dispatch telemetry snapshot for this extension.
    fn dispatch_metrics_snapshot(&self) -> RuntimeExtensionDispatchMetrics {
        self.dispatcher.metrics_snapshot(self.name)
    }

    /// Tracks one background resource task so it can be aborted on shutdown.
    fn push_resource_handle(&self, handle: JoinHandle<()>) {
        if let Ok(mut guard) = self.resource_handles.lock() {
            guard.push(handle);
        }
    }

    /// Aborts all background resource tasks owned by this extension.
    fn abort_resource_handles(&self) {
        if let Ok(mut guard) = self.resource_handles.lock() {
            for handle in guard.drain(..) {
                handle.abort();
            }
        }
    }
}

/// Bounded asynchronous dispatcher for one active runtime extension.
#[derive(Clone)]
struct ExtensionDispatcher {
    /// Bounded queue used for packet delivery.
    tx: mpsc::Sender<QueuedRuntimePacketEvent>,
    /// Total number of dropped packet events.
    dropped_events: Arc<AtomicU64>,
    /// Current queue depth.
    queue_depth: Arc<AtomicU64>,
    /// Maximum queue depth observed since startup.
    max_queue_depth: Arc<AtomicU64>,
    /// Number of packet callbacks completed.
    dispatched_events: Arc<AtomicU64>,
    /// Cumulative queue wait time across dispatched events.
    total_dispatch_lag_us: Arc<AtomicU64>,
    /// Maximum queue wait time observed since startup.
    max_dispatch_lag_us: Arc<AtomicU64>,
    /// Join handle for the background dispatch worker.
    worker: Arc<Mutex<Option<JoinHandle<()>>>>,
}

/// Packet event buffered for asynchronous extension dispatch.
struct QueuedRuntimePacketEvent {
    /// Event to deliver to the extension callback.
    event: RuntimePacketEvent,
    /// Instant when the event entered the queue.
    queued_at: Instant,
}

impl ExtensionDispatcher {
    /// Spawns one bounded dispatcher worker for an active extension.
    fn spawn(
        extension: &Arc<dyn RuntimeExtension>,
        extension_name: &'static str,
        queue_capacity: usize,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<QueuedRuntimePacketEvent>(queue_capacity.max(1));
        let dropped_events = Arc::new(AtomicU64::new(0));
        let queue_depth = Arc::new(AtomicU64::new(0));
        let max_queue_depth = Arc::new(AtomicU64::new(0));
        let dispatched_events = Arc::new(AtomicU64::new(0));
        let total_dispatch_lag_us = Arc::new(AtomicU64::new(0));
        let max_dispatch_lag_us = Arc::new(AtomicU64::new(0));
        let worker_extension = Arc::clone(extension);
        let worker_queue_depth = Arc::clone(&queue_depth);
        let worker_dispatched_events = Arc::clone(&dispatched_events);
        let worker_total_dispatch_lag_us = Arc::clone(&total_dispatch_lag_us);
        let worker_max_dispatch_lag_us = Arc::clone(&max_dispatch_lag_us);
        let worker = tokio::spawn(async move {
            while let Some(queued_event) = rx.recv().await {
                worker_queue_depth.fetch_sub(1, Ordering::Relaxed);
                let queue_lag_us =
                    u64::try_from(queued_event.queued_at.elapsed().as_micros()).unwrap_or(u64::MAX);
                worker_dispatched_events.fetch_add(1, Ordering::Relaxed);
                worker_total_dispatch_lag_us.fetch_add(queue_lag_us, Ordering::Relaxed);
                record_max_atomic(&worker_max_dispatch_lag_us, queue_lag_us);

                let callback_extension = Arc::clone(&worker_extension);
                let callback_result = tokio::spawn(async move {
                    callback_extension
                        .on_packet_received(queued_event.event)
                        .await;
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
            queue_depth,
            max_queue_depth,
            dispatched_events,
            total_dispatch_lag_us,
            max_dispatch_lag_us,
            worker: Arc::new(Mutex::new(Some(worker))),
        }
    }

    /// Enqueues one runtime packet event for asynchronous delivery.
    fn dispatch(&self, extension_name: &'static str, event: RuntimePacketEvent) {
        let queued_event = QueuedRuntimePacketEvent {
            event,
            queued_at: Instant::now(),
        };
        let queue_depth = self
            .queue_depth
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        record_max_atomic(&self.max_queue_depth, queue_depth);
        match self.tx.try_send(queued_event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.queue_depth.fetch_sub(1, Ordering::Relaxed);
                self.record_drop(extension_name, "queue full");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.queue_depth.fetch_sub(1, Ordering::Relaxed);
                self.record_drop(extension_name, "queue closed");
            }
        }
    }

    /// Returns the number of packet events dropped by this dispatcher.
    fn dropped_count(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    /// Builds a dispatch telemetry snapshot for this dispatcher.
    fn metrics_snapshot(&self, extension_name: &'static str) -> RuntimeExtensionDispatchMetrics {
        let dispatched_events = self.dispatched_events.load(Ordering::Relaxed);
        let total_dispatch_lag_us = self.total_dispatch_lag_us.load(Ordering::Relaxed);
        let avg_dispatch_lag_us = if dispatched_events == 0 {
            0
        } else {
            total_dispatch_lag_us
                .checked_div(dispatched_events)
                .unwrap_or_default()
        };
        RuntimeExtensionDispatchMetrics {
            extension: extension_name,
            dropped_events: self.dropped_events.load(Ordering::Relaxed),
            queue_depth: self.queue_depth.load(Ordering::Relaxed),
            max_queue_depth: self.max_queue_depth.load(Ordering::Relaxed),
            dispatched_events,
            avg_dispatch_lag_us,
            max_dispatch_lag_us: self.max_dispatch_lag_us.load(Ordering::Relaxed),
        }
    }

    /// Aborts the background dispatch worker if it is still running.
    fn abort_worker(&self) {
        if let Ok(mut guard) = self.worker.lock()
            && let Some(handle) = guard.take()
        {
            handle.abort();
        }
    }

    /// Records one dropped packet event and emits sampled warnings.
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
                queue_depth = self.queue_depth.load(Ordering::Relaxed),
                "dropping runtime extension packet event to protect ingest hot path"
            );
        }
    }
}

/// Separate runtime extension host from observer plugin host.
#[derive(Clone)]
pub struct RuntimeExtensionHost {
    /// Shared host configuration and runtime state.
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

    /// Starts a hardened host builder profile for production environments.
    #[must_use]
    pub fn production_builder() -> RuntimeExtensionHostBuilder {
        RuntimeExtensionHostBuilder::production_defaults()
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

    /// Returns dispatch telemetry snapshots per active runtime extension.
    #[must_use]
    pub fn dispatch_metrics_by_extension(&self) -> Vec<RuntimeExtensionDispatchMetrics> {
        self.inner
            .runtime_state
            .read()
            .ok()
            .and_then(|guard| {
                guard.as_ref().map(|state| {
                    state
                        .active_extensions
                        .iter()
                        .map(|extension| extension.dispatch_metrics_snapshot())
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
        let mut seen_extension_names = HashSet::<&'static str>::new();

        for extension in self.inner.extensions.iter() {
            let extension = Arc::clone(extension);
            let extension_name = extension.name();
            let has_explicit_name = extension.has_explicit_name();

            if !has_explicit_name {
                let concrete_type_name = std::any::type_name_of_val(extension.as_ref());
                tracing::warn!(
                    extension = extension_name,
                    concrete_type = concrete_type_name,
                    "runtime extension uses implicit type-name identifier; override `name()` with a stable literal for telemetry/filter durability"
                );
                if self.inner.require_explicit_extension_names {
                    report.failures.push(RuntimeExtensionStartupFailure {
                        extension: extension_name,
                        reason:
                            "runtime policy requires explicit stable extension names; override RuntimeExtension::name"
                                .to_owned(),
                    });
                    continue;
                }
            }

            if !seen_extension_names.insert(extension_name) {
                report.failures.push(RuntimeExtensionStartupFailure {
                    extension: extension_name,
                    reason: format!(
                        "duplicate extension name `{extension_name}`; extension names must be unique"
                    ),
                });
                continue;
            }

            let startup_context = ExtensionContext { extension_name };
            let manifest_result =
                timeout(self.inner.startup_timeout, extension.setup(startup_context)).await;
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
                    &extension,
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
        self.on_observer_packet_shared(source, Arc::from(bytes));
    }

    /// Emits one observer ingress packet with shared payload ownership.
    ///
    /// Use this when ingress already owns packet bytes behind `Arc<[u8]>` to
    /// avoid an additional payload allocation.
    pub fn on_observer_packet_shared(&self, source: SocketAddr, bytes: Arc<[u8]>) {
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
            bytes,
            observed_unix_ms: current_unix_ms(),
        };
        self.dispatch_runtime_packet(&event);
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
        self.dispatch_runtime_packet(&event);
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
            let shutdown_context = ExtensionContext {
                extension_name: extension.name,
            };
            let shutdown_result = timeout(
                self.inner.shutdown_timeout,
                extension.extension.shutdown(shutdown_context),
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

    /// Dispatches one packet event to all eligible active extensions.
    fn dispatch_runtime_packet(&self, event: &RuntimePacketEvent) {
        let Some(active_extensions) = self.inner.runtime_state.read().ok().and_then(|guard| {
            guard
                .as_ref()
                .map(|state| Arc::clone(&state.active_extensions))
        }) else {
            return;
        };
        for extension in active_extensions.iter() {
            if !extension_can_observe_event(extension, event) {
                continue;
            }
            if !extension
                .subscriptions
                .iter()
                .any(|subscription| subscription.matches(event))
            {
                continue;
            }
            extension.dispatcher.dispatch(extension.name, event.clone());
        }
    }

    /// Starts external resources declared by one extension manifest.
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

/// Returns whether an extension is allowed to observe the provided packet event.
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

/// Immutable metadata for events emitted from one extension-owned resource.
#[derive(Clone)]
struct ExtensionResourceEmitter {
    /// Host receiving translated resource events.
    host: RuntimeExtensionHost,
    /// Owning extension identifier.
    owner_extension: String,
    /// Stable resource identifier from the extension manifest.
    resource_id: String,
    /// Shared visibility tag, if the stream is shared.
    shared_tag: Option<String>,
    /// Transport used by the resource.
    transport: RuntimePacketTransport,
    /// Resource local socket address, if available.
    local_addr: Option<SocketAddr>,
    /// Resource remote socket address, if available.
    remote_addr: Option<SocketAddr>,
}

impl ExtensionResourceEmitter {
    /// Builds a new emitter for one extension-owned resource stream.
    fn new(
        host: RuntimeExtensionHost,
        extension_name: &str,
        resource_id: &str,
        shared_tag: Option<String>,
        transport: RuntimePacketTransport,
        local_addr: Option<SocketAddr>,
        remote_addr: Option<SocketAddr>,
    ) -> Self {
        Self {
            host,
            owner_extension: extension_name.to_owned(),
            resource_id: resource_id.to_owned(),
            shared_tag,
            transport,
            local_addr,
            remote_addr,
        }
    }

    /// Emits one packet payload, chunking it to the configured max size.
    fn emit_payload(
        &self,
        payload: &[u8],
        websocket_frame_type: Option<RuntimeWebSocketFrameType>,
        max_payload_chunk_bytes: usize,
    ) {
        let chunk_size = max_payload_chunk_bytes.max(1);
        for chunk in payload.chunks(chunk_size) {
            self.emit_event(
                RuntimePacketEventClass::Packet,
                websocket_frame_type,
                Arc::from(chunk),
            );
        }
    }

    /// Emits one runtime packet event from this resource.
    fn emit_event(
        &self,
        event_class: RuntimePacketEventClass,
        websocket_frame_type: Option<RuntimeWebSocketFrameType>,
        bytes: Arc<[u8]>,
    ) {
        let source = RuntimePacketSource {
            kind: RuntimePacketSourceKind::ExtensionResource,
            transport: self.transport,
            event_class,
            owner_extension: Some(self.owner_extension.clone()),
            resource_id: Some(self.resource_id.clone()),
            shared_tag: self.shared_tag.clone(),
            websocket_frame_type,
            local_addr: self.local_addr,
            remote_addr: self.remote_addr,
        };
        self.host.emit_extension_packet(source, bytes);
    }
}

/// Runtime parameters for one streaming resource reader.
struct ExtensionResourceReadContext {
    /// Emitter translating raw I/O into runtime packet events.
    emitter: ExtensionResourceEmitter,
    /// Maximum chunk size used for emitted payload slices.
    max_payload_chunk_bytes: usize,
}

impl ExtensionResourceReadContext {
    /// Creates a new resource read context for payload-oriented streams.
    const fn new(emitter: ExtensionResourceEmitter, max_payload_chunk_bytes: usize) -> Self {
        Self {
            emitter,
            max_payload_chunk_bytes,
        }
    }
}

/// Spawns one UDP listener declared by an extension resource manifest.
async fn spawn_udp_listener(
    host: RuntimeExtensionHost,
    extension: &Arc<ActiveRuntimeExtension>,
    spec: UdpListenerSpec,
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
    let emitter = ExtensionResourceEmitter::new(
        host,
        &owner_extension,
        &resource_id,
        shared_tag,
        RuntimePacketTransport::Udp,
        local_addr,
        None,
    );
    let handle = tokio::spawn(async move {
        let mut buffer = vec![0_u8; read_buffer_bytes];
        loop {
            match socket.recv_from(&mut buffer).await {
                Ok((len, remote_addr)) => {
                    if len == 0 {
                        continue;
                    }
                    if let Some(payload) = buffer.get(..len) {
                        ExtensionResourceEmitter {
                            remote_addr: Some(remote_addr),
                            ..emitter.clone()
                        }
                        .emit_event(
                            RuntimePacketEventClass::Packet,
                            None,
                            Arc::from(payload),
                        );
                    }
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

/// Spawns one TCP listener declared by an extension resource manifest.
async fn spawn_tcp_listener(
    host: RuntimeExtensionHost,
    extension: &Arc<ActiveRuntimeExtension>,
    spec: TcpListenerSpec,
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
                    let emitter = ExtensionResourceEmitter::new(
                        host.clone(),
                        &owner_extension,
                        &resource_id,
                        shared_tag.clone(),
                        RuntimePacketTransport::Tcp,
                        local_addr,
                        Some(remote_addr),
                    );
                    read_tcp_stream_packets(
                        ExtensionResourceReadContext::new(emitter, read_buffer_bytes),
                        stream,
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

/// Spawns one TCP connector declared by an extension resource manifest.
async fn spawn_tcp_connector(
    host: RuntimeExtensionHost,
    extension: &Arc<ActiveRuntimeExtension>,
    spec: TcpConnectorSpec,
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
        let emitter = ExtensionResourceEmitter::new(
            host,
            &owner_extension,
            &resource_id,
            shared_tag,
            RuntimePacketTransport::Tcp,
            local_addr,
            remote_addr,
        );
        read_tcp_stream_packets(
            ExtensionResourceReadContext::new(emitter, read_buffer_bytes),
            stream,
        )
        .await;
    });
    Ok(handle)
}

/// Spawns one WebSocket connector declared by an extension resource manifest.
async fn spawn_ws_connector(
    host: RuntimeExtensionHost,
    extension: &Arc<ActiveRuntimeExtension>,
    spec: WsConnectorSpec,
) -> Result<JoinHandle<()>, String> {
    let max_payload_chunk_bytes = spec
        .read_buffer_bytes
        .max(DEFAULT_RESOURCE_READ_BUFFER_BYTES);
    let (stream, _response) = connect_async_with_config(
        spec.url.as_str(),
        Some(extension_websocket_transport_config(
            max_payload_chunk_bytes,
        )),
        false,
    )
    .await
    .map_err(|error| format!("failed to connect websocket {}: {error}", spec.url))?;
    let io = stream.get_ref().get_ref();
    let local_addr = io.local_addr().ok();
    let peer_addr = io.peer_addr().ok();
    let owner_extension = extension.name.to_owned();
    let resource_id = spec.resource_id;
    let shared_tag = visibility_tag(spec.visibility);
    let handle = tokio::spawn(async move {
        let emitter = ExtensionResourceEmitter::new(
            host,
            &owner_extension,
            &resource_id,
            shared_tag,
            RuntimePacketTransport::WebSocket,
            local_addr,
            peer_addr,
        );
        read_websocket_messages(
            ExtensionResourceReadContext::new(emitter, max_payload_chunk_bytes),
            stream,
        )
        .await;
    });
    Ok(handle)
}

/// Builds bounded websocket transport config for extension-owned connectors.
fn extension_websocket_transport_config(max_payload_chunk_bytes: usize) -> WebSocketConfig {
    let max_message_size = max_payload_chunk_bytes
        .max(DEFAULT_RESOURCE_READ_BUFFER_BYTES)
        .saturating_mul(EXTENSION_WEBSOCKET_MESSAGE_LIMIT_MULTIPLIER);
    WebSocketConfig::default()
        .max_message_size(Some(max_message_size))
        .max_frame_size(Some(max_message_size))
}

/// Reads packet chunks from one TCP stream and forwards them into the runtime.
async fn read_tcp_stream_packets(context: ExtensionResourceReadContext, mut stream: TcpStream) {
    let mut buffer = vec![0_u8; context.max_payload_chunk_bytes.max(1)];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) => break,
            Ok(len) => {
                if let Some(payload) = buffer.get(..len) {
                    context.emitter.emit_event(
                        RuntimePacketEventClass::Packet,
                        None,
                        Arc::from(payload),
                    );
                }
            }
            Err(error) => {
                if error.kind() != ErrorKind::Interrupted {
                    tracing::warn!(
                        extension = context.emitter.owner_extension.as_str(),
                        resource_id = context.emitter.resource_id.as_str(),
                        error = %error,
                        "extension tcp stream read loop terminated"
                    );
                }
                break;
            }
        }
    }
}

/// Reads decoded WebSocket messages and forwards them into the runtime.
async fn read_websocket_messages(
    context: ExtensionResourceReadContext,
    mut stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
) {
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => {
                context.emitter.emit_payload(
                    text.as_str().as_bytes(),
                    Some(RuntimeWebSocketFrameType::Text),
                    context.max_payload_chunk_bytes,
                );
            }
            Some(Ok(Message::Binary(bytes))) => {
                context.emitter.emit_payload(
                    bytes.as_ref(),
                    Some(RuntimeWebSocketFrameType::Binary),
                    context.max_payload_chunk_bytes,
                );
            }
            Some(Ok(Message::Ping(payload))) => {
                context.emitter.emit_payload(
                    payload.as_ref(),
                    Some(RuntimeWebSocketFrameType::Ping),
                    context.max_payload_chunk_bytes,
                );
                if let Err(error) = stream.send(Message::Pong(payload)).await {
                    tracing::warn!(
                        extension = context.emitter.owner_extension.as_str(),
                        resource_id = context.emitter.resource_id.as_str(),
                        error = %error,
                        "failed to send websocket pong frame; stopping connector"
                    );
                    break;
                }
            }
            Some(Ok(Message::Pong(payload))) => {
                context.emitter.emit_payload(
                    payload.as_ref(),
                    Some(RuntimeWebSocketFrameType::Pong),
                    context.max_payload_chunk_bytes,
                );
            }
            Some(Ok(Message::Close(frame))) => {
                let close_payload = frame
                    .as_ref()
                    .map(|frame| frame.reason.as_bytes())
                    .unwrap_or_default();
                context.emitter.emit_event(
                    RuntimePacketEventClass::ConnectionClosed,
                    None,
                    Arc::from(close_payload),
                );
                tracing::info!(
                    extension = context.emitter.owner_extension.as_str(),
                    resource_id = context.emitter.resource_id.as_str(),
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
                    extension = context.emitter.owner_extension.as_str(),
                    resource_id = context.emitter.resource_id.as_str(),
                    error = %error,
                    "websocket connector read loop terminated"
                );
                break;
            }
            None => break,
        }
    }
}

/// Converts extension stream visibility into the optional shared stream tag.
fn visibility_tag(visibility: ExtensionStreamVisibility) -> Option<String> {
    match visibility {
        ExtensionStreamVisibility::Private => None,
        ExtensionStreamVisibility::Shared { tag } => Some(tag),
    }
}

/// Manifest data retained after policy validation.
struct ValidatedManifest {
    /// Capabilities granted to the extension.
    capabilities: HashSet<ExtensionCapability>,
    /// Packet subscriptions accepted by the extension.
    subscriptions: Vec<PacketSubscription>,
}

/// Validates one extension manifest against the active runtime policy.
fn validate_manifest(
    extension_name: &'static str,
    manifest: &ExtensionManifest,
    policy: &RuntimeExtensionCapabilityPolicy,
) -> Result<ValidatedManifest, String> {
    if extension_name.trim().is_empty() {
        return Err("extension declares empty name".to_owned());
    }
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
        let (resource_id, visibility, read_buffer_bytes, required_capability) = match resource {
            ExtensionResourceSpec::UdpListener(spec) => (
                &spec.resource_id,
                &spec.visibility,
                spec.read_buffer_bytes,
                ExtensionCapability::BindUdp,
            ),
            ExtensionResourceSpec::TcpListener(spec) => (
                &spec.resource_id,
                &spec.visibility,
                spec.read_buffer_bytes,
                ExtensionCapability::BindTcp,
            ),
            ExtensionResourceSpec::TcpConnector(spec) => (
                &spec.resource_id,
                &spec.visibility,
                spec.read_buffer_bytes,
                ExtensionCapability::ConnectTcp,
            ),
            ExtensionResourceSpec::WsConnector(spec) => (
                &spec.resource_id,
                &spec.visibility,
                spec.read_buffer_bytes,
                ExtensionCapability::ConnectWebSocket,
            ),
        };
        if resource_id.trim().is_empty() {
            return Err(format!(
                "extension `{extension_name}` declares empty resource_id"
            ));
        }
        if !resource_ids.insert(resource_id.clone()) {
            return Err(format!(
                "duplicate resource_id `{resource_id}` in startup manifest for extension `{extension_name}`"
            ));
        }
        if read_buffer_bytes > MAX_RESOURCE_READ_BUFFER_BYTES {
            return Err(format!(
                "resource `{resource_id}` read_buffer_bytes {read_buffer_bytes} exceeds max {}",
                MAX_RESOURCE_READ_BUFFER_BYTES
            ));
        }
        if matches!(
            visibility,
            ExtensionStreamVisibility::Shared { tag } if tag.trim().is_empty()
        ) {
            return Err(format!(
                "resource `{resource_id}` declares empty shared visibility tag"
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

/// Atomically records a new maximum value when it exceeds the observed maximum.
fn record_max_atomic(target: &AtomicU64, value: u64) {
    let mut observed = target.load(Ordering::Relaxed);
    while value > observed {
        match target.compare_exchange_weak(observed, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(actual) => observed = actual,
        }
    }
}

/// Returns the current Unix timestamp in milliseconds.
fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

/// Converts a panic payload into a loggable message string.
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

    use crate::framework::ExtensionSetupError;
    use async_trait::async_trait;
    use tokio::io::AsyncWriteExt;

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

        async fn setup(
            &self,
            _ctx: ExtensionContext,
        ) -> Result<ExtensionManifest, ExtensionSetupError> {
            Ok(self.startup_manifest.clone())
        }

        async fn on_packet_received(&self, _event: RuntimePacketEvent) {
            self.packet_count.fetch_add(1, Ordering::Relaxed);
        }

        async fn shutdown(&self, _ctx: ExtensionContext) {
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

        async fn setup(
            &self,
            _ctx: ExtensionContext,
        ) -> Result<ExtensionManifest, ExtensionSetupError> {
            Err(ExtensionSetupError::new("intentional startup fail"))
        }
    }

    struct ImplicitNameExtension;

    #[async_trait]
    impl RuntimeExtension for ImplicitNameExtension {
        async fn setup(
            &self,
            _ctx: ExtensionContext,
        ) -> Result<ExtensionManifest, ExtensionSetupError> {
            Ok(ExtensionManifest {
                capabilities: vec![ExtensionCapability::ObserveObserverIngress],
                resources: Vec::new(),
                subscriptions: vec![PacketSubscription {
                    source_kind: Some(RuntimePacketSourceKind::ObserverIngress),
                    ..PacketSubscription::default()
                }],
            })
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
                    resources: vec![ExtensionResourceSpec::UdpListener(UdpListenerSpec {
                        resource_id: "udp-1".to_owned(),
                        bind_addr: SocketAddr::from_str("127.0.0.1:0").expect("valid addr"),
                        visibility: ExtensionStreamVisibility::Private,
                        read_buffer_bytes: 128,
                    })],
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
    async fn startup_rejects_empty_resource_id() {
        let host = RuntimeExtensionHost::builder()
            .add_extension(CounterExtension {
                name: "empty-resource-id",
                startup_manifest: ExtensionManifest {
                    capabilities: vec![ExtensionCapability::BindUdp],
                    resources: vec![ExtensionResourceSpec::UdpListener(UdpListenerSpec {
                        resource_id: "   ".to_owned(),
                        bind_addr: SocketAddr::from_str("127.0.0.1:0").expect("valid addr"),
                        visibility: ExtensionStreamVisibility::Private,
                        read_buffer_bytes: 128,
                    })],
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
        assert!(report.failures[0].reason.contains("empty resource_id"));
    }

    #[tokio::test]
    async fn startup_rejects_empty_extension_name() {
        let host = RuntimeExtensionHost::builder()
            .add_extension(CounterExtension {
                name: "   ",
                startup_manifest: ExtensionManifest {
                    capabilities: vec![ExtensionCapability::BindUdp],
                    resources: vec![ExtensionResourceSpec::UdpListener(UdpListenerSpec {
                        resource_id: "udp-feed".to_owned(),
                        bind_addr: SocketAddr::from_str("127.0.0.1:0").expect("valid addr"),
                        visibility: ExtensionStreamVisibility::Private,
                        read_buffer_bytes: 128,
                    })],
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
        assert!(report.failures[0].reason.contains("empty name"));
    }

    #[tokio::test]
    async fn startup_rejects_empty_shared_visibility_tag() {
        let host = RuntimeExtensionHost::builder()
            .add_extension(CounterExtension {
                name: "empty-shared-tag",
                startup_manifest: ExtensionManifest {
                    capabilities: vec![ExtensionCapability::BindTcp],
                    resources: vec![ExtensionResourceSpec::TcpListener(TcpListenerSpec {
                        resource_id: "tcp-feed".to_owned(),
                        bind_addr: SocketAddr::from_str("127.0.0.1:0").expect("valid addr"),
                        visibility: ExtensionStreamVisibility::Shared {
                            tag: "  ".to_owned(),
                        },
                        read_buffer_bytes: 128,
                    })],
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
        assert!(
            report.failures[0]
                .reason
                .contains("empty shared visibility tag")
        );
    }

    #[tokio::test]
    async fn startup_rejects_oversized_read_buffer_bytes() {
        let host = RuntimeExtensionHost::builder()
            .add_extension(CounterExtension {
                name: "oversized-read-buffer",
                startup_manifest: ExtensionManifest {
                    capabilities: vec![ExtensionCapability::ConnectWebSocket],
                    resources: vec![ExtensionResourceSpec::WsConnector(WsConnectorSpec {
                        resource_id: "ws-feed".to_owned(),
                        url: "ws://127.0.0.1:1/feed".to_owned(),
                        visibility: ExtensionStreamVisibility::Private,
                        read_buffer_bytes: MAX_RESOURCE_READ_BUFFER_BYTES.saturating_add(1),
                    })],
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
        assert!(report.failures[0].reason.contains("read_buffer_bytes"));
    }

    #[tokio::test]
    async fn production_defaults_deny_outbound_connectors() {
        let host = RuntimeExtensionHost::production_builder()
            .add_extension(CounterExtension {
                name: "connect-tcp-extension",
                startup_manifest: ExtensionManifest {
                    capabilities: vec![ExtensionCapability::ConnectTcp],
                    resources: vec![ExtensionResourceSpec::TcpConnector(TcpConnectorSpec {
                        resource_id: "tcp-outbound".to_owned(),
                        remote_addr: SocketAddr::from_str("127.0.0.1:9").expect("valid addr"),
                        visibility: ExtensionStreamVisibility::Private,
                        read_buffer_bytes: 128,
                    })],
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
        assert!(report.failures[0].reason.contains("not allowed"));
    }

    #[tokio::test]
    async fn strict_name_policy_rejects_implicit_type_name_extensions() {
        let host = RuntimeExtensionHost::builder()
            .with_require_explicit_extension_names(true)
            .add_extension(ImplicitNameExtension)
            .build();

        let report = host.startup().await;
        assert_eq!(report.active_extensions, 0);
        assert_eq!(report.failed_extensions, 1);
        assert!(
            report.failures[0]
                .reason
                .contains("requires explicit stable extension names")
        );
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

        async fn setup(
            &self,
            _ctx: ExtensionContext,
        ) -> Result<ExtensionManifest, ExtensionSetupError> {
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

        let metrics = host.dispatch_metrics_by_extension();
        assert_eq!(metrics.len(), 1);
        assert!(metrics[0].dropped_events > 0);
        assert!(metrics[0].max_queue_depth >= 1);
        assert_eq!(metrics[0].queue_depth, 0);
        assert!(metrics[0].dispatched_events >= 1);
    }

    #[tokio::test]
    async fn shutdown_deadline_then_cancel() {
        let shutdown_called = Arc::new(AtomicBool::new(false));
        let shutdown_timeout = Duration::from_millis(25);
        let shutdown_wait = Duration::from_secs(5);
        let host = RuntimeExtensionHost::builder()
            .with_shutdown_timeout(shutdown_timeout)
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
                shutdown_wait,
                shutdown_called: Arc::clone(&shutdown_called),
            })
            .build();
        let report = host.startup().await;
        assert_eq!(report.active_extensions, 1);

        let started = tokio::time::Instant::now();
        host.shutdown().await;
        let elapsed = started.elapsed();
        assert!(elapsed >= shutdown_timeout);
        assert!(elapsed < Duration::from_secs(1));
        assert!(elapsed < shutdown_wait);
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
                assert!(stream.write_all(b"tcp").await.is_ok());
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
                assert!(websocket.send(Message::Text("ws".into())).await.is_ok());
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
                        ExtensionResourceSpec::UdpListener(UdpListenerSpec {
                            resource_id: "udp-listener".to_owned(),
                            bind_addr: SocketAddr::from_str("127.0.0.1:0").expect("addr"),
                            visibility: ExtensionStreamVisibility::Private,
                            read_buffer_bytes: 128,
                        }),
                        ExtensionResourceSpec::TcpListener(TcpListenerSpec {
                            resource_id: "tcp-listener".to_owned(),
                            bind_addr: SocketAddr::from_str("127.0.0.1:0").expect("addr"),
                            visibility: ExtensionStreamVisibility::Private,
                            read_buffer_bytes: 128,
                        }),
                        ExtensionResourceSpec::TcpConnector(TcpConnectorSpec {
                            resource_id: "tcp-connector".to_owned(),
                            remote_addr: tcp_server_addr,
                            visibility: ExtensionStreamVisibility::Private,
                            read_buffer_bytes: 128,
                        }),
                        ExtensionResourceSpec::WsConnector(WsConnectorSpec {
                            resource_id: "ws-connector".to_owned(),
                            url: format!("ws://{ws_server_addr}/feed"),
                            visibility: ExtensionStreamVisibility::Private,
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
        assert!(tcp_server_task.await.is_ok());
        assert!(ws_server_task.await.is_ok());
        host.shutdown().await;
    }

    #[test]
    fn extension_websocket_transport_config_caps_frames_from_chunk_size() {
        let config = extension_websocket_transport_config(4_096);
        let expected = 4_096_usize.saturating_mul(EXTENSION_WEBSOCKET_MESSAGE_LIMIT_MULTIPLIER);

        assert_eq!(config.max_frame_size, Some(expected));
        assert_eq!(config.max_message_size, Some(expected));
    }
}
