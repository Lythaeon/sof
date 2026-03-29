//! Submission module unit tests.

#![allow(clippy::indexing_slicing, clippy::panic)]

use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use async_trait::async_trait;
use sof_types::SignatureBytes;
use solana_keypair::Keypair;
use solana_signature::Signature;
use solana_signer::Signer;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{sleep, timeout},
};

use super::*;
use crate::{
    builder::TxBuilder,
    providers::{LeaderTarget, StaticLeaderProvider, StaticRecentBlockhashProvider},
    routing::RoutingPolicy,
};

/// Mock RPC transport with configurable response.
#[derive(Debug)]
struct MockRpcTransport {
    /// Return value to use.
    result: Result<String, SubmitTransportError>,
    /// Number of submit calls.
    calls: AtomicU64,
}

/// Mock Jito transport with configurable response.
#[derive(Debug)]
struct MockJitoTransport {
    /// Return value to use.
    result: Result<JitoSubmitResponse, SubmitTransportError>,
    /// Number of submit calls.
    calls: AtomicU64,
}

/// Mock RPC transport that waits before returning.
#[derive(Debug)]
struct DelayedRpcTransport {
    /// Return value to use.
    result: Result<String, SubmitTransportError>,
    /// Artificial delay before returning.
    delay: Duration,
    /// Number of submit calls.
    calls: AtomicU64,
}

/// Mock Jito transport that waits before returning.
#[derive(Debug)]
struct DelayedJitoTransport {
    /// Return value to use.
    result: Result<JitoSubmitResponse, SubmitTransportError>,
    /// Artificial delay before returning.
    delay: Duration,
    /// Number of submit calls.
    calls: AtomicU64,
}

#[async_trait]
impl RpcSubmitTransport for MockRpcTransport {
    async fn submit_rpc(
        &self,
        _tx_bytes: &[u8],
        _config: &RpcSubmitConfig,
    ) -> Result<String, SubmitTransportError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.result.clone()
    }
}

#[async_trait]
impl JitoSubmitTransport for MockJitoTransport {
    async fn submit_jito(
        &self,
        _tx_bytes: &[u8],
        _config: &JitoSubmitConfig,
    ) -> Result<JitoSubmitResponse, SubmitTransportError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.result.clone()
    }
}

#[async_trait]
impl RpcSubmitTransport for DelayedRpcTransport {
    async fn submit_rpc(
        &self,
        _tx_bytes: &[u8],
        _config: &RpcSubmitConfig,
    ) -> Result<String, SubmitTransportError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        sleep(self.delay).await;
        self.result.clone()
    }
}

#[async_trait]
impl JitoSubmitTransport for DelayedJitoTransport {
    async fn submit_jito(
        &self,
        _tx_bytes: &[u8],
        _config: &JitoSubmitConfig,
    ) -> Result<JitoSubmitResponse, SubmitTransportError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        sleep(self.delay).await;
        self.result.clone()
    }
}

/// Mock direct transport with configurable response.
#[derive(Debug)]
struct MockDirectTransport {
    /// Return value to use.
    result: Result<LeaderTarget, SubmitTransportError>,
    /// Number of submit calls.
    calls: AtomicU64,
}

#[async_trait]
impl DirectSubmitTransport for MockDirectTransport {
    async fn submit_direct(
        &self,
        _tx_bytes: &[u8],
        _targets: &[LeaderTarget],
        _policy: RoutingPolicy,
        _config: &DirectSubmitConfig,
    ) -> Result<LeaderTarget, SubmitTransportError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.result.clone()
    }
}

/// Mock direct transport that returns responses in sequence.
#[derive(Debug)]
struct SequencedDirectTransport {
    /// Ordered responses per call.
    results: Vec<Result<LeaderTarget, SubmitTransportError>>,
    /// Number of submit calls.
    calls: AtomicU64,
}

/// Mock toxic-flow source with a fixed snapshot.
#[derive(Debug)]
struct MockFlowSafetySource {
    /// Snapshot returned to the submit client.
    snapshot: TxFlowSafetySnapshot,
}

impl TxFlowSafetySource for MockFlowSafetySource {
    fn toxic_flow_snapshot(&self) -> TxFlowSafetySnapshot {
        self.snapshot.clone()
    }
}

/// Recording outcome reporter used by guard-path tests.
#[derive(Debug, Default)]
struct RecordingOutcomeReporter {
    /// Recorded outcomes in call order.
    outcomes: Mutex<Vec<TxSubmitOutcome>>,
}

impl TxSubmitOutcomeReporter for RecordingOutcomeReporter {
    fn record_outcome(&self, outcome: &TxSubmitOutcome) {
        self.outcomes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(outcome.clone());
    }
}

#[async_trait]
impl DirectSubmitTransport for SequencedDirectTransport {
    async fn submit_direct(
        &self,
        _tx_bytes: &[u8],
        _targets: &[LeaderTarget],
        _policy: RoutingPolicy,
        _config: &DirectSubmitConfig,
    ) -> Result<LeaderTarget, SubmitTransportError> {
        let calls = self.calls.fetch_add(1, Ordering::Relaxed);
        let call_index = calls as usize;
        let response = self
            .results
            .get(call_index)
            .or_else(|| self.results.last())
            .cloned();
        response.unwrap_or_else(|| {
            Err(SubmitTransportError::Failure {
                message: "no response configured".to_owned(),
            })
        })
    }
}

