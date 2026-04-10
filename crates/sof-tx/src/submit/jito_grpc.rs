//! Jito searcher gRPC bundle transport implementation.

use std::time::Duration;

use async_trait::async_trait;
use sof_support::time_support::nonzero_duration_or;
use tonic::{
    Request, Status,
    client::Grpc,
    codegen::http::uri::PathAndQuery,
    transport::{Channel, ClientTlsConfig, Endpoint},
};
use tonic_prost::ProstCodec;

use super::{
    JitoBlockEngineEndpoint, JitoSubmitConfig, JitoSubmitResponse, JitoSubmitTransport,
    JitoTransportConfig, SubmitTransportError,
};

/// Default timeout used for Jito gRPC connect and request deadlines.
const DEFAULT_JITO_GRPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Minimal shared header message for Jito bundle requests.
#[derive(Clone, PartialEq, ::prost::Message)]
struct SharedHeader {}

/// Minimal packet flags message for Jito bundle requests.
#[derive(Clone, PartialEq, ::prost::Message)]
struct PacketFlags {}

/// Packet metadata accepted by Jito's searcher service.
#[derive(Clone, PartialEq, ::prost::Message)]
struct PacketMeta {
    /// Serialized packet length in bytes.
    #[prost(uint64, tag = "1")]
    size: u64,
    /// Sender IP string; empty when not provided.
    #[prost(string, tag = "2")]
    addr: String,
    /// Sender port; zero when not provided.
    #[prost(uint32, tag = "3")]
    port: u32,
    /// Optional packet flags.
    #[prost(message, optional, tag = "4")]
    flags: Option<PacketFlags>,
    /// Sender stake when known.
    #[prost(uint64, tag = "5")]
    sender_stake: u64,
}

/// Packet payload accepted by Jito's searcher service.
#[derive(Clone, PartialEq, ::prost::Message)]
struct Packet {
    /// Raw serialized wire transaction bytes.
    #[prost(bytes = "vec", tag = "1")]
    data: Vec<u8>,
    /// Packet metadata.
    #[prost(message, optional, tag = "2")]
    meta: Option<PacketMeta>,
}

/// One bundle request containing packetized transactions.
#[derive(Clone, PartialEq, ::prost::Message)]
struct Bundle {
    /// Optional bundle header.
    #[prost(message, optional, tag = "2")]
    header: Option<SharedHeader>,
    /// Transactions encoded as packets.
    #[prost(message, repeated, tag = "3")]
    packets: Vec<Packet>,
}

/// Searcher gRPC request for bundle submission.
#[derive(Clone, PartialEq, ::prost::Message)]
struct SendBundleRequest {
    /// Bundle payload submitted to the block engine.
    #[prost(message, optional, tag = "1")]
    bundle: Option<Bundle>,
}

/// Searcher gRPC response for bundle submission.
#[derive(Clone, PartialEq, ::prost::Message)]
struct SendBundleResponse {
    /// Server-generated bundle UUID.
    #[prost(string, tag = "1")]
    uuid: String,
}

/// Jito searcher gRPC transport that submits a single transaction as a one-packet bundle.
#[derive(Debug, Clone)]
pub struct JitoGrpcTransport {
    /// gRPC channel used for searcher requests.
    channel: Channel,
}

impl JitoGrpcTransport {
    /// Creates a gRPC transport against the default Jito mainnet endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError::Config`] when endpoint construction fails.
    pub fn new() -> Result<Self, SubmitTransportError> {
        Self::with_config(JitoTransportConfig::default())
    }

