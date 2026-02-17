//! JSON-RPC submit transport implementation.

use std::time::Duration;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};

use super::{RpcSubmitConfig, RpcSubmitTransport, SubmitTransportError};

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

        let response =
            response
                .error_for_status()
                .map_err(|error| SubmitTransportError::Failure {
                    message: error.to_string(),
                })?;

        let parsed: JsonRpcResponse =
            response
                .json()
                .await
                .map_err(|error| SubmitTransportError::Failure {
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
