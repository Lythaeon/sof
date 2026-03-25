use std::io::{ErrorKind, IoSliceMut};
use std::net::SocketAddr;
#[cfg(target_os = "linux")]
use std::os::fd::{AsFd, AsRawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_queue::ArrayQueue;

// Linux-only ancillary control messages are used for RX queue overflow telemetry.
// Runtime ingest remains cross-platform; non-Linux builds use the standard recv_from path.
#[cfg(target_os = "linux")]
use nix::poll::{PollFd, PollFlags};
#[cfg(target_os = "linux")]
use nix::sys::socket::{ControlMessageOwned, MsgFlags, SockaddrStorage, recvmsg};
use socket2::SockRef;
use thiserror::Error;
use tokio::task::{self, JoinHandle};

use crate::ingest::RawPacketBatchSender;
use crate::ingest::config::{
    enable_rxq_ovfl_tracking, read_udp_batch_max_wait_ms, read_udp_batch_size,
    read_udp_idle_wait_ms, read_udp_rcvbuf_bytes, read_udp_receiver_core,
};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RawPacketIngress {
    Udp,
}

pub const UDP_PACKET_BUFFER_BYTES: usize = 2048;
const RAW_PACKET_BATCH_RECYCLER_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy)]
pub struct RawPacket {
    pub source: SocketAddr,
    pub ingress: RawPacketIngress,
    pub buffer_index: usize,
    pub len: usize,
}

#[derive(Debug)]
pub struct RawPacketBatch {
    storage: Option<RawPacketBatchStorage>,
    recycler: Option<Arc<RawPacketBatchRecycler>>,
}

#[derive(Debug, Default)]
struct RawPacketBatchStorage {
    buffers: Vec<[u8; UDP_PACKET_BUFFER_BYTES]>,
    packets: Vec<RawPacket>,
}

impl RawPacketBatchStorage {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            buffers: vec![[0_u8; UDP_PACKET_BUFFER_BYTES]; capacity],
            packets: Vec::with_capacity(capacity),
        }
    }

    fn clear(&mut self) {
        self.packets.clear();
    }
}

#[derive(Debug)]
pub(super) struct RawPacketBatchRecycler {
    packet_capacity: usize,
    queue: ArrayQueue<RawPacketBatchStorage>,
}

impl RawPacketBatchRecycler {
    fn new(packet_capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            packet_capacity: packet_capacity.max(1),
            queue: ArrayQueue::new(RAW_PACKET_BATCH_RECYCLER_CAPACITY),
        })
    }

    fn allocate(self: &Arc<Self>) -> RawPacketBatch {
        let storage = self
            .queue
            .pop()
            .unwrap_or_else(|| RawPacketBatchStorage::with_capacity(self.packet_capacity));
        RawPacketBatch {
            storage: Some(storage),
            recycler: Some(Arc::clone(self)),
        }
    }

    fn recycle(&self, mut storage: RawPacketBatchStorage) {
        storage.clear();
        let _ = self.queue.push(storage);
    }
}