/// Builds one signed transfer transaction for tests.
fn signed_transfer_bytes() -> (Vec<u8>, Signature) {
    let payer = Keypair::new();
    let recipient = Keypair::new();
    let tx_result = TxBuilder::new(payer.pubkey())
        .add_instruction(solana_system_interface::instruction::transfer(
            &payer.pubkey(),
            &recipient.pubkey(),
            1,
        ))
        .build_and_sign([9_u8; 32], &[&payer]);

    assert!(tx_result.is_ok());
    let mut bytes = Vec::new();
    let mut signature = Signature::default();
    if let Ok(tx) = tx_result {
        let first = tx.signatures.first();
        assert!(first.is_some());
        if let Some(first) = first {
            signature = *first;
        }
        let encoded_result = bincode::serialize(&tx);
        assert!(encoded_result.is_ok());
        if let Ok(encoded) = encoded_result {
            bytes = encoded;
        }
    }
    (bytes, signature)
}

/// Returns a static leader target.
fn target(port: u16) -> LeaderTarget {
    LeaderTarget::new(None, SocketAddr::from(([127, 0, 0, 1], port)))
}

/// Decodes the first short-vec length prefix in a serialized transaction.
fn decode_short_vec_len(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut value = 0_usize;
    let mut shift = 0_u32;
    for (idx, byte) in bytes.iter().copied().take(3).enumerate() {
        value |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some((value, idx.saturating_add(1)));
        }
        shift = shift.saturating_add(7);
    }
    None
}

/// Rewrites the first signature bytes so repeated profile iterations do not trip dedupe.
fn rewrite_first_signature(bytes: &mut [u8], seed: u64) {
    const BYTE_SHIFTS: [u32; 64] = [
        0, 8, 16, 24, 32, 40, 48, 56, 0, 8, 16, 24, 32, 40, 48, 56, 0, 8, 16, 24, 32, 40, 48, 56,
        0, 8, 16, 24, 32, 40, 48, 56, 0, 8, 16, 24, 32, 40, 48, 56, 0, 8, 16, 24, 32, 40, 48, 56,
        0, 8, 16, 24, 32, 40, 48, 56, 0, 8, 16, 24, 32, 40, 48, 56,
    ];
    let decoded = decode_short_vec_len(bytes);
    assert!(decoded.is_some());
    let (signature_count, offset) = decoded.unwrap_or((0, 0));
    assert!(signature_count > 0);
    let end = offset.saturating_add(64);
    assert!(bytes.get(offset..end).is_some());
    for (idx, byte) in bytes[offset..end].iter_mut().enumerate() {
        let shift = BYTE_SHIFTS[idx];
        let lane = ((seed >> shift) & 0xff) as u8;
        *byte = lane.wrapping_add(idx as u8);
    }
}

/// Clones one prebuilt serialized transaction and rewrites the first signature for uniqueness.
fn profiled_tx_bytes(base_bytes: &[u8], seed: u64) -> Vec<u8> {
    let mut bytes = base_bytes.to_vec();
    rewrite_first_signature(&mut bytes, seed);
    bytes
}

