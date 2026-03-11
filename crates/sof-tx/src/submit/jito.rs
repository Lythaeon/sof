//! Jito block-engine submit transport implementation.

use std::time::Duration;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use reqwest::Url;
use serde::{Deserialize, Serialize};

use super::{JitoSubmitConfig, JitoSubmitTransport, SubmitTransportError};

/// Default Jito mainnet block-engine base URL.
const DEFAULT_JITO_BLOCK_ENGINE_URL: &str = "https://mainnet.block-engine.jito.wtf";

/// Typed Jito auth token sent as `x-jito-auth`.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct JitoAuthToken(String);

impl JitoAuthToken {
    /// Creates a validated auth token wrapper.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// Returns the raw token string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Typed Jito block-engine endpoint.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum JitoBlockEngineEndpoint {
    /// Default Jito mainnet block-engine endpoint.
    Mainnet,
    /// Custom parsed block-engine base URL.
    Custom(Url),
}

impl JitoBlockEngineEndpoint {
    /// Returns the default mainnet block-engine endpoint.
    #[must_use]
    pub const fn mainnet() -> Self {
        Self::Mainnet
    }

    /// Creates a custom block-engine endpoint from a parsed URL.
    #[must_use]
    pub const fn custom(url: Url) -> Self {
        Self::Custom(url)
    }

    /// Returns the base URL.
    #[must_use]
    pub fn as_url(&self) -> &str {
        match self {
            Self::Mainnet => DEFAULT_JITO_BLOCK_ENGINE_URL,
            Self::Custom(url) => url.as_str(),
        }
    }
}

impl Default for JitoBlockEngineEndpoint {
    fn default() -> Self {
        Self::mainnet()
    }
}

/// Transport-level Jito block-engine settings.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct JitoTransportConfig {
    /// Target Jito block-engine endpoint.
    pub endpoint: JitoBlockEngineEndpoint,
    /// HTTP timeout applied to block-engine requests.
    pub request_timeout: Duration,
    /// Optional auth token sent as `x-jito-auth`.
    pub auth_token: Option<JitoAuthToken>,
}

impl Default for JitoTransportConfig {
    fn default() -> Self {
        Self {
            endpoint: JitoBlockEngineEndpoint::default(),
            request_timeout: Duration::from_secs(10),
            auth_token: None,
        }
    }
}

/// Jito block-engine JSON-RPC transport using `/api/v1/transactions`.
#[derive(Debug, Clone)]
pub struct JitoJsonRpcTransport {
    /// HTTP client used for block-engine calls.
    client: reqwest::Client,
    /// Transport-level request settings.
    transport_config: JitoTransportConfig,
}

impl JitoJsonRpcTransport {
    /// Creates a Jito block-engine transport.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError::Config`] when HTTP client creation fails.
    pub fn new() -> Result<Self, SubmitTransportError> {
        Self::with_config(JitoTransportConfig::default())
    }

    /// Creates a Jito block-engine transport for one typed endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError::Config`] when HTTP client creation fails.
    pub fn with_endpoint(endpoint: JitoBlockEngineEndpoint) -> Result<Self, SubmitTransportError> {
        Self::with_config(JitoTransportConfig {
            endpoint,
            ..JitoTransportConfig::default()
        })
    }

    /// Creates a Jito block-engine transport with explicit transport settings.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError::Config`] when HTTP client creation fails.
    pub fn with_config(
        transport_config: JitoTransportConfig,
    ) -> Result<Self, SubmitTransportError> {
        let client = reqwest::Client::builder()
            .timeout(transport_config.request_timeout)
            .build()
            .map_err(|error| SubmitTransportError::Config {
                message: error.to_string(),
            })?;
        Ok(Self {
            client,
            transport_config,
        })
    }

    /// Builds the per-request endpoint URL with optional revert protection.
    fn request_url(&self, config: &JitoSubmitConfig) -> String {
        let mut url = self
            .transport_config
            .endpoint
            .as_url()
            .trim_end_matches('/')
            .to_owned();
        url.push_str("/api/v1/transactions");
        if config.bundle_only {
            url.push_str("?bundleOnly=true");
        }
        url
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
impl JitoSubmitTransport for JitoJsonRpcTransport {
    async fn submit_jito(
        &self,
        tx_bytes: &[u8],
        config: &JitoSubmitConfig,
    ) -> Result<String, SubmitTransportError> {
        #[derive(Debug, Serialize)]
        struct JitoRpcConfig<'config> {
            /// Transaction encoding format.
            encoding: &'config str,
        }

        let encoded_tx = BASE64_STANDARD.encode(tx_bytes);
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "sendTransaction",
            "params": [
                encoded_tx,
                JitoRpcConfig { encoding: "base64" }
            ]
        });

        let mut request = self.client.post(self.request_url(config)).json(&payload);
        if let Some(auth_token) = &self.transport_config.auth_token {
            request = request.header("x-jito-auth", auth_token.as_str());
        }

        let response = request
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
                message: format!("jito error {}: {}", error.code, error.message),
            });
        }

        Err(SubmitTransportError::Failure {
            message: "jito returned neither result nor error".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_url_uses_transactions_path() {
        let transport_result = JitoJsonRpcTransport::new();
        assert!(transport_result.is_ok());
        let Some(transport) = transport_result.ok() else {
            return;
        };

        let url = transport.request_url(&JitoSubmitConfig::default());

        assert_eq!(
            url,
            "https://mainnet.block-engine.jito.wtf/api/v1/transactions"
        );
    }

    #[test]
    fn request_url_appends_bundle_only_query() {
        let parsed_url_result = Url::parse("https://mainnet.block-engine.jito.wtf/");
        assert!(parsed_url_result.is_ok());
        let Some(parsed_url) = parsed_url_result.ok() else {
            return;
        };
        let transport_result =
            JitoJsonRpcTransport::with_endpoint(JitoBlockEngineEndpoint::custom(parsed_url));
        assert!(transport_result.is_ok());
        let Some(transport) = transport_result.ok() else {
            return;
        };

        let url = transport.request_url(&JitoSubmitConfig { bundle_only: true });

        assert_eq!(
            url,
            "https://mainnet.block-engine.jito.wtf/api/v1/transactions?bundleOnly=true"
        );
    }

    #[test]
    fn transport_config_defaults_are_stable() {
        let config = JitoTransportConfig::default();

        assert_eq!(config.endpoint, JitoBlockEngineEndpoint::mainnet());
        assert_eq!(config.request_timeout, Duration::from_secs(10));
        assert_eq!(config.auth_token, None);
    }
}