impl RawPacketBatch {
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            storage: Some(RawPacketBatchStorage::with_capacity(capacity)),
            recycler: None,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.storage
            .as_ref()
            .map_or(0, |storage| storage.packets.len())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&mut self) {
        if let Some(storage) = self.storage.as_mut() {
            storage.clear();
        }
    }

    pub fn reserve(&mut self, additional: usize) {
        let Some(storage) = self.storage.as_mut() else {
            return;
        };
        let needed = storage.packets.len().saturating_add(additional);
        if needed > storage.buffers.len() {
            storage
                .buffers
                .resize(needed, [0_u8; UDP_PACKET_BUFFER_BYTES]);
        }
        storage.packets.reserve(additional);
    }

    pub(super) fn ensure_receive_slots(&mut self, additional: usize) -> usize {
        let Some(storage) = self.storage.as_mut() else {
            return 0;
        };
        let start_index = storage.packets.len();
        let needed = start_index.saturating_add(additional);
        if needed > storage.buffers.len() {
            storage
                .buffers
                .resize(needed, [0_u8; UDP_PACKET_BUFFER_BYTES]);
        }
        start_index
    }

    pub(super) fn receive_buffer_mut(
        &mut self,
        buffer_index: usize,
    ) -> Option<&mut [u8; UDP_PACKET_BUFFER_BYTES]> {
        self.storage.as_mut()?.buffers.get_mut(buffer_index)
    }

    pub(super) fn push_received_metadata(
        &mut self,
        source: SocketAddr,
        ingress: RawPacketIngress,
        buffer_index: usize,
        len: usize,
    ) -> Result<(), UdpReceiverError> {
        let Some(storage) = self.storage.as_mut() else {
            return Err(UdpReceiverError::Receive {
                source: std::io::Error::other("raw packet batch storage missing"),
            });
        };
        if len > UDP_PACKET_BUFFER_BYTES {
            return Err(UdpReceiverError::InvalidPacketLength {
                len,
                capacity: UDP_PACKET_BUFFER_BYTES,
            });
        }
        storage.packets.push(RawPacket {
            source,
            ingress,
            buffer_index,
            len,
        });
        Ok(())
    }

    pub fn push_packet_bytes(
        &mut self,
        source: SocketAddr,
        ingress: RawPacketIngress,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        self.push_packet(source, ingress, bytes)
            .map_err(|error| match error {
                UdpReceiverError::InvalidPacketLength { len, capacity } => std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!("udp packet length {len} exceeds buffer capacity {capacity}"),
                ),
                UdpReceiverError::Receive { source } => source,
                UdpReceiverError::BindSocket { source, .. }
                | UdpReceiverError::SetBlockingMode { source }
                | UdpReceiverError::SetReadTimeout { source } => source,
            })
    }

    pub(super) fn push_packet(
        &mut self,
        source: SocketAddr,
        ingress: RawPacketIngress,
        bytes: &[u8],
    ) -> Result<(), UdpReceiverError> {
        if self.storage.is_none() {
            return Err(UdpReceiverError::Receive {
                source: std::io::Error::other("raw packet batch storage missing"),
            });
        }
        if bytes.len() > UDP_PACKET_BUFFER_BYTES {
            return Err(UdpReceiverError::InvalidPacketLength {
                len: bytes.len(),
                capacity: UDP_PACKET_BUFFER_BYTES,
            });
        }
        let buffer_index = self.ensure_receive_slots(1);
        let buffer =
            self.receive_buffer_mut(buffer_index)
                .ok_or_else(|| UdpReceiverError::Receive {
                    source: std::io::Error::other("raw packet receive buffer missing"),
                })?;
        buffer[..bytes.len()].copy_from_slice(bytes);
        self.push_received_metadata(source, ingress, buffer_index, bytes.len())
    }

    #[must_use]
    pub fn packets(&self) -> &[RawPacket] {
        self.storage
            .as_ref()
            .map_or(&[], |storage| storage.packets.as_slice())
    }

    #[must_use]
    pub fn packet_bytes(&self, packet: RawPacket) -> &[u8] {
        let storage = self
            .storage
            .as_ref()
            .expect("raw packet batch storage available");
        &storage.buffers[packet.buffer_index][..packet.len]
    }

    fn take_for_send(&mut self) -> Self {
        if let Some(recycler) = self.recycler.as_ref() {
            let replacement = recycler.allocate();
            return std::mem::replace(self, replacement);
        }
        let packet_capacity = self
            .storage
            .as_ref()
            .map_or(1, |storage| storage.buffers.len().max(1));
        std::mem::replace(self, Self::with_capacity(packet_capacity))
    }

    #[cfg(test)]
    pub(super) fn recycler_for_tests(capacity: usize) -> Arc<RawPacketBatchRecycler> {
        RawPacketBatchRecycler::new(capacity)
    }

    #[cfg(test)]
    pub(super) fn from_recycler_for_tests(recycler: &Arc<RawPacketBatchRecycler>) -> Self {
        recycler.allocate()
    }
}

