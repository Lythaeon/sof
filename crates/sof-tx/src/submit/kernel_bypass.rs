//! Kernel-bypass direct-submit transport hooks for `kernel-bypass` integrations.

use std::{io, net::SocketAddr, sync::Arc, time::Instant};

use async_trait::async_trait;
use tokio::{
    task::JoinSet,
    time::{sleep, timeout},
};

use super::{DirectSubmitConfig, DirectSubmitTransport, SubmitTransportError};
use crate::{providers::LeaderTarget, routing::RoutingPolicy};

/// Datagram socket interface expected by [`KernelBypassDirectTransport`].
///
/// Arb bots can implement this trait for their own `kernel-bypass` socket/client type
/// and pass it into [`KernelBypassDirectTransport::new`].
#[async_trait]
pub trait KernelBypassDatagramSocket: Send + Sync {
    /// Sends one datagram payload to the target socket address.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when send operation fails.
    async fn send_to(&self, payload: &[u8], target: SocketAddr) -> io::Result<usize>;
}

/// `kernel-bypass`-oriented direct transport adapter.
///
/// This transport delegates packet sends to a caller-provided
/// [`KernelBypassDatagramSocket`] implementation.
#[derive(Debug, Clone)]
pub struct KernelBypassDirectTransport<S> {
    /// Caller-provided socket implementation used for datagram sends.
    socket: Arc<S>,
}

impl<S> KernelBypassDirectTransport<S> {
    /// Creates a direct transport around a `kernel-bypass` datagram socket.
    #[must_use]
    pub const fn new(socket: Arc<S>) -> Self {
        Self { socket }
    }
}

#[async_trait]
impl<S> DirectSubmitTransport for KernelBypassDirectTransport<S>
where
    S: KernelBypassDatagramSocket + 'static,
{
    async fn submit_direct(
        &self,
        tx_bytes: &[u8],
        targets: &[LeaderTarget],
        policy: RoutingPolicy,
        config: &DirectSubmitConfig,
    ) -> Result<LeaderTarget, SubmitTransportError> {
        let config = config.clone().normalized();
        if targets.is_empty() {
            return Err(SubmitTransportError::Config {
                message: "no targets provided".to_owned(),
            });
        }

        let deadline = Instant::now()
            .checked_add(config.global_timeout)
            .ok_or_else(|| SubmitTransportError::Failure {
                message: "failed to calculate direct-submit deadline".to_owned(),
            })?;
        let payload: Arc<[u8]> = Arc::from(tx_bytes.to_vec());
        for round in 0..config.direct_target_rounds {
            if round > 0 {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let remaining = deadline.saturating_duration_since(now);
                let sleep_for = remaining.min(config.rebroadcast_interval);
                if !sleep_for.is_zero() {
                    sleep(sleep_for).await;
                }
            }
            for chunk in targets.chunks(policy.normalized().max_parallel_sends) {
                let now = Instant::now();
                if now >= deadline {
                    return Err(SubmitTransportError::Failure {
                        message: "global direct-submit timeout exceeded".to_owned(),
                    });
                }
                let remaining = deadline.saturating_duration_since(now);
                let per_target = remaining.min(config.per_target_timeout);
                let mut in_flight = JoinSet::new();

                for target in chunk {
                    let socket = Arc::clone(&self.socket);
                    let payload = Arc::clone(&payload);
                    let target = target.clone();
                    in_flight.spawn(async move {
                        let send_result = timeout(
                            per_target,
                            socket.send_to(payload.as_ref(), target.tpu_addr),
                        )
                        .await;
                        match send_result {
                            Ok(Ok(_bytes_sent)) => Some(target),
                            Ok(Err(_send_error)) => None,
                            Err(_elapsed) => None,
                        }
                    });
                }

                while let Some(result) = in_flight.join_next().await {
                    if let Ok(Some(target)) = result {
                        in_flight.abort_all();
                        return Ok(target);
                    }
                }
            }
        }

        Err(SubmitTransportError::Failure {
            message: "all direct targets failed".to_owned(),
        })
    }
}
