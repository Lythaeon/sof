//! UDP direct-submit transport implementation.

use std::time::Instant;

use async_trait::async_trait;
use tokio::{net::UdpSocket, time::timeout};

use super::{DirectSubmitConfig, DirectSubmitTransport, SubmitTransportError};
use crate::{providers::LeaderTarget, routing::RoutingPolicy};

/// UDP-based direct transport that sends transaction bytes to TPU targets.
#[derive(Debug, Default, Clone, Copy)]
pub struct UdpDirectTransport;

#[async_trait]
impl DirectSubmitTransport for UdpDirectTransport {
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

        let socket =
            UdpSocket::bind("0.0.0.0:0")
                .await
                .map_err(|error| SubmitTransportError::Failure {
                    message: error.to_string(),
                })?;

        let deadline = Instant::now()
            .checked_add(config.global_timeout)
            .ok_or_else(|| SubmitTransportError::Failure {
                message: "failed to calculate direct-submit deadline".to_owned(),
            })?;
        for _round in 0..config.direct_target_rounds {
            for chunk in targets.chunks(policy.normalized().max_parallel_sends) {
                for target in chunk {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(SubmitTransportError::Failure {
                            message: "global direct-submit timeout exceeded".to_owned(),
                        });
                    }
                    let remaining = deadline.saturating_duration_since(now);
                    let per_target = remaining.min(config.per_target_timeout);
                    let send_result =
                        timeout(per_target, socket.send_to(tx_bytes, target.tpu_addr)).await;
                    match send_result {
                        Ok(Ok(_bytes_sent)) => return Ok(target.clone()),
                        Ok(Err(_send_error)) => {}
                        Err(_elapsed) => {}
                    }
                }
            }
        }

        Err(SubmitTransportError::Failure {
            message: "all direct targets failed".to_owned(),
        })
    }
}