impl Default for RawPacketBatch {
    fn default() -> Self {
        Self::with_capacity(1)
    }
}

impl Drop for RawPacketBatch {
    fn drop(&mut self) {
        let Some(recycler) = self.recycler.as_ref() else {
            return;
        };
        if let Some(storage) = self.storage.take() {
            recycler.recycle(storage);
        }
    }
}

#[derive(Debug, Error)]
pub(super) enum UdpReceiverError {
    #[error("failed to bind udp receiver socket on {bind_addr}: {source}")]
    BindSocket {
        bind_addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("failed to set udp receiver socket blocking mode: {source}")]
    SetBlockingMode { source: std::io::Error },
    #[error("failed to set udp receiver read timeout: {source}")]
    SetReadTimeout { source: std::io::Error },
    #[error("invalid udp packet length {len}; buffer capacity {capacity}")]
    InvalidPacketLength { len: usize, capacity: usize },
    #[error("udp receive failed: {source}")]
    Receive { source: std::io::Error },
}

#[derive(Clone, Debug, Default)]
pub struct ReceiverTelemetry {
    packets: Arc<AtomicU64>,
    last_packet_unix_ms: Arc<AtomicU64>,
    dropped_packets: Arc<AtomicU64>,
    dropped_batches: Arc<AtomicU64>,
    sent_batches: Arc<AtomicU64>,
    sent_packets: Arc<AtomicU64>,
    rxq_ovfl_drops: Arc<AtomicU64>,
}