    /// Creates a gRPC transport with explicit transport settings.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError::Config`] when endpoint construction fails.
    pub fn with_config(config: JitoTransportConfig) -> Result<Self, SubmitTransportError> {
        let JitoTransportConfig {
            endpoint: block_engine_endpoint,
            request_timeout,
        } = config;
        let request_timeout =
            nonzero_duration_or(request_timeout, DEFAULT_JITO_GRPC_REQUEST_TIMEOUT);
        let endpoint_url = block_engine_endpoint.as_url().to_owned();
        let mut transport_endpoint =
            Endpoint::from_shared(endpoint_url.clone()).map_err(|error| {
                SubmitTransportError::Config {
                    message: error.to_string(),
                }
            })?;
        transport_endpoint = transport_endpoint
            .connect_timeout(request_timeout)
            .timeout(request_timeout)
            .tcp_nodelay(true);
        if endpoint_url.starts_with("https://") {
            transport_endpoint = transport_endpoint
                .tls_config(ClientTlsConfig::new().with_webpki_roots())
                .map_err(|error| SubmitTransportError::Config {
                    message: error.to_string(),
                })?;
        }
        Ok(Self {
            channel: transport_endpoint.connect_lazy(),
        })
    }

    /// Creates a gRPC transport for one typed endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitTransportError::Config`] when endpoint construction fails.
    pub fn with_endpoint(endpoint: JitoBlockEngineEndpoint) -> Result<Self, SubmitTransportError> {
        Self::with_config(JitoTransportConfig {
            endpoint,
            ..JitoTransportConfig::default()
        })
    }

    /// Converts one wire transaction into the packet shape expected by Jito bundle gRPC.
    fn packet_from_tx(tx_bytes: &[u8]) -> Packet {
        Packet {
            data: tx_bytes.to_vec(),
            meta: Some(PacketMeta {
                size: tx_bytes.len() as u64,
                addr: String::new(),
                port: 0,
                flags: None,
                sender_stake: 0,
            }),
        }
    }

    /// Builds the unary gRPC request for one single-transaction bundle.
    fn send_bundle_request(tx_bytes: &[u8]) -> SendBundleRequest {
        SendBundleRequest {
            bundle: Some(Bundle {
                header: None,
                packets: vec![Self::packet_from_tx(tx_bytes)],
            }),
        }
    }

    /// Executes the unary `SendBundle` RPC against the configured searcher endpoint.
    async fn send_bundle(&self, request: SendBundleRequest) -> Result<SendBundleResponse, Status> {
        let mut grpc = Grpc::new(self.channel.clone());
        grpc.ready()
            .await
            .map_err(|error| Status::unavailable(error.to_string()))?;
        let response = grpc
            .unary(
                Request::new(request),
                PathAndQuery::from_static("/searcher.SearcherService/SendBundle"),
                ProstCodec::<SendBundleRequest, SendBundleResponse>::default(),
            )
            .await?;
        Ok(response.into_inner())
    }
}

#[async_trait]
impl JitoSubmitTransport for JitoGrpcTransport {
    async fn submit_jito(
        &self,
        tx_bytes: &[u8],
        _config: &JitoSubmitConfig,
    ) -> Result<JitoSubmitResponse, SubmitTransportError> {
        let response = self.send_bundle(Self::send_bundle_request(tx_bytes)).await;
        let response = response.map_err(|error| SubmitTransportError::Failure {
            message: error.to_string(),
        })?;

        Ok(JitoSubmitResponse {
            transaction_signature: None,
            bundle_id: Some(response.uuid),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_from_tx_uses_wire_length() {
        let packet = JitoGrpcTransport::packet_from_tx(&[1, 2, 3, 4]);

        assert_eq!(packet.data, vec![1, 2, 3, 4]);
        assert_eq!(packet.meta.as_ref().map(|meta| meta.size), Some(4));
    }

    #[test]
    fn send_bundle_request_wraps_single_packet() {
        let request = JitoGrpcTransport::send_bundle_request(&[9, 8, 7]);

        let packet_count = request
            .bundle
            .as_ref()
            .map(|bundle| bundle.packets.len())
            .unwrap_or_default();
        assert_eq!(packet_count, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn jito_grpc_transport_accepts_zero_timeout_config() {
        let transport = JitoGrpcTransport::with_config(JitoTransportConfig {
            endpoint: JitoBlockEngineEndpoint::default(),
            request_timeout: Duration::ZERO,
        });
        assert!(transport.is_ok());
    }
}