#[tokio::test]
async fn rpc_only_uses_rpc_transport() {
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let direct = Arc::new(MockDirectTransport {
        result: Ok(target(9001)),
        calls: AtomicU64::new(0),
    });
    let jito = Arc::new(MockJitoTransport {
        result: Ok(JitoSubmitResponse {
            transaction_signature: Some("jito-signature".to_owned()),
            bundle_id: None,
        }),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([9_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(Some(target(9001)), Vec::new())),
    )
    .with_rpc_transport(rpc.clone())
    .with_direct_transport(direct.clone())
    .with_jito_transport(jito.clone());

    let (bytes, signature) = signed_transfer_bytes();
    let result = client
        .submit_signed(
            SignedTx::VersionedTransactionBytes(bytes),
            SubmitMode::RpcOnly,
        )
        .await;

    assert!(result.is_ok());
    if let Ok(result) = result {
        assert_eq!(
            result.signature,
            Some(SignatureBytes::from_solana(signature))
        );
        assert_eq!(result.rpc_signature, Some("rpc-signature".to_owned()));
        assert_eq!(result.jito_signature, None);
        assert_eq!(result.jito_bundle_id, None);
        assert_eq!(result.direct_target, None);
        assert_eq!(result.legacy_mode, Some(SubmitMode::RpcOnly));
        assert_eq!(result.plan, SubmitPlan::rpc_only());
        assert!(!result.used_fallback_route);
    }

    let rpc_calls = rpc.calls.load(Ordering::Relaxed);
    let direct_calls = direct.calls.load(Ordering::Relaxed);
    let jito_calls = jito.calls.load(Ordering::Relaxed);
    assert_eq!(rpc_calls, 1);
    assert_eq!(direct_calls, 0);
    assert_eq!(jito_calls, 0);
}

#[tokio::test]
async fn builder_allows_signed_rpc_submit_without_blockhash_provider() {
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::builder()
        .with_rpc_transport(rpc.clone())
        .build();

    let (bytes, signature) = signed_transfer_bytes();
    let result = client
        .submit_signed(
            SignedTx::VersionedTransactionBytes(bytes),
            SubmitMode::RpcOnly,
        )
        .await;

    assert!(result.is_ok());
    if let Ok(result) = result {
        assert_eq!(
            result.signature,
            Some(SignatureBytes::from_solana(signature))
        );
        assert_eq!(result.rpc_signature, Some("rpc-signature".to_owned()));
    }
    assert_eq!(rpc.calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn jito_only_uses_jito_transport() {
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let direct = Arc::new(MockDirectTransport {
        result: Ok(target(9006)),
        calls: AtomicU64::new(0),
    });
    let jito = Arc::new(MockJitoTransport {
        result: Ok(JitoSubmitResponse {
            transaction_signature: Some("jito-signature".to_owned()),
            bundle_id: None,
        }),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([9_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(Some(target(9006)), Vec::new())),
    )
    .with_rpc_transport(rpc.clone())
    .with_direct_transport(direct.clone())
    .with_jito_transport(jito.clone());

    let (bytes, signature) = signed_transfer_bytes();
    let result = client
        .submit_signed(
            SignedTx::VersionedTransactionBytes(bytes),
            SubmitMode::JitoOnly,
        )
        .await;

    assert!(result.is_ok());
    if let Ok(result) = result {
        assert_eq!(
            result.signature,
            Some(SignatureBytes::from_solana(signature))
        );
        assert_eq!(result.rpc_signature, None);
        assert_eq!(result.jito_signature, Some("jito-signature".to_owned()));
        assert_eq!(result.jito_bundle_id, None);
        assert_eq!(result.direct_target, None);
        assert_eq!(result.legacy_mode, Some(SubmitMode::JitoOnly));
        assert_eq!(result.plan, SubmitPlan::jito_only());
        assert!(!result.used_fallback_route);
    }

    let rpc_calls = rpc.calls.load(Ordering::Relaxed);
    let direct_calls = direct.calls.load(Ordering::Relaxed);
    let jito_calls = jito.calls.load(Ordering::Relaxed);
    assert_eq!(rpc_calls, 0);
    assert_eq!(direct_calls, 0);
    assert_eq!(jito_calls, 1);
}

#[tokio::test]
async fn jito_only_accepts_bundle_id_from_grpc_transport() {
    let jito = Arc::new(MockJitoTransport {
        result: Ok(JitoSubmitResponse {
            transaction_signature: None,
            bundle_id: Some("bundle-uuid".to_owned()),
        }),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([17_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(Some(target(9007)), Vec::new())),
    )
    .with_jito_transport(jito.clone());

    let (bytes, signature) = signed_transfer_bytes();
    let result = client
        .submit_signed(
            SignedTx::VersionedTransactionBytes(bytes),
            SubmitMode::JitoOnly,
        )
        .await;

    assert!(result.is_ok());
    if let Ok(result) = result {
        assert_eq!(
            result.signature,
            Some(SignatureBytes::from_solana(signature))
        );
        assert_eq!(result.rpc_signature, None);
        assert_eq!(result.jito_signature, None);
        assert_eq!(result.jito_bundle_id, Some("bundle-uuid".to_owned()));
        assert_eq!(result.direct_target, None);
        assert_eq!(result.legacy_mode, Some(SubmitMode::JitoOnly));
        assert_eq!(result.plan, SubmitPlan::jito_only());
        assert!(!result.used_fallback_route);
    }

    assert_eq!(jito.calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn rpc_only_constructor_uses_rpc_for_blockhash_and_submit() {
    let blockhash = bs58::encode([31_u8; 32]).into_string();
    let listener = TcpListener::bind("127.0.0.1:0").await;
    assert!(listener.is_ok());
    let listener = listener.unwrap_or_else(|error| panic!("{error}"));
    let addr = listener.local_addr();
    assert!(addr.is_ok());
    let addr = addr.unwrap_or_else(|error| panic!("{error}"));

    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let accepted = listener.accept().await;
            assert!(accepted.is_ok());
            let (mut stream, _) = accepted.unwrap_or_else(|error| panic!("{error}"));
            let mut buffer = [0_u8; 8192];
            let read = stream.read(&mut buffer).await;
            assert!(read.is_ok());
            let request = String::from_utf8_lossy(&buffer[..read.unwrap_or(0)]);
            let body = if request.contains("getLatestBlockhash") {
                format!(
                    "{{\"jsonrpc\":\"2.0\",\"result\":{{\"value\":{{\"blockhash\":\"{blockhash}\"}}}},\"id\":1}}"
                )
            } else {
                assert!(request.contains("sendTransaction"));
                "{\"jsonrpc\":\"2.0\",\"result\":\"rpc-signature\",\"id\":1}".to_owned()
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let write = stream.write_all(response.as_bytes()).await;
            assert!(write.is_ok());
        }
    });

    let client = TxSubmitClient::rpc_only(format!("http://{addr}"));
    assert!(client.is_ok());
    let mut client = client.unwrap_or_else(|error| panic!("{error}"));
    let refreshed = client.refresh_latest_blockhash_bytes().await;
    assert_eq!(refreshed, Ok(Some([31_u8; 32])));

    let payer = Keypair::new();
    let recipient = Keypair::new();
    let builder = TxBuilder::new(payer.pubkey()).add_instruction(
        solana_system_interface::instruction::transfer(&payer.pubkey(), &recipient.pubkey(), 1),
    );
    let tx = builder.build_and_sign([31_u8; 32], &[&payer]);
    assert!(tx.is_ok());
    let tx = tx.unwrap_or_else(|error| panic!("{error}"));
    let tx_bytes = bincode::serialize(&tx);
    assert!(tx_bytes.is_ok());
    let result = client
        .submit_signed(
            SignedTx::VersionedTransactionBytes(tx_bytes.unwrap_or_default()),
            SubmitMode::RpcOnly,
        )
        .await;

    assert!(result.is_ok());
    if let Ok(result) = result {
        assert_eq!(result.rpc_signature, Some("rpc-signature".to_owned()));
        assert_eq!(result.direct_target, None);
    }

    let joined = server.await;
    assert!(joined.is_ok());
}

#[tokio::test]
async fn builder_rpc_defaults_uses_rpc_for_blockhash_and_submit() {
    let blockhash = bs58::encode([41_u8; 32]).into_string();
    let listener = TcpListener::bind("127.0.0.1:0").await;
    assert!(listener.is_ok());
    let listener = listener.unwrap_or_else(|error| panic!("{error}"));
    let addr = listener.local_addr();
    assert!(addr.is_ok());
    let addr = addr.unwrap_or_else(|error| panic!("{error}"));

    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let accepted = listener.accept().await;
            assert!(accepted.is_ok());
            let (mut stream, _) = accepted.unwrap_or_else(|error| panic!("{error}"));
            let mut buffer = [0_u8; 8192];
            let read = stream.read(&mut buffer).await;
            assert!(read.is_ok());
            let request = String::from_utf8_lossy(&buffer[..read.unwrap_or(0)]);
            let body = if request.contains("getLatestBlockhash") {
                format!(
                    "{{\"jsonrpc\":\"2.0\",\"result\":{{\"value\":{{\"blockhash\":\"{blockhash}\"}}}},\"id\":1}}"
                )
            } else {
                assert!(request.contains("sendTransaction"));
                "{\"jsonrpc\":\"2.0\",\"result\":\"rpc-signature\",\"id\":1}".to_owned()
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let write = stream.write_all(response.as_bytes()).await;
            assert!(write.is_ok());
        }
    });

    let built = TxSubmitClient::builder().with_rpc_defaults(format!("http://{addr}"));
    assert!(built.is_ok());
    let mut client = built.unwrap_or_else(|error| panic!("{error}")).build();
    let refreshed = client.refresh_latest_blockhash_bytes().await;
    assert_eq!(refreshed, Ok(Some([41_u8; 32])));

    let payer = Keypair::new();
    let recipient = Keypair::new();
    let builder = TxBuilder::new(payer.pubkey()).add_instruction(
        solana_system_interface::instruction::transfer(&payer.pubkey(), &recipient.pubkey(), 1),
    );
    let tx = builder.build_and_sign([41_u8; 32], &[&payer]);
    assert!(tx.is_ok());
    let tx = tx.unwrap_or_else(|error| panic!("{error}"));
    let tx_bytes = bincode::serialize(&tx);
    assert!(tx_bytes.is_ok());
    let result = client
        .submit_signed(
            SignedTx::VersionedTransactionBytes(tx_bytes.unwrap_or_default()),
            SubmitMode::RpcOnly,
        )
        .await;

    assert!(result.is_ok());
    if let Ok(result) = result {
        assert_eq!(result.rpc_signature, Some("rpc-signature".to_owned()));
        assert_eq!(result.direct_target, None);
    }

    let joined = server.await;
    assert!(joined.is_ok());
}

#[tokio::test]
async fn direct_only_uses_direct_transport() {
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let direct_target = target(9011);
    let direct = Arc::new(MockDirectTransport {
        result: Ok(direct_target.clone()),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([10_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(
            Some(direct_target.clone()),
            Vec::new(),
        )),
    )
    .with_rpc_transport(rpc.clone())
    .with_direct_transport(direct.clone());

    let (bytes, _signature) = signed_transfer_bytes();
    let result = client
        .submit_signed(
            SignedTx::WireTransactionBytes(bytes),
            SubmitMode::DirectOnly,
        )
        .await;

    assert!(result.is_ok());
    if let Ok(result) = result {
        assert_eq!(result.direct_target, Some(direct_target));
        assert_eq!(result.rpc_signature, None);
        assert_eq!(result.jito_signature, None);
        assert_eq!(result.jito_bundle_id, None);
        assert_eq!(result.legacy_mode, Some(SubmitMode::DirectOnly));
        assert_eq!(result.plan, SubmitPlan::direct_only());
        assert!(!result.used_fallback_route);
    }

    let rpc_calls = rpc.calls.load(Ordering::Relaxed);
    let direct_calls = direct.calls.load(Ordering::Relaxed);
    assert_eq!(rpc_calls, 0);
    assert_eq!(direct_calls, 1);
}

#[tokio::test]
async fn hybrid_falls_back_to_rpc_when_direct_fails() {
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-fallback-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let direct = Arc::new(MockDirectTransport {
        result: Err(SubmitTransportError::Failure {
            message: "direct failed".to_owned(),
        }),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([11_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(Some(target(9021)), Vec::new())),
    )
    .with_rpc_transport(rpc.clone())
    .with_direct_transport(direct.clone());

    let (bytes, _signature) = signed_transfer_bytes();
    let result = client
        .submit_signed(
            SignedTx::VersionedTransactionBytes(bytes),
            SubmitMode::Hybrid,
        )
        .await;

    assert!(result.is_ok());
    if let Ok(result) = result {
        assert_eq!(result.direct_target, None);
        assert_eq!(
            result.rpc_signature,
            Some("rpc-fallback-signature".to_owned())
        );
        assert_eq!(result.jito_signature, None);
        assert_eq!(result.jito_bundle_id, None);
        assert_eq!(result.legacy_mode, Some(SubmitMode::Hybrid));
        assert_eq!(result.plan, SubmitPlan::hybrid());
        assert!(result.used_fallback_route);
    }

    let rpc_calls = rpc.calls.load(Ordering::Relaxed);
    let direct_calls = direct.calls.load(Ordering::Relaxed);
    assert_eq!(rpc_calls, 1);
    assert_eq!(direct_calls, 3);
}

#[tokio::test]
async fn hybrid_uses_second_direct_attempt_before_rpc() {
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-fallback-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let direct_target = target(9031);
    let direct = Arc::new(SequencedDirectTransport {
        results: vec![
            Err(SubmitTransportError::Failure {
                message: "first attempt failed".to_owned(),
            }),
            Ok(direct_target.clone()),
        ],
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([13_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(
            Some(direct_target.clone()),
            Vec::new(),
        )),
    )
    .with_rpc_transport(rpc.clone())
    .with_direct_transport(direct.clone());

    let (bytes, _signature) = signed_transfer_bytes();
    let result = client
        .submit_signed(
            SignedTx::VersionedTransactionBytes(bytes),
            SubmitMode::Hybrid,
        )
        .await;

    assert!(result.is_ok());
    if let Ok(result) = result {
        assert_eq!(result.direct_target, Some(direct_target));
        assert_eq!(result.rpc_signature, None);
        assert_eq!(result.jito_signature, None);
        assert_eq!(result.jito_bundle_id, None);
        assert_eq!(result.legacy_mode, Some(SubmitMode::Hybrid));
        assert_eq!(result.plan, SubmitPlan::hybrid());
        assert!(!result.used_fallback_route);
    }

    let direct_calls = direct.calls.load(Ordering::Relaxed);
    let rpc_calls = timeout(Duration::from_millis(100), async {
        loop {
            let calls = rpc.calls.load(Ordering::Relaxed);
            if calls > 0 {
                break calls;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(rpc_calls.is_ok());
    let rpc_calls = rpc_calls.unwrap_or(0);
    assert_eq!(direct_calls, 2);
    assert_eq!(rpc_calls, 1);
}

#[tokio::test]
async fn low_latency_reliability_uses_single_hybrid_attempt() {
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-fallback-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let direct = Arc::new(MockDirectTransport {
        result: Err(SubmitTransportError::Failure {
            message: "direct failed".to_owned(),
        }),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([14_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(Some(target(9041)), Vec::new())),
    )
    .with_reliability(SubmitReliability::LowLatency)
    .with_rpc_transport(rpc.clone())
    .with_direct_transport(direct.clone());

    let (bytes, _signature) = signed_transfer_bytes();
    let result = client
        .submit_signed(
            SignedTx::VersionedTransactionBytes(bytes),
            SubmitMode::Hybrid,
        )
        .await;

    assert!(result.is_ok());
    if let Ok(result) = result {
        assert_eq!(result.direct_target, None);
        assert_eq!(
            result.rpc_signature,
            Some("rpc-fallback-signature".to_owned())
        );
        assert_eq!(result.jito_signature, None);
        assert_eq!(result.jito_bundle_id, None);
        assert_eq!(result.legacy_mode, Some(SubmitMode::Hybrid));
        assert_eq!(result.plan, SubmitPlan::hybrid());
        assert!(result.used_fallback_route);
    }

    let rpc_calls = rpc.calls.load(Ordering::Relaxed);
    let direct_calls = direct.calls.load(Ordering::Relaxed);
    assert_eq!(direct_calls, 2);
    assert_eq!(rpc_calls, 1);
}

#[tokio::test]
async fn low_latency_hybrid_direct_success_skips_rpc_broadcast() {
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let direct_target = target(9042);
    let direct = Arc::new(MockDirectTransport {
        result: Ok(direct_target.clone()),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([15_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(
            Some(direct_target.clone()),
            Vec::new(),
        )),
    )
    .with_reliability(SubmitReliability::LowLatency)
    .with_rpc_transport(rpc.clone())
    .with_direct_transport(direct.clone());

    let (bytes, _signature) = signed_transfer_bytes();
    let result = client
        .submit_signed(
            SignedTx::VersionedTransactionBytes(bytes),
            SubmitMode::Hybrid,
        )
        .await;

    assert!(result.is_ok());
    if let Ok(result) = result {
        assert_eq!(result.direct_target, Some(direct_target));
        assert_eq!(result.rpc_signature, None);
        assert_eq!(result.jito_signature, None);
        assert_eq!(result.jito_bundle_id, None);
        assert_eq!(result.legacy_mode, Some(SubmitMode::Hybrid));
        assert_eq!(result.plan, SubmitPlan::hybrid());
        assert!(!result.used_fallback_route);
    }

    let rpc_calls = rpc.calls.load(Ordering::Relaxed);
    let direct_calls = direct.calls.load(Ordering::Relaxed);
    assert_eq!(direct_calls, 1);
    assert_eq!(rpc_calls, 0);
}

#[tokio::test]
async fn duplicate_signature_is_suppressed() {
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([12_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(None, Vec::new())),
    )
    .with_rpc_transport(rpc)
    .with_dedupe_ttl(Duration::from_secs(60));

    let (bytes, _signature) = signed_transfer_bytes();
    let first = client
        .submit_signed(
            SignedTx::VersionedTransactionBytes(bytes.clone()),
            SubmitMode::RpcOnly,
        )
        .await;
    assert!(first.is_ok());

    let second = client
        .submit_signed(
            SignedTx::VersionedTransactionBytes(bytes),
            SubmitMode::RpcOnly,
        )
        .await;
    assert!(second.is_err());
    assert!(matches!(second, Err(SubmitError::DuplicateSignature)));
}

#[tokio::test]
async fn toxic_flow_guard_rejects_reorg_risk_before_submit() {
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let reporter = Arc::new(RecordingOutcomeReporter::default());
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([21_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(Some(target(9051)), Vec::new())),
    )
    .with_rpc_transport(rpc.clone())
    .with_flow_safety_source(Arc::new(MockFlowSafetySource {
        snapshot: TxFlowSafetySnapshot {
            quality: TxFlowSafetyQuality::ReorgRisk,
            issues: vec![TxFlowSafetyIssue::ReorgRisk],
            current_state_version: Some(99),
            replay_recovery_pending: false,
        },
    }))
    .with_outcome_reporter(reporter.clone());

    let (bytes, _signature) = signed_transfer_bytes();
    let result = client
        .submit_signed(
            SignedTx::VersionedTransactionBytes(bytes),
            SubmitMode::RpcOnly,
        )
        .await;

    assert!(matches!(
        result,
        Err(SubmitError::ToxicFlow {
            reason: TxToxicFlowRejectionReason::UnsafeControlPlane {
                quality: TxFlowSafetyQuality::ReorgRisk
            }
        })
    ));
    assert_eq!(rpc.calls.load(Ordering::Relaxed), 0);
    assert_eq!(client.toxic_flow_telemetry().rejected_due_to_reorg_risk, 1);
    let outcomes = reporter
        .outcomes
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    assert_eq!(outcomes.len(), 1);
    let first = outcomes.first();
    assert!(first.is_some());
    if let Some(first) = first {
        assert_eq!(first.kind, TxSubmitOutcomeKind::RejectedDueToReorgRisk);
    }
}

#[tokio::test]
async fn toxic_flow_guard_rejects_state_drift_before_submit() {
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([22_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(Some(target(9052)), Vec::new())),
    )
    .with_rpc_transport(rpc.clone())
    .with_flow_safety_source(Arc::new(MockFlowSafetySource {
        snapshot: TxFlowSafetySnapshot {
            quality: TxFlowSafetyQuality::Stable,
            issues: Vec::new(),
            current_state_version: Some(200),
            replay_recovery_pending: false,
        },
    }));

    let (bytes, _signature) = signed_transfer_bytes();
    let result = client
        .submit_signed_with_context(
            SignedTx::VersionedTransactionBytes(bytes),
            SubmitMode::RpcOnly,
            TxSubmitContext {
                suppression_keys: Vec::new(),
                decision_state_version: Some(150),
                opportunity_created_at: None,
            },
        )
        .await;

    assert!(matches!(
        result,
        Err(SubmitError::ToxicFlow {
            reason: TxToxicFlowRejectionReason::StateDrift {
                drift: 50,
                max_allowed: 4
            }
        })
    ));
    assert_eq!(rpc.calls.load(Ordering::Relaxed), 0);
    assert_eq!(client.toxic_flow_telemetry().rejected_due_to_state_drift, 1);
}

#[tokio::test]
async fn toxic_flow_suppression_keys_block_repeated_opportunities() {
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([23_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(None, Vec::new())),
    )
    .with_rpc_transport(rpc.clone())
    .with_flow_safety_source(Arc::new(MockFlowSafetySource {
        snapshot: TxFlowSafetySnapshot {
            quality: TxFlowSafetyQuality::Stable,
            issues: Vec::new(),
            current_state_version: Some(300),
            replay_recovery_pending: false,
        },
    }));

    let key = TxSubmitSuppressionKey::Opportunity([7_u8; 32]);
    let context = TxSubmitContext {
        suppression_keys: vec![key.clone()],
        decision_state_version: Some(300),
        opportunity_created_at: Some(SystemTime::now()),
    };

    let (first_bytes, _) = signed_transfer_bytes();
    let first = client
        .submit_signed_with_context(
            SignedTx::VersionedTransactionBytes(first_bytes),
            SubmitMode::RpcOnly,
            context.clone(),
        )
        .await;
    assert!(first.is_ok());

    let (second_bytes, _) = signed_transfer_bytes();
    let second = client
        .submit_signed_with_context(
            SignedTx::VersionedTransactionBytes(second_bytes),
            SubmitMode::RpcOnly,
            context,
        )
        .await;

    assert!(matches!(
        second,
        Err(SubmitError::ToxicFlow {
            reason: TxToxicFlowRejectionReason::Suppressed
        })
    ));
    assert_eq!(client.toxic_flow_telemetry().suppressed_submissions, 1);
}

#[tokio::test]
async fn all_at_once_plan_submits_across_jito_and_direct() {
    let direct = Arc::new(MockDirectTransport {
        result: Ok(target(9001)),
        calls: AtomicU64::new(0),
    });
    let jito = Arc::new(DelayedJitoTransport {
        result: Ok(JitoSubmitResponse {
            transaction_signature: Some("jito-signature".to_owned()),
            bundle_id: Some("bundle-1".to_owned()),
        }),
        delay: Duration::from_millis(200),
        calls: AtomicU64::new(0),
    });
    let reporter = Arc::new(RecordingOutcomeReporter::default());
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([29_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(Some(target(9001)), Vec::new())),
    )
    .with_direct_transport(direct.clone())
    .with_jito_transport(jito.clone())
    .with_outcome_reporter(reporter.clone());

    let (bytes, signature) = signed_transfer_bytes();
    let result = timeout(
        Duration::from_millis(50),
        client.submit_signed_via(
            SignedTx::VersionedTransactionBytes(bytes),
            SubmitPlan::all_at_once(vec![SubmitRoute::Direct, SubmitRoute::Jito]),
        ),
    )
    .await;

    assert!(result.is_ok());
    if let Ok(Ok(result)) = result {
        assert_eq!(
            result.signature,
            Some(SignatureBytes::from_solana(signature))
        );
        assert_eq!(result.plan.strategy, SubmitStrategy::AllAtOnce);
        assert_eq!(
            result.plan.routes,
            vec![SubmitRoute::Direct, SubmitRoute::Jito]
        );
        assert_eq!(result.legacy_mode, None);
        assert_eq!(result.successful_routes, vec![SubmitRoute::Direct]);
        assert_eq!(result.first_success_route, Some(SubmitRoute::Direct));
        assert_eq!(result.direct_target, Some(target(9001)));
        assert_eq!(result.jito_signature, None);
        assert_eq!(result.jito_bundle_id, None);
        assert_eq!(result.rpc_signature, None);
        assert!(!result.used_fallback_route);
    }

    assert_eq!(direct.calls.load(Ordering::Relaxed), 1);
    assert_eq!(jito.calls.load(Ordering::Relaxed), 1);

    let late_result = timeout(Duration::from_millis(300), async {
        loop {
            let outcomes = reporter
                .outcomes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            if outcomes
                .iter()
                .any(|outcome| outcome.kind == TxSubmitOutcomeKind::JitoAccepted)
            {
                break outcomes;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(late_result.is_ok());
}

#[tokio::test]
async fn hybrid_direct_success_with_rpc_broadcast_returns_before_rpc_completes() {
    let direct_target = target(9043);
    let direct = Arc::new(MockDirectTransport {
        result: Ok(direct_target.clone()),
        calls: AtomicU64::new(0),
    });
    let rpc = Arc::new(DelayedRpcTransport {
        result: Ok("rpc-broadcast-signature".to_owned()),
        delay: Duration::from_millis(200),
        calls: AtomicU64::new(0),
    });
    let reporter = Arc::new(RecordingOutcomeReporter::default());
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([37_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(
            Some(direct_target.clone()),
            Vec::new(),
        )),
    )
    .with_direct_transport(direct.clone())
    .with_rpc_transport(rpc.clone())
    .with_outcome_reporter(reporter.clone())
    .with_direct_config(DirectSubmitConfig {
        hybrid_rpc_broadcast: true,
        agave_rebroadcast_enabled: false,
        latency_aware_targeting: false,
        ..DirectSubmitConfig::default()
    });

    let (bytes, _) = signed_transfer_bytes();
    let result = timeout(
        Duration::from_millis(50),
        client.submit_signed(
            SignedTx::VersionedTransactionBytes(bytes),
            SubmitMode::Hybrid,
        ),
    )
    .await;

    assert!(result.is_ok());
    if let Ok(Ok(result)) = result {
        assert_eq!(result.first_success_route, Some(SubmitRoute::Direct));
        assert_eq!(result.successful_routes, vec![SubmitRoute::Direct]);
        assert_eq!(result.direct_target, Some(direct_target));
        assert_eq!(result.rpc_signature, None);
        assert!(!result.used_fallback_route);
    }

    let late_result = timeout(Duration::from_millis(300), async {
        loop {
            let outcomes = reporter
                .outcomes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            let saw_direct = outcomes
                .iter()
                .any(|outcome| outcome.kind == TxSubmitOutcomeKind::DirectAccepted);
            let saw_rpc = outcomes
                .iter()
                .any(|outcome| outcome.kind == TxSubmitOutcomeKind::RpcAccepted);
            if saw_direct && saw_rpc {
                break outcomes;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(late_result.is_ok());
}

#[tokio::test]
#[ignore]
async fn submit_rpc_only_profile_fixture() {
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([30_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(None, Vec::new())),
    )
    .with_rpc_transport(rpc.clone());

    let (base_bytes, _) = signed_transfer_bytes();
    let iterations = 5_000_u64;
    let start = Instant::now();
    for idx in 0..iterations {
        let bytes = profiled_tx_bytes(&base_bytes, idx);
        let result = client
            .submit_signed(
                SignedTx::VersionedTransactionBytes(bytes),
                SubmitMode::RpcOnly,
            )
            .await;
        assert!(result.is_ok());
    }
    println!("rpc_only_us={}", start.elapsed().as_micros());
}

#[tokio::test]
#[ignore]
async fn submit_jito_only_profile_fixture() {
    let jito = Arc::new(MockJitoTransport {
        result: Ok(JitoSubmitResponse {
            transaction_signature: Some("jito-signature".to_owned()),
            bundle_id: Some("bundle-1".to_owned()),
        }),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([31_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(None, Vec::new())),
    )
    .with_jito_transport(jito.clone());

    let (base_bytes, _) = signed_transfer_bytes();
    let iterations = 5_000_u64;
    let start = Instant::now();
    for idx in 0..iterations {
        let bytes = profiled_tx_bytes(&base_bytes, idx);
        let result = client
            .submit_signed(
                SignedTx::VersionedTransactionBytes(bytes),
                SubmitMode::JitoOnly,
            )
            .await;
        assert!(result.is_ok());
    }
    println!("jito_only_us={}", start.elapsed().as_micros());
}

#[tokio::test]
#[ignore]
async fn submit_direct_only_profile_fixture() {
    let direct_target = target(9101);
    let direct = Arc::new(MockDirectTransport {
        result: Ok(direct_target.clone()),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([32_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(
            Some(direct_target.clone()),
            Vec::new(),
        )),
    )
    .with_direct_transport(direct.clone());

    let (base_bytes, _) = signed_transfer_bytes();
    let iterations = 5_000_u64;
    let start = Instant::now();
    for idx in 0..iterations {
        let bytes = profiled_tx_bytes(&base_bytes, idx);
        let result = client
            .submit_signed(
                SignedTx::VersionedTransactionBytes(bytes),
                SubmitMode::DirectOnly,
            )
            .await;
        assert!(result.is_ok());
    }
    println!("direct_only_us={}", start.elapsed().as_micros());
}

#[tokio::test]
#[ignore]
async fn submit_hybrid_fallback_profile_fixture() {
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-fallback-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let direct = Arc::new(MockDirectTransport {
        result: Err(SubmitTransportError::Failure {
            message: "direct failed".to_owned(),
        }),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([33_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(Some(target(9102)), Vec::new())),
    )
    .with_rpc_transport(rpc.clone())
    .with_direct_transport(direct.clone())
    .with_direct_config(DirectSubmitConfig {
        direct_submit_attempts: 1,
        hybrid_direct_attempts: 1,
        rebroadcast_interval: Duration::from_nanos(1),
        agave_rebroadcast_enabled: false,
        hybrid_rpc_broadcast: false,
        latency_aware_targeting: false,
        ..DirectSubmitConfig::default()
    });

    let (base_bytes, _) = signed_transfer_bytes();
    let iterations = 3_000_u64;
    let start = Instant::now();
    for idx in 0..iterations {
        let bytes = profiled_tx_bytes(&base_bytes, idx);
        let result = client
            .submit_signed(
                SignedTx::VersionedTransactionBytes(bytes),
                SubmitMode::Hybrid,
            )
            .await;
        assert!(result.is_ok());
    }
    println!("hybrid_fallback_us={}", start.elapsed().as_micros());
}

#[tokio::test]
#[ignore]
async fn submit_all_at_once_profile_fixture() {
    let direct = Arc::new(MockDirectTransport {
        result: Ok(target(9103)),
        calls: AtomicU64::new(0),
    });
    let rpc = Arc::new(MockRpcTransport {
        result: Ok("rpc-signature".to_owned()),
        calls: AtomicU64::new(0),
    });
    let jito = Arc::new(MockJitoTransport {
        result: Ok(JitoSubmitResponse {
            transaction_signature: Some("jito-signature".to_owned()),
            bundle_id: Some("bundle-1".to_owned()),
        }),
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([34_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(Some(target(9103)), Vec::new())),
    )
    .with_direct_transport(direct.clone())
    .with_rpc_transport(rpc.clone())
    .with_jito_transport(jito.clone());

    let (base_bytes, _) = signed_transfer_bytes();
    let iterations = 3_000_u64;
    let start = Instant::now();
    for idx in 0..iterations {
        let bytes = profiled_tx_bytes(&base_bytes, idx);
        let result = client
            .submit_signed_via(
                SignedTx::VersionedTransactionBytes(bytes),
                SubmitPlan::all_at_once(vec![
                    SubmitRoute::Direct,
                    SubmitRoute::Rpc,
                    SubmitRoute::Jito,
                ]),
            )
            .await;
        assert!(result.is_ok());
    }
    println!("all_at_once_us={}", start.elapsed().as_micros());
}