impl ReceiverTelemetry {
    #[must_use]
    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.packets.load(Ordering::Relaxed),
            self.last_packet_unix_ms.load(Ordering::Relaxed),
        )
    }

    #[must_use]
    pub fn dropped_packets(&self) -> u64 {
        self.dropped_packets.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn dropped_batches(&self) -> u64 {
        self.dropped_batches.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn sent_packets(&self) -> u64 {
        self.sent_packets.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn sent_batches(&self) -> u64 {
        self.sent_batches.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn rxq_ovfl_drops(&self) -> u64 {
        self.rxq_ovfl_drops.load(Ordering::Relaxed)
    }

    fn record_packet(&self) {
        self.packets.fetch_add(1, Ordering::Relaxed);
        self.last_packet_unix_ms
            .store(current_unix_ms(), Ordering::Relaxed);
    }

    fn record_packets(&self, packet_count: usize) {
        self.packets.fetch_add(
            u64::try_from(packet_count).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.last_packet_unix_ms
            .store(current_unix_ms(), Ordering::Relaxed);
    }

    fn record_sent_batch(&self, packet_count: usize) {
        self.sent_batches.fetch_add(1, Ordering::Relaxed);
        self.sent_packets.fetch_add(
            u64::try_from(packet_count).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    fn record_dropped_batch(&self, packet_count: usize) {
        self.dropped_batches.fetch_add(1, Ordering::Relaxed);
        self.dropped_packets.fetch_add(
            u64::try_from(packet_count).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    fn record_rxq_ovfl_drops(&self, drop_count: u64) {
        self.rxq_ovfl_drops.fetch_add(drop_count, Ordering::Relaxed);
    }
}

#[must_use]
pub fn spawn_udp_receiver(bind_addr: SocketAddr, tx: RawPacketBatchSender) -> JoinHandle<()> {
    spawn_udp_receiver_with_shutdown(bind_addr, tx, None)
}

#[must_use]
pub fn spawn_udp_receiver_with_shutdown(
    bind_addr: SocketAddr,
    tx: RawPacketBatchSender,
    shutdown: Option<Arc<AtomicBool>>,
) -> JoinHandle<()> {
    task::spawn_blocking(move || {
        if let Err(err) = run_udp_receiver(bind_addr, &tx, None, shutdown.as_ref()) {
            tracing::error!(%bind_addr, error = %err, "udp receiver terminated");
        }
    })
}

#[must_use]
pub fn spawn_udp_receiver_from_std(
    std_socket: std::net::UdpSocket,
    tx: RawPacketBatchSender,
) -> JoinHandle<()> {
    spawn_udp_receiver_from_std_with_telemetry_and_shutdown(std_socket, tx, None, None)
}

#[must_use]
pub fn spawn_udp_receiver_from_std_with_telemetry(
    std_socket: std::net::UdpSocket,
    tx: RawPacketBatchSender,
    telemetry: Option<ReceiverTelemetry>,
) -> JoinHandle<()> {
    spawn_udp_receiver_from_std_with_telemetry_and_shutdown(std_socket, tx, telemetry, None)
}

#[must_use]
pub fn spawn_udp_receiver_from_std_with_telemetry_and_shutdown(
    std_socket: std::net::UdpSocket,
    tx: RawPacketBatchSender,
    telemetry: Option<ReceiverTelemetry>,
    shutdown: Option<Arc<AtomicBool>>,
) -> JoinHandle<()> {
    task::spawn_blocking(move || {
        if let Err(err) =
            run_udp_receiver_with_socket(&std_socket, &tx, telemetry.as_ref(), shutdown.as_ref())
        {
            tracing::error!(error = %err, "udp receiver terminated");
        }
    })
}

fn run_udp_receiver(
    bind_addr: SocketAddr,
    tx: &RawPacketBatchSender,
    telemetry: Option<&ReceiverTelemetry>,
    shutdown: Option<&Arc<AtomicBool>>,
) -> Result<(), UdpReceiverError> {
    let std_socket = std::net::UdpSocket::bind(bind_addr)
        .map_err(|source| UdpReceiverError::BindSocket { bind_addr, source })?;
    tracing::info!(%bind_addr, "listening for udp packets");
    run_udp_receiver_with_socket(&std_socket, tx, telemetry, shutdown)
}

fn run_udp_receiver_with_socket(
    std_socket: &std::net::UdpSocket,
    tx: &RawPacketBatchSender,
    telemetry: Option<&ReceiverTelemetry>,
    shutdown: Option<&Arc<AtomicBool>>,
) -> Result<(), UdpReceiverError> {
    tune_udp_socket(std_socket);
    maybe_pin_receiver_thread(std_socket);

    let batch_max_wait = Duration::from_millis(read_udp_batch_max_wait_ms());
    let idle_wait = Duration::from_millis(read_udp_idle_wait_ms()).max(batch_max_wait);
    let track_rxq_ovfl = enable_rxq_ovfl_tracking(std_socket);

    let batch_size = read_udp_batch_size();
    #[cfg(target_os = "linux")]
    let mut batch_scratch = (!track_rxq_ovfl).then(|| io::UdpBatchScratch::new(batch_size));
    #[cfg(target_os = "linux")]
    let mut poll_fd =
        (!track_rxq_ovfl).then(|| [PollFd::new(std_socket.as_fd(), PollFlags::POLLIN)]);
    #[cfg(target_os = "linux")]
    if batch_scratch.is_some() {
        std_socket
            .set_nonblocking(true)
            .map_err(|source| UdpReceiverError::SetBlockingMode { source })?;
    } else {
        std_socket
            .set_nonblocking(false)
            .map_err(|source| UdpReceiverError::SetBlockingMode { source })?;
        std_socket
            .set_read_timeout(Some(idle_wait))
            .map_err(|source| UdpReceiverError::SetReadTimeout { source })?;
    }
    #[cfg(not(target_os = "linux"))]
    {
        std_socket
            .set_nonblocking(false)
            .map_err(|source| UdpReceiverError::SetBlockingMode { source })?;
        std_socket
            .set_read_timeout(Some(idle_wait))
            .map_err(|source| UdpReceiverError::SetReadTimeout { source })?;
    }
    #[cfg(not(target_os = "linux"))]
    let mut current_wait = idle_wait;
    #[cfg(target_os = "linux")]
    let mut current_wait = batch_scratch.as_ref().map_or(idle_wait, |_| batch_max_wait);
    let recycler = RawPacketBatchRecycler::new(batch_size);
    let mut buffer = vec![0_u8; UDP_PACKET_BUFFER_BYTES];
    let mut batch = recycler.allocate();
    let mut batch_started_at: Option<Instant> = None;
    let mut last_rxq_ovfl_counter: Option<u64> = None;
    loop {
        if should_shutdown(shutdown) {
            flush_batch(tx, &mut batch, telemetry);
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        if let (Some(scratch), Some(poll_fd)) = (batch_scratch.as_mut(), poll_fd.as_mut()) {
            match recv_udp_batch_coalesced(
                std_socket,
                scratch,
                &mut batch,
                idle_wait,
                batch_max_wait,
                poll_fd,
            ) {
                Ok(received) => {
                    if received == 0 {
                        continue;
                    }
                    if let Some(telemetry) = telemetry {
                        telemetry.record_packets(received);
                    }
                    flush_batch(tx, &mut batch, telemetry);
                    continue;
                }
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock
                        || error.kind() == ErrorKind::TimedOut =>
                {
                    if should_shutdown(shutdown) {
                        return Ok(());
                    }
                    continue;
                }
                Err(error) => {
                    return Err(UdpReceiverError::Receive { source: error });
                }
            }
        }
        match recv_udp_packet(std_socket, &mut buffer, track_rxq_ovfl) {
            Ok(packet) => {
                if let (Some(telemetry), Some(drop_counter)) = (telemetry, packet.rxq_ovfl_counter)
                {
                    if let Some(previous) = last_rxq_ovfl_counter {
                        let dropped = drop_counter.saturating_sub(previous);
                        if dropped > 0 {
                            telemetry.record_rxq_ovfl_drops(dropped);
                        }
                    }
                    last_rxq_ovfl_counter = Some(drop_counter);
                }
                let bytes =
                    buffer
                        .get(..packet.len)
                        .ok_or(UdpReceiverError::InvalidPacketLength {
                            len: packet.len,
                            capacity: buffer.len(),
                        })?;
                if batch_started_at.is_none() {
                    batch_started_at = Some(Instant::now());
                    if current_wait != batch_max_wait {
                        std_socket
                            .set_read_timeout(Some(batch_max_wait))
                            .map_err(|source| UdpReceiverError::SetReadTimeout { source })?;
                        current_wait = batch_max_wait;
                    }
                }
                if let Some(telemetry) = telemetry {
                    telemetry.record_packet();
                }
                batch.push_packet(packet.source, RawPacketIngress::Udp, bytes)?;
                let batch_elapsed = batch_started_at
                    .map(|started_at| started_at.elapsed())
                    .unwrap_or_default();
                if batch.len() >= batch_size || batch_elapsed >= batch_max_wait {
                    flush_batch(tx, &mut batch, telemetry);
                    batch_started_at = None;
                    if current_wait != idle_wait {
                        std_socket
                            .set_read_timeout(Some(idle_wait))
                            .map_err(|source| UdpReceiverError::SetReadTimeout { source })?;
                        current_wait = idle_wait;
                    }
                }
            }
            Err(error)
                if error.kind() == ErrorKind::WouldBlock || error.kind() == ErrorKind::TimedOut =>
            {
                flush_batch(tx, &mut batch, telemetry);
                batch_started_at = None;
                if current_wait != idle_wait {
                    std_socket
                        .set_read_timeout(Some(idle_wait))
                        .map_err(|source| UdpReceiverError::SetReadTimeout { source })?;
                    current_wait = idle_wait;
                }
                if should_shutdown(shutdown) {
                    return Ok(());
                }
            }
            Err(error) => {
                return Err(UdpReceiverError::Receive { source: error });
            }
        }
    }
}

fn should_shutdown(shutdown: Option<&Arc<AtomicBool>>) -> bool {
    shutdown.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

#[path = "io.rs"]
mod io;
use io::{
    current_unix_ms, flush_batch, maybe_pin_receiver_thread, recv_udp_batch_coalesced,
    recv_udp_packet, tune_udp_socket,
};
