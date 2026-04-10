//! Jito block-engine submit transport implementation.

use std::time::Duration;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use reqwest::{Url, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::from_slice as json_from_slice;
use sof_support::time_support::nonzero_duration_or;

use super::{JitoSubmitConfig, JitoSubmitResponse, JitoSubmitTransport, SubmitTransportError};

/// Default Jito mainnet block-engine base URL.
const DEFAULT_JITO_BLOCK_ENGINE_URL: &str = "https://mainnet.block-engine.jito.wtf";
/// Maximum HTTP body size accepted from Jito submit responses.
const MAX_JITO_SUBMIT_RESPONSE_BYTES: usize = 64 * 1024;
/// Default timeout used for Jito HTTP requests.
const DEFAULT_JITO_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Typed Jito mainnet region.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum JitoBlockEngineRegion {
    /// Amsterdam region.
    Amsterdam,
    /// Dublin region.
    Dublin,
    /// Frankfurt region.
    Frankfurt,
    /// London region.
    London,
    /// New York region.
    NewYork,
    /// Salt Lake City region.
    SaltLakeCity,
    /// Singapore region.
    Singapore,
    /// Tokyo region.
    Tokyo,
}

/// Typed Jito block-engine endpoint.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum JitoBlockEngineEndpoint {
    /// Default Jito mainnet block-engine endpoint.
    Mainnet,
    /// Region-specific Jito mainnet block-engine endpoint.
    MainnetRegion(JitoBlockEngineRegion),
    /// Custom parsed block-engine base URL.
    Custom(Url),
}

impl JitoBlockEngineEndpoint {
    /// Returns the default mainnet block-engine endpoint.
    #[must_use]
    pub const fn mainnet() -> Self {
        Self::Mainnet
    }

    /// Returns one typed regional mainnet block-engine endpoint.
    #[must_use]
    pub const fn mainnet_region(region: JitoBlockEngineRegion) -> Self {
        Self::MainnetRegion(region)
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
            Self::MainnetRegion(region) => match region {
                JitoBlockEngineRegion::Amsterdam => {
                    "https://amsterdam.mainnet.block-engine.jito.wtf"
                }
                JitoBlockEngineRegion::Dublin => "https://dublin.mainnet.block-engine.jito.wtf",
                JitoBlockEngineRegion::Frankfurt => {
                    "https://frankfurt.mainnet.block-engine.jito.wtf"
                }
                JitoBlockEngineRegion::London => "https://london.mainnet.block-engine.jito.wtf",
                JitoBlockEngineRegion::NewYork => "https://ny.mainnet.block-engine.jito.wtf",
                JitoBlockEngineRegion::SaltLakeCity => "https://slc.mainnet.block-engine.jito.wtf",
                JitoBlockEngineRegion::Singapore => {
                    "https://singapore.mainnet.block-engine.jito.wtf"
                }
                JitoBlockEngineRegion::Tokyo => "https://tokyo.mainnet.block-engine.jito.wtf",
            },
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
}

impl Default for JitoTransportConfig {
    fn default() -> Self {
        Self {
            endpoint: JitoBlockEngineEndpoint::default(),
            request_timeout: DEFAULT_JITO_REQUEST_TIMEOUT,
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
        let request_timeout = nonzero_duration_or(
            transport_config.request_timeout,
            DEFAULT_JITO_REQUEST_TIMEOUT,
        );
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .connect_timeout(request_timeout)
            .timeout(request_timeout)
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
    ) -> Result<JitoSubmitResponse, SubmitTransportError> {
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

        let response = self
            .client
            .post(self.request_url(config))
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
            return Ok(JitoSubmitResponse {
                transaction_signature: Some(signature),
                bundle_id: None,
            });
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

/// Reads one Jito submit response body while enforcing a fixed maximum byte budget.
async fn read_http_response_bytes_bounded(
    mut response: reqwest::Response,
) -> Result<Vec<u8>, SubmitTransportError> {
    if response
        .content_length()
        .is_some_and(|content_length| content_length > MAX_JITO_SUBMIT_RESPONSE_BYTES as u64)
    {
        return Err(SubmitTransportError::Failure {
            message: format!(
                "response body exceeded max size of {MAX_JITO_SUBMIT_RESPONSE_BYTES} bytes"
            ),
        });
    }

    let initial_capacity = response
        .content_length()
        .and_then(|content_length| usize::try_from(content_length).ok())
        .unwrap_or(0)
        .min(MAX_JITO_SUBMIT_RESPONSE_BYTES);
    let mut body = Vec::with_capacity(initial_capacity);
    while let Some(chunk) =
        response
            .chunk()
            .await
            .map_err(|error| SubmitTransportError::Failure {
                message: error.to_string(),
            })?
    {
        let remaining = MAX_JITO_SUBMIT_RESPONSE_BYTES.saturating_sub(body.len());
        if chunk.len() > remaining {
            return Err(SubmitTransportError::Failure {
                message: format!(
                    "response body exceeded max size of {MAX_JITO_SUBMIT_RESPONSE_BYTES} bytes"
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
    }

    #[test]
    fn transport_accepts_zero_timeout_config() {
        let transport = JitoJsonRpcTransport::with_config(JitoTransportConfig {
            endpoint: JitoBlockEngineEndpoint::default(),
            request_timeout: Duration::ZERO,
        });
        assert!(transport.is_ok());
    }

    #[test]
    fn regional_endpoint_uses_documented_slug() {
        let endpoint = JitoBlockEngineEndpoint::mainnet_region(JitoBlockEngineRegion::Frankfurt);

        assert_eq!(
            endpoint.as_url(),
            "https://frankfurt.mainnet.block-engine.jito.wtf"
        );
    }

    #[tokio::test]
    async fn jito_transport_rejects_redirects() {
        let parsed_url = Url::parse(
            &spawn_http_response_server(
                "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://127.0.0.1/\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    .to_owned(),
            )
            .await,
        );
        assert!(parsed_url.is_ok());
        let parsed_url = parsed_url.unwrap_or_else(|error| panic!("{error}"));
        let transport =
            JitoJsonRpcTransport::with_endpoint(JitoBlockEngineEndpoint::custom(parsed_url));
        assert!(transport.is_ok());
        let transport = transport.unwrap_or_else(|error| panic!("{error}"));

        let error = transport
            .submit_jito(&[1, 2, 3], &JitoSubmitConfig::default())
            .await;
        assert!(error.is_err());
        let error = match error {
            Ok(_response) => panic!("redirect should fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("redirect"));
    }

    #[tokio::test]
    async fn jito_transport_rejects_oversized_responses() {
        let endpoint = spawn_http_response_server(format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            MAX_JITO_SUBMIT_RESPONSE_BYTES.saturating_add(1)
        ))
        .await;
        let parsed_url = Url::parse(&endpoint);
        assert!(parsed_url.is_ok());
        let parsed_url = parsed_url.unwrap_or_else(|error| panic!("{error}"));
        let transport =
            JitoJsonRpcTransport::with_endpoint(JitoBlockEngineEndpoint::custom(parsed_url));
        assert!(transport.is_ok());
        let transport = transport.unwrap_or_else(|error| panic!("{error}"));

        let error = transport
            .submit_jito(&[1, 2, 3], &JitoSubmitConfig::default())
            .await;
        assert!(error.is_err());
        let error = match error {
            Ok(_response) => panic!("oversized body should fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("exceeded max size"));
    }
}
