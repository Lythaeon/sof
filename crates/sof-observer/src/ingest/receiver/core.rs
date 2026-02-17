use std::io::{ErrorKind, IoSliceMut};
use std::net::SocketAddr;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(target_os = "linux")]
use nix::sys::socket::{ControlMessageOwned, MsgFlags, SockaddrStorage, recvmsg};
use socket2::SockRef;
use thiserror::Error;
use tokio::{
    io::AsyncReadExt,
    sync::mpsc,
    task::{self, JoinHandle},
    time::sleep,
};

use crate::ingest::config::{
    enable_rxq_ovfl_tracking, read_udp_batch_max_wait_ms, read_udp_batch_size,
    read_udp_idle_wait_ms, read_udp_rcvbuf_bytes, read_udp_receiver_core,
};

#[derive(Debug)]
pub struct RawPacket {
    pub source: SocketAddr,
    pub bytes: Vec<u8>,
}

pub type RawPacketBatch = Vec<RawPacket>;

#[derive(Debug, Error)]
enum UdpReceiverError {
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
pub fn spawn_udp_receiver(
    bind_addr: SocketAddr,
    tx: mpsc::Sender<RawPacketBatch>,
) -> JoinHandle<()> {
    task::spawn_blocking(move || {
        if let Err(err) = run_udp_receiver(bind_addr, &tx, None) {
            tracing::error!(%bind_addr, error = %err, "udp receiver terminated");
        }
    })
}

#[must_use]
pub fn spawn_tcp_relay_receiver(
    remote_addr: SocketAddr,
    tx: mpsc::Sender<RawPacketBatch>,
) -> JoinHandle<()> {
    task::spawn(async move {
        if let Err(err) = run_tcp_relay_receiver(remote_addr, tx).await {
            tracing::error!(remote = %remote_addr, error = %err, "tcp relay receiver terminated");
        }
    })
}

#[must_use]
pub fn spawn_udp_receiver_from_std(
    std_socket: std::net::UdpSocket,
    tx: mpsc::Sender<RawPacketBatch>,
) -> JoinHandle<()> {
    spawn_udp_receiver_from_std_with_telemetry(std_socket, tx, None)
}

#[must_use]
pub fn spawn_udp_receiver_from_std_with_telemetry(
    std_socket: std::net::UdpSocket,
    tx: mpsc::Sender<RawPacketBatch>,
    telemetry: Option<ReceiverTelemetry>,
) -> JoinHandle<()> {
    task::spawn_blocking(move || {
        if let Err(err) = run_udp_receiver_with_socket(&std_socket, &tx, telemetry.as_ref()) {
            tracing::error!(error = %err, "udp receiver terminated");
        }
    })
}

fn run_udp_receiver(
    bind_addr: SocketAddr,
    tx: &mpsc::Sender<RawPacketBatch>,
    telemetry: Option<&ReceiverTelemetry>,
) -> Result<(), UdpReceiverError> {
    let std_socket = std::net::UdpSocket::bind(bind_addr)
        .map_err(|source| UdpReceiverError::BindSocket { bind_addr, source })?;
    tracing::info!(%bind_addr, "listening for udp packets");
    run_udp_receiver_with_socket(&std_socket, tx, telemetry)
}

fn run_udp_receiver_with_socket(
    std_socket: &std::net::UdpSocket,
    tx: &mpsc::Sender<RawPacketBatch>,
    telemetry: Option<&ReceiverTelemetry>,
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
    let mut buffer = vec![0_u8; 2048];
    let mut batch = Vec::with_capacity(batch_size);
    let mut batch_started_at: Option<Instant> = None;
    let mut last_rxq_ovfl_counter: Option<u64> = None;
    loop {
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
                batch.push(RawPacket {
                    source: packet.source,
                    bytes: bytes.to_vec(),
                });
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
            }
            Err(error) => {
                return Err(UdpReceiverError::Receive { source: error });
            }
        }
    }
}

#[path = "io.rs"]
mod io;
use io::{
    current_unix_ms, flush_batch, maybe_pin_receiver_thread, recv_udp_packet,
    run_tcp_relay_receiver, tune_udp_socket,
};
