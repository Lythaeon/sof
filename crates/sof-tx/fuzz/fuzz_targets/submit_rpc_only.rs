#![no_main]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use libfuzzer_sys::fuzz_target;
use solana_keypair::Keypair;
use solana_signer::Signer;
use sof_solana_compat::TxBuilder;
use sof_tx::{
    RpcSubmitConfig, SignedTx, SubmitMode, SubmitPlan, SubmitTransportError, TxSubmitClient,
    providers::{StaticLeaderProvider, StaticRecentBlockhashProvider},
    submit::RpcSubmitTransport,
};

#[derive(Debug)]
struct FuzzRpcTransport {
    should_fail: bool,
    calls: AtomicU64,
}

#[async_trait]
impl RpcSubmitTransport for FuzzRpcTransport {
    async fn submit_rpc(
        &self,
        tx_bytes: &[u8],
        _config: &RpcSubmitConfig,
    ) -> Result<String, SubmitTransportError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.should_fail {
            return Err(SubmitTransportError::Failure {
                message: format!("rpc failure len={}", tx_bytes.len()),
            });
        }
        Ok(format!("rpc-{}", tx_bytes.len()))
    }
}

fn tx_bytes_from_input(bytes: &[u8]) -> Vec<u8> {
    if bytes.first().copied().unwrap_or(0) & 1 == 1 {
        return bytes.to_vec();
    }
    let payer = Keypair::new();
    let recipient = Keypair::new();
    let builder = TxBuilder::new(payer.pubkey()).add_instruction(
        solana_system_interface::instruction::transfer(&payer.pubkey(), &recipient.pubkey(), 1),
    );
    match builder.build_and_sign([8_u8; 32], &[&payer]) {
        Ok(tx) => bincode::serialize(&tx).unwrap_or_else(|_| bytes.to_vec()),
        Err(_) => bytes.to_vec(),
    }
}

fuzz_target!(|bytes: &[u8]| {
    let tx_bytes = tx_bytes_from_input(bytes);
    let signed_tx = if bytes.get(1).copied().unwrap_or(0) & 1 == 1 {
        SignedTx::WireTransactionBytes(tx_bytes)
    } else {
        SignedTx::VersionedTransactionBytes(tx_bytes)
    };
    let rpc = Arc::new(FuzzRpcTransport {
        should_fail: bytes.get(2).copied().unwrap_or(0) & 1 == 1,
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([9_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(None, Vec::new())),
    )
    .with_rpc_transport(rpc.clone());

    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    let result = runtime.block_on(client.submit_signed(signed_tx, SubmitMode::RpcOnly));
    match result {
        Ok(result) => {
            assert_eq!(result.legacy_mode, Some(SubmitMode::RpcOnly));
            assert_eq!(result.plan, SubmitPlan::rpc_only());
            assert!(result.rpc_signature.is_some());
            assert_eq!(result.jito_signature, None);
            assert_eq!(result.direct_target, None);
        }
        Err(error) => match error {
            sof_tx::SubmitError::DecodeSignedBytes { .. } => {
                assert_eq!(rpc.calls.load(Ordering::Relaxed), 0);
            }
            sof_tx::SubmitError::Rpc { .. } => {
                assert_eq!(rpc.calls.load(Ordering::Relaxed), 1);
            }
            other => panic!("unexpected submit error: {other}"),
        },
    }
});
