//! Provider traits and basic in-memory adapters used by the transaction SDK.

use std::{
    net::SocketAddr,
    sync::{Arc, RwLock},
    time::Duration,
};

use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::from_slice as json_from_slice;
use sof_types::PubkeyBytes;

use crate::submit::SubmitTransportError;

/// Maximum HTTP body size accepted from `getLatestBlockhash` RPC responses.
const MAX_BLOCKHASH_RPC_RESPONSE_BYTES: usize = 64 * 1024;

/// One leader/validator target that can receive transactions directly.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct LeaderTarget {
    /// Optional validator identity.
    pub identity: Option<PubkeyBytes>,
    /// TPU/ingress socket used for direct submit.
    pub tpu_addr: SocketAddr,
}

impl LeaderTarget {
    /// Creates a leader target with optional identity.
    #[must_use]
    pub const fn new(identity: Option<PubkeyBytes>, tpu_addr: SocketAddr) -> Self {
        Self { identity, tpu_addr }
    }
}

/// Source of the latest recent blockhash bytes.
pub trait RecentBlockhashProvider: Send + Sync {
    /// Returns the newest blockhash bytes when available.
    fn latest_blockhash(&self) -> Option<[u8; 32]>;
}

/// RPC-backed blockhash provider settings.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RpcRecentBlockhashProviderConfig {
    /// HTTP timeout applied to blockhash requests.
    pub request_timeout: Duration,
}

impl Default for RpcRecentBlockhashProviderConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(10),
        }
    }
}

/// Cached recent-blockhash provider sourced from a Solana JSON-RPC endpoint.
#[derive(Debug, Clone)]
pub struct RpcRecentBlockhashProvider {
    /// Latest successfully fetched blockhash cached for synchronous readers.
    latest: Arc<RwLock<Option<[u8; 32]>>>,
    /// HTTP client reused across on-demand refreshes.
    client: reqwest::Client,
    /// Target JSON-RPC endpoint URL.
    rpc_url: String,
}

impl RpcRecentBlockhashProvider {
    /// Creates a provider backed by one RPC endpoint using on-demand refresh.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError`] when the HTTP client cannot be built.
    pub fn new(rpc_url: impl Into<String>) -> Result<Self, SubmitTransportError> {
        let config = RpcRecentBlockhashProviderConfig::default();
        Self::with_config(rpc_url, &config)
    }

    /// Creates a provider with explicit request settings.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError`] when the HTTP client cannot be built.
    pub fn with_config(
        rpc_url: impl Into<String>,
        config: &RpcRecentBlockhashProviderConfig,
    ) -> Result<Self, SubmitTransportError> {
        let rpc_url = rpc_url.into();
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .connect_timeout(config.request_timeout)
            .timeout(config.request_timeout)
            .build()
            .map_err(|error| SubmitTransportError::Config {
                message: error.to_string(),
            })?;
        let latest = Arc::new(RwLock::new(None));
        Ok(Self {
            latest,
            client,
            rpc_url,
        })
    }

    /// Forces one refresh against the configured RPC endpoint and returns the cached value.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError`] if the RPC request fails or the response is invalid.
    pub async fn refresh(&self) -> Result<[u8; 32], SubmitTransportError> {
        let blockhash = fetch_latest_blockhash(&self.client, &self.rpc_url).await?;
        let mut latest = self
            .latest
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *latest = Some(blockhash);
        Ok(blockhash)
    }
}

