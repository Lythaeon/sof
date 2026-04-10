//! JSON-RPC submit transport implementation.

use std::time::Duration;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::from_slice as json_from_slice;

use super::{RpcSubmitConfig, RpcSubmitTransport, SubmitTransportError};

/// Maximum HTTP body size accepted from JSON-RPC submit responses.
const MAX_RPC_SUBMIT_RESPONSE_BYTES: usize = 64 * 1024;

/// JSON-RPC transport that submits encoded transactions via `sendTransaction`.
#[derive(Debug, Clone)]
pub struct JsonRpcTransport {
    /// HTTP client used for RPC calls.
    client: reqwest::Client,
    /// Target JSON-RPC endpoint URL.
    rpc_url: String,
}

impl JsonRpcTransport {
    /// Creates a JSON-RPC transport.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError::Config`] when HTTP client creation fails.
    pub fn new(rpc_url: impl Into<String>) -> Result<Self, SubmitTransportError> {
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| SubmitTransportError::Config {
                message: error.to_string(),
            })?;
        Ok(Self {
            client,
            rpc_url: rpc_url.into(),
        })
    }
}

/// JSON-RPC envelope.
#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    /// Result value for successful calls.
    result: Option<String>,
    /// Error payload for failed calls.
    error: Option<JsonRpcError>,
}

/// JSON-RPC error object.
#[derive(Debug, Deserialize)]
struct JsonRpcError {
    /// JSON-RPC error code.
    code: i64,
    /// Human-readable message.
    message: String,
}

#[async_trait]
impl RpcSubmitTransport for JsonRpcTransport {
    async fn submit_rpc(
        &self,
        tx_bytes: &[u8],
        config: &RpcSubmitConfig,
    ) -> Result<String, SubmitTransportError> {
        #[derive(Debug, Serialize)]
        struct RpcConfig<'config> {
            /// Transaction encoding format.
            encoding: &'config str,
            /// Optional preflight skip flag.
            #[serde(rename = "skipPreflight")]
            skip_preflight: bool,
            /// Optional preflight commitment.
            #[serde(
                rename = "preflightCommitment",
                skip_serializing_if = "Option::is_none"
            )]
            preflight_commitment: Option<&'config str>,
        }

        let encoded_tx = BASE64_STANDARD.encode(tx_bytes);
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendTransaction",
            "params": [
                encoded_tx,
                RpcConfig {
                    encoding: "base64",
                    skip_preflight: config.skip_preflight,
                    preflight_commitment: config.preflight_commitment.as_deref(),
                }
            ]
        });

        let response = self
            .client
            .post(&self.rpc_url)
            .json(&payload)
            .send()
            .await
            .map_err(|error| SubmitTransportError::Failure {
                message: error.to_string(),
            })?;
        if response.status().is_redirection() {
            return Err(SubmitTransportError::Failure {
                message: format!("unexpected redirect response: {}", response.status()),
            });
        }

        let response =
            response
                .error_for_status()
                .map_err(|error| SubmitTransportError::Failure {
                    message: error.to_string(),
                })?;

        let response_body = read_http_response_bytes_bounded(response).await?;
        let parsed: JsonRpcResponse =
            json_from_slice(&response_body).map_err(|error| SubmitTransportError::Failure {
                message: error.to_string(),
            })?;

        if let Some(signature) = parsed.result {
            return Ok(signature);
        }
        if let Some(error) = parsed.error {
            return Err(SubmitTransportError::Failure {
                message: format!("rpc error {}: {}", error.code, error.message),
            });
        }

        Err(SubmitTransportError::Failure {
            message: "rpc returned neither result nor error".to_owned(),
        })
    }
}

/// Reads one submit response body while enforcing a fixed maximum byte budget.
async fn read_http_response_bytes_bounded(
    mut response: reqwest::Response,
) -> Result<Vec<u8>, SubmitTransportError> {
    if response
        .content_length()
        .is_some_and(|content_length| content_length > MAX_RPC_SUBMIT_RESPONSE_BYTES as u64)
    {
        return Err(SubmitTransportError::Failure {
            message: format!(
                "response body exceeded max size of {MAX_RPC_SUBMIT_RESPONSE_BYTES} bytes"
            ),
        });
    }

    let initial_capacity = response
        .content_length()
        .and_then(|content_length| usize::try_from(content_length).ok())
        .unwrap_or(0)
        .min(MAX_RPC_SUBMIT_RESPONSE_BYTES);
    let mut body = Vec::with_capacity(initial_capacity);
    while let Some(chunk) =
        response
            .chunk()
            .await
            .map_err(|error| SubmitTransportError::Failure {
                message: error.to_string(),
            })?
    {
        let remaining = MAX_RPC_SUBMIT_RESPONSE_BYTES.saturating_sub(body.len());
        if chunk.len() > remaining {
            return Err(SubmitTransportError::Failure {
                message: format!(
                    "response body exceeded max size of {MAX_RPC_SUBMIT_RESPONSE_BYTES} bytes"
                ),
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn spawn_http_response_server(response: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await;
        assert!(listener.is_ok());
        let listener = listener.unwrap_or_else(|error| panic!("{error}"));
        let addr = listener.local_addr();
        assert!(addr.is_ok());
        let addr = addr.unwrap_or_else(|error| panic!("{error}"));
        tokio::spawn(async move {
            let accepted = listener.accept().await;
            assert!(accepted.is_ok());
            let (mut stream, _) = accepted.unwrap_or_else(|error| panic!("{error}"));
            let mut buffer = [0_u8; 4096];
            let read = stream.read(&mut buffer).await;
            assert!(read.is_ok());
            let write = stream.write_all(response.as_bytes()).await;
            assert!(write.is_ok());
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn json_rpc_transport_rejects_redirects() {
        let endpoint = spawn_http_response_server(
            "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://127.0.0.1/\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                .to_owned(),
        )
        .await;
        let transport = JsonRpcTransport::new(endpoint);
        assert!(transport.is_ok());
        let transport = transport.unwrap_or_else(|error| panic!("{error}"));

        let error = transport
            .submit_rpc(&[1, 2, 3], &RpcSubmitConfig::default())
            .await;
        assert!(error.is_err());
        let error = match error {
            Ok(_signature) => panic!("redirect should fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("redirect"));
    }

    #[tokio::test]
    async fn json_rpc_transport_rejects_oversized_responses() {
        let endpoint = spawn_http_response_server(format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            MAX_RPC_SUBMIT_RESPONSE_BYTES.saturating_add(1)
        ))
        .await;
        let transport = JsonRpcTransport::new(endpoint);
        assert!(transport.is_ok());
        let transport = transport.unwrap_or_else(|error| panic!("{error}"));

        let error = transport
            .submit_rpc(&[1, 2, 3], &RpcSubmitConfig::default())
            .await;
        assert!(error.is_err());
        let error = match error {
            Ok(_signature) => panic!("oversized body should fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("exceeded max size"));
    }
}
