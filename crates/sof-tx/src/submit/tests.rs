//! Submission module unit tests.

use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use solana_keypair::Keypair;
use solana_signature::Signature;
use solana_signer::Signer;

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
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([9_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(Some(target(9001)), Vec::new())),
    )
    .with_rpc_transport(rpc.clone())
    .with_direct_transport(direct.clone());

    let (bytes, signature) = signed_transfer_bytes();
    let result = client
        .submit_signed(
            SignedTx::VersionedTransactionBytes(bytes),
            SubmitMode::RpcOnly,
        )
        .await;

    assert!(result.is_ok());
    if let Ok(result) = result {
        assert_eq!(result.signature, Some(signature));
        assert_eq!(result.rpc_signature, Some("rpc-signature".to_owned()));
        assert_eq!(result.direct_target, None);
        assert!(!result.used_rpc_fallback);
    }

    let rpc_calls = rpc.calls.load(Ordering::Relaxed);
    let direct_calls = direct.calls.load(Ordering::Relaxed);
    assert_eq!(rpc_calls, 1);
    assert_eq!(direct_calls, 0);
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
        assert!(!result.used_rpc_fallback);
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
        assert!(result.used_rpc_fallback);
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
        assert_eq!(
            result.rpc_signature,
            Some("rpc-fallback-signature".to_owned())
        );
        assert!(!result.used_rpc_fallback);
    }

    let rpc_calls = rpc.calls.load(Ordering::Relaxed);
    let direct_calls = direct.calls.load(Ordering::Relaxed);
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
        assert!(result.used_rpc_fallback);
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
        assert!(!result.used_rpc_fallback);
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