impl RecentBlockhashProvider for RpcRecentBlockhashProvider {
    fn latest_blockhash(&self) -> Option<[u8; 32]> {
        *self
            .latest
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Source of current/next leader targets.
pub trait LeaderProvider: Send + Sync {
    /// Returns the currently scheduled leader target.
    fn current_leader(&self) -> Option<LeaderTarget>;

    /// Returns up to `n` upcoming leader targets.
    fn next_leaders(&self, n: usize) -> Vec<LeaderTarget>;
}

/// In-memory blockhash provider for tests and static configurations.
#[derive(Debug, Clone)]
pub struct StaticRecentBlockhashProvider {
    /// Optional static blockhash bytes.
    value: Option<[u8; 32]>,
}

impl StaticRecentBlockhashProvider {
    /// Creates a provider with an optional static blockhash.
    #[must_use]
    pub const fn new(value: Option<[u8; 32]>) -> Self {
        Self { value }
    }
}

impl RecentBlockhashProvider for StaticRecentBlockhashProvider {
    fn latest_blockhash(&self) -> Option<[u8; 32]> {
        self.value
    }
}

/// In-memory leader provider for tests and static configurations.
#[derive(Debug, Clone, Default)]
pub struct StaticLeaderProvider {
    /// Optional current leader.
    current: Option<LeaderTarget>,
    /// Ordered next leaders.
    next: Vec<LeaderTarget>,
}

impl StaticLeaderProvider {
    /// Creates a static leader provider.
    #[must_use]
    pub const fn new(current: Option<LeaderTarget>, next: Vec<LeaderTarget>) -> Self {
        Self { current, next }
    }
}

impl LeaderProvider for StaticLeaderProvider {
    fn current_leader(&self) -> Option<LeaderTarget> {
        self.current.clone()
    }

    fn next_leaders(&self, n: usize) -> Vec<LeaderTarget> {
        self.next.iter().take(n).cloned().collect()
    }
}

/// Minimal JSON-RPC response envelope for `getLatestBlockhash`.
#[derive(Debug, Deserialize)]
struct LatestBlockhashRpcResponse {
    /// Successful RPC result payload.
    result: Option<LatestBlockhashResult>,
    /// Error payload returned by the RPC server.
    error: Option<JsonRpcError>,
}

/// Parsed `getLatestBlockhash` result object.
#[derive(Debug, Deserialize)]
struct LatestBlockhashResult {
    /// RPC value payload.
    value: LatestBlockhashValue,
}

/// JSON payload that carries one base58-encoded recent blockhash.
#[derive(Debug, Deserialize)]
struct LatestBlockhashValue {
    /// Base58-encoded recent blockhash string.
    blockhash: String,
}

/// Minimal JSON-RPC error object.
#[derive(Debug, Deserialize)]
struct JsonRpcError {
    /// Numeric JSON-RPC error code.
    code: i64,
    /// Human-readable error message.
    message: String,
}

/// JSON-RPC request envelope for `getLatestBlockhash`.
#[derive(Debug, Serialize)]
struct LatestBlockhashRequest<'request> {
    /// JSON-RPC protocol version.
    jsonrpc: &'request str,
    /// Caller-chosen request ID.
    id: u64,
    /// Requested RPC method.
    method: &'request str,
    /// Commitment config array.
    params: [LatestBlockhashRequestConfig<'request>; 1],
}

/// Commitment config payload for `getLatestBlockhash`.
#[derive(Debug, Serialize)]
struct LatestBlockhashRequestConfig<'request> {
    /// Commitment level requested from RPC.
    commitment: &'request str,
}

/// Fetches the latest recent blockhash from one Solana JSON-RPC endpoint.
async fn fetch_latest_blockhash(
    client: &reqwest::Client,
    rpc_url: &str,
) -> Result<[u8; 32], SubmitTransportError> {
    let payload = LatestBlockhashRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getLatestBlockhash",
        params: [LatestBlockhashRequestConfig {
            commitment: "processed",
        }],
    };
    let response = client
        .post(rpc_url)
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
    let response = response
        .error_for_status()
        .map_err(|error| SubmitTransportError::Failure {
            message: error.to_string(),
        })?;
    let response_body = read_http_response_bytes_bounded(response).await?;
    let parsed: LatestBlockhashRpcResponse =
        json_from_slice(&response_body).map_err(|error| SubmitTransportError::Failure {
            message: error.to_string(),
        })?;
    if let Some(result) = parsed.result {
        return parse_blockhash(&result.value.blockhash);
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

/// Reads one RPC response body while enforcing a fixed maximum byte budget.
async fn read_http_response_bytes_bounded(
    mut response: reqwest::Response,
) -> Result<Vec<u8>, SubmitTransportError> {
    if response
        .content_length()
        .is_some_and(|content_length| content_length > MAX_BLOCKHASH_RPC_RESPONSE_BYTES as u64)
    {
        return Err(SubmitTransportError::Failure {
            message: format!(
                "response body exceeded max size of {MAX_BLOCKHASH_RPC_RESPONSE_BYTES} bytes"
            ),
        });
    }

    let initial_capacity = response
        .content_length()
        .and_then(|content_length| usize::try_from(content_length).ok())
        .unwrap_or(0)
        .min(MAX_BLOCKHASH_RPC_RESPONSE_BYTES);
    let mut body = Vec::with_capacity(initial_capacity);
    while let Some(chunk) = response.chunk().await.map_err(|error| SubmitTransportError::Failure {
        message: error.to_string(),
    })? {
        let remaining = MAX_BLOCKHASH_RPC_RESPONSE_BYTES.saturating_sub(body.len());
        if chunk.len() > remaining {
            return Err(SubmitTransportError::Failure {
                message: format!(
                    "response body exceeded max size of {MAX_BLOCKHASH_RPC_RESPONSE_BYTES} bytes"
                ),
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Decodes one base58 blockhash string into the byte format used by `TxBuilder`.
fn parse_blockhash(blockhash: &str) -> Result<[u8; 32], SubmitTransportError> {
    let decoded =
        bs58::decode(blockhash)
            .into_vec()
            .map_err(|error| SubmitTransportError::Failure {
                message: format!("failed to decode recent blockhash: {error}"),
            })?;
    let bytes: [u8; 32] = decoded
        .try_into()
        .map_err(|_error| SubmitTransportError::Failure {
            message: "rpc blockhash did not decode to 32 bytes".to_owned(),
        })?;
    Ok(bytes)
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
    async fn rpc_recent_blockhash_provider_fetches_initial_value() {
        let expected = [9_u8; 32];
        let blockhash = bs58::encode(expected).into_string();
        let listener = TcpListener::bind("127.0.0.1:0").await;
        assert!(listener.is_ok());
        let listener = listener.unwrap_or_else(|error| panic!("{error}"));
        let addr = listener.local_addr();
        assert!(addr.is_ok());
        let addr = addr.unwrap_or_else(|error| panic!("{error}"));

        let server = tokio::spawn(async move {
            let accepted = listener.accept().await;
            assert!(accepted.is_ok());
            let (mut stream, _) = accepted.unwrap_or_else(|error| panic!("{error}"));
            let mut buffer = [0_u8; 4096];
            let read = stream.read(&mut buffer).await;
            assert!(read.is_ok());
            let request = String::from_utf8_lossy(&buffer[..read.unwrap_or(0)]);
            assert!(request.contains("getLatestBlockhash"));
            let body = format!(
                "{{\"jsonrpc\":\"2.0\",\"result\":{{\"value\":{{\"blockhash\":\"{blockhash}\"}}}},\"id\":1}}"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let write = stream.write_all(response.as_bytes()).await;
            assert!(write.is_ok());
        });

        let provider = RpcRecentBlockhashProvider::with_config(
            format!("http://{addr}"),
            &RpcRecentBlockhashProviderConfig::default(),
        );
        assert!(provider.is_ok());
        let provider = provider.unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(provider.latest_blockhash(), None);
        let refreshed = provider.refresh().await;
        assert!(refreshed.is_ok());
        assert_eq!(refreshed.unwrap_or([0_u8; 32]), expected);
        assert_eq!(provider.latest_blockhash(), Some(expected));

        let joined = server.await;
        assert!(joined.is_ok());
    }

    #[tokio::test]
    async fn rpc_recent_blockhash_provider_rejects_redirects() {
        let target = spawn_http_response_server(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                .to_owned(),
        )
        .await;
        let endpoint = spawn_http_response_server(format!(
            "HTTP/1.1 307 Temporary Redirect\r\nlocation: {target}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        ))
        .await;

        let provider = RpcRecentBlockhashProvider::new(endpoint);
        assert!(provider.is_ok());
        let provider = provider.unwrap_or_else(|error| panic!("{error}"));
        let error = match provider.refresh().await {
            Ok(_blockhash) => panic!("redirect should fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("redirect"));
    }

    #[tokio::test]
    async fn rpc_recent_blockhash_provider_rejects_oversized_responses() {
        let endpoint = spawn_http_response_server(format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            MAX_BLOCKHASH_RPC_RESPONSE_BYTES.saturating_add(1)
        ))
        .await;

        let provider = RpcRecentBlockhashProvider::new(endpoint);
        assert!(provider.is_ok());
        let provider = provider.unwrap_or_else(|error| panic!("{error}"));
        let error = match provider.refresh().await {
            Ok(_blockhash) => panic!("oversized body should fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("exceeded max size"));
    }
}
