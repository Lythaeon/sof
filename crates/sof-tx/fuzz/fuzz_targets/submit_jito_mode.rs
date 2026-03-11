#![no_main]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use libfuzzer_sys::fuzz_target;
use solana_keypair::Keypair;
use solana_signer::Signer;
use sof_tx::{
    JitoSubmitConfig, SignedTx, SubmitMode, SubmitTransportError, TxBuilder, TxSubmitClient,
    providers::{StaticLeaderProvider, StaticRecentBlockhashProvider},
    submit::JitoSubmitTransport,
};

#[derive(Debug)]
struct FuzzJitoTransport {
    should_fail: bool,
    calls: AtomicU64,
}

#[async_trait]
impl JitoSubmitTransport for FuzzJitoTransport {
    async fn submit_jito(
        &self,
        tx_bytes: &[u8],
        config: &JitoSubmitConfig,
    ) -> Result<String, SubmitTransportError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.should_fail {
            return Err(SubmitTransportError::Failure {
                message: format!(
                    "fuzz jito failure bundle_only={} len={}",
                    config.bundle_only,
                    tx_bytes.len()
                ),
            });
        }
        Ok(format!("sig-{}-{}", u8::from(config.bundle_only), tx_bytes.len()))
    }
}

fn maybe_valid_signed_tx(bytes: &[u8]) -> Vec<u8> {
    if bytes.first().copied().unwrap_or(0) & 1 == 0 {
        return bytes.to_vec();
    }

    let payer = Keypair::new();
    let recipient = Keypair::new();
    let mut builder = TxBuilder::new(payer.pubkey());
    if bytes.get(1).copied().unwrap_or(0) & 1 == 1 {
        builder = builder.with_compute_unit_limit(200_000);
    }
    if bytes.get(2).copied().unwrap_or(0) & 1 == 1 {
        builder = builder.with_priority_fee_micro_lamports(50_000);
    }
    let builder = builder.add_instruction(solana_system_interface::instruction::transfer(
        &payer.pubkey(),
        &recipient.pubkey(),
        1,
    ));

    match builder.build_and_sign([7_u8; 32], &[&payer]) {
        Ok(tx) => match bincode::serialize(&tx) {
            Ok(encoded) => encoded,
            Err(_) => bytes.to_vec(),
        },
        Err(_) => bytes.to_vec(),
    }
}

fuzz_target!(|bytes: &[u8]| {
    let tx_bytes = maybe_valid_signed_tx(bytes);
    let bundle_only = bytes.get(3).copied().unwrap_or(0) & 1 == 1;
    let should_fail = bytes.get(4).copied().unwrap_or(0) & 1 == 1;
    let signed_variant = if bytes.get(5).copied().unwrap_or(0) & 1 == 1 {
        SignedTx::WireTransactionBytes(tx_bytes.clone())
    } else {
        SignedTx::VersionedTransactionBytes(tx_bytes.clone())
    };

    let jito = Arc::new(FuzzJitoTransport {
        should_fail,
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([9_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(None, Vec::new())),
    )
    .with_jito_transport(jito.clone())
    .with_jito_config(JitoSubmitConfig { bundle_only });

    let runtime_result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    assert!(runtime_result.is_ok());
    let Some(runtime) = runtime_result.ok() else {
        return;
    };

    let result = runtime.block_on(client.submit_signed(signed_variant, SubmitMode::JitoOnly));
    let calls = jito.calls.load(Ordering::Relaxed);

    match result {
        Ok(result) => {
            assert!(calls <= 1);
            assert_eq!(result.mode, SubmitMode::JitoOnly);
            assert_eq!(result.rpc_signature, None);
            assert_eq!(result.direct_target, None);
            assert!(!result.used_rpc_fallback);
            assert!(result.jito_signature.is_some());
        }
        Err(error) => match error {
            sof_tx::SubmitError::DecodeSignedBytes { .. } => {
                assert_eq!(calls, 0);
            }
            sof_tx::SubmitError::Jito { .. } => {
                assert_eq!(calls, 1);
            }
            other => panic!("unexpected submit error: {other}"),
        },
    }
});
