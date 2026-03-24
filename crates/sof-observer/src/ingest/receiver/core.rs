use std::io::{ErrorKind, IoSliceMut};
use std::net::SocketAddr;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// Linux-only ancillary control messages are used for RX queue overflow telemetry.
// Runtime ingest remains cross-platform; non-Linux builds use the standard recv_from path.
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

#[derive(Debug, Clone, Copy)]
pub struct RawPacket {
    pub source: SocketAddr,
    pub ingress: RawPacketIngress,
    pub buffer_index: usize,
    pub len: usize,
}

#[derive(Debug, Default)]
pub struct RawPacketBatch {
    buffers: Vec<[u8; UDP_PACKET_BUFFER_BYTES]>,
    packets: Vec<RawPacket>,
}

impl RawPacketBatch {
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffers: vec![[0_u8; UDP_PACKET_BUFFER_BYTES]; capacity],
            packets: Vec::with_capacity(capacity),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.packets.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    pub fn clear(&mut self) {
        self.packets.clear();
    }

    pub fn reserve(&mut self, additional: usize) {
        let needed = self.packets.len().saturating_add(additional);
        if needed > self.buffers.len() {
            self.buffers.resize(needed, [0_u8; UDP_PACKET_BUFFER_BYTES]);
        }
        self.packets.reserve(additional);
    }

    pub(super) fn push_packet(
        &mut self,
        source: SocketAddr,
        ingress: RawPacketIngress,
        bytes: &[u8],
    ) -> Result<(), UdpReceiverError> {
        if bytes.len() > UDP_PACKET_BUFFER_BYTES {
            return Err(UdpReceiverError::InvalidPacketLength {
                len: bytes.len(),
                capacity: UDP_PACKET_BUFFER_BYTES,
            });
        }
        let buffer_index = self.packets.len();
        if buffer_index == self.buffers.len() {
            self.buffers.push([0_u8; UDP_PACKET_BUFFER_BYTES]);
        }
        self.buffers[buffer_index][..bytes.len()].copy_from_slice(bytes);
        self.packets.push(RawPacket {
            source,
            ingress,
            buffer_index,
            len: bytes.len(),
        });
        Ok(())
    }

    #[must_use]
    pub fn packets(&self) -> &[RawPacket] {
        &self.packets
    }

    #[must_use]
    pub fn packet_bytes(&self, packet: RawPacket) -> &[u8] {
        &self.buffers[packet.buffer_index][..packet.len]
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
    std_socket
        .set_nonblocking(false)
        .map_err(|source| UdpReceiverError::SetBlockingMode { source })?;
    tune_udp_socket(std_socket);
    maybe_pin_receiver_thread(std_socket);

    let batch_max_wait = Duration::from_millis(read_udp_batch_max_wait_ms());
    let idle_wait = Duration::from_millis(read_udp_idle_wait_ms()).max(batch_max_wait);
    std_socket
        .set_read_timeout(Some(idle_wait))
        .map_err(|source| UdpReceiverError::SetReadTimeout { source })?;
    let mut current_wait = idle_wait;
    let track_rxq_ovfl = enable_rxq_ovfl_tracking(std_socket);

    let batch_size = read_udp_batch_size();
    #[cfg(target_os = "linux")]
    let mut batch_scratch = (!track_rxq_ovfl).then(|| io::UdpBatchScratch::new(batch_size));
    let mut buffer = vec![0_u8; UDP_PACKET_BUFFER_BYTES];
    let mut batch = RawPacketBatch::with_capacity(batch_size);
    let mut batch_started_at: Option<Instant> = None;
    let mut last_rxq_ovfl_counter: Option<u64> = None;
    loop {
        if should_shutdown(shutdown) {
            flush_batch(tx, &mut batch, telemetry);
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        if let Some(scratch) = batch_scratch.as_mut() {
            match recv_udp_batch(std_socket, scratch, &mut batch) {
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
    current_unix_ms, flush_batch, maybe_pin_receiver_thread, recv_udp_batch, recv_udp_packet,
    tune_udp_socket,
};
