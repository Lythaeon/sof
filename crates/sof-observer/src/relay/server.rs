use std::net::SocketAddr;

use thiserror::Error;
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::broadcast,
    task::{self, JoinHandle},
};

#[derive(Debug, Error)]
enum RelayServerError {
    #[error("failed to bind tcp relay server on {bind_addr}: {source}")]
    Bind {
        bind_addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("failed to accept tcp relay client on {bind_addr}: {source}")]
    Accept {
        bind_addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("failed to write tcp relay frame: {source}")]
    Write { source: std::io::Error },
}

#[must_use]
pub fn spawn_tcp_relay_server(
    bind_addr: SocketAddr,
    packet_bus: broadcast::Sender<Vec<u8>>,
) -> JoinHandle<()> {
    task::spawn(async move {
        if let Err(error) = run_tcp_relay_server(bind_addr, packet_bus).await {
            tracing::error!(%bind_addr, error = %error, "tcp relay server terminated");
        }
    })
}

async fn run_tcp_relay_server(
    bind_addr: SocketAddr,
    packet_bus: broadcast::Sender<Vec<u8>>,
) -> Result<(), RelayServerError> {
    const MAX_FRAME_SIZE: usize = u16::MAX as usize;

    let listener = TcpListener::bind(bind_addr)
        .await
        .map_err(|source| RelayServerError::Bind { bind_addr, source })?;
    tracing::info!(%bind_addr, "tcp relay server listening");

    loop {
        let (stream, peer) = listener
            .accept()
            .await
            .map_err(|source| RelayServerError::Accept { bind_addr, source })?;
        tracing::info!(%peer, "tcp relay client connected");
        let mut rx = packet_bus.subscribe();
        task::spawn(async move {
            if let Err(error) = stream_packets(stream, &mut rx, MAX_FRAME_SIZE).await {
                tracing::warn!(%peer, error = %error, "tcp relay client disconnected");
            }
        });
    }
}

async fn stream_packets(
    mut stream: TcpStream,
    rx: &mut broadcast::Receiver<Vec<u8>>,
    max_frame_size: usize,
) -> Result<(), RelayServerError> {
    loop {
        let packet = match rx.recv().await {
            Ok(packet) => packet,
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "tcp relay client lagged; dropping stale packets");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        };

        if packet.is_empty() || packet.len() > max_frame_size {
            continue;
        }

        let len = (packet.len() as u16).to_be_bytes();
        stream
            .write_all(&len)
            .await
            .map_err(|source| RelayServerError::Write { source })?;
        stream
            .write_all(&packet)
            .await
            .map_err(|source| RelayServerError::Write { source })?;
    }
}
