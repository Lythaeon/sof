#![no_main]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use libfuzzer_sys::fuzz_target;
use solana_keypair::Keypair;
use solana_signer::Signer;
use sof_tx::{
    DirectSubmitConfig, SignedTx, SubmitMode, SubmitTransportError, TxBuilder, TxSubmitClient,
    providers::{LeaderTarget, StaticLeaderProvider, StaticRecentBlockhashProvider},
    routing::RoutingPolicy,
    submit::DirectSubmitTransport,
};

#[derive(Debug)]
struct FuzzDirectTransport {
    should_fail: bool,
    calls: AtomicU64,
}

#[async_trait]
impl DirectSubmitTransport for FuzzDirectTransport {
    async fn submit_direct(
        &self,
        tx_bytes: &[u8],
        targets: &[LeaderTarget],
        _policy: RoutingPolicy,
        _config: &DirectSubmitConfig,
    ) -> Result<LeaderTarget, SubmitTransportError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.should_fail || targets.is_empty() {
            return Err(SubmitTransportError::Failure {
                message: format!("direct failure len={} targets={}", tx_bytes.len(), targets.len()),
            });
        }
        Ok(targets[0].clone())
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
    let direct_target = LeaderTarget::new(None, std::net::SocketAddr::from(([127, 0, 0, 1], 9001)));
    let direct = Arc::new(FuzzDirectTransport {
        should_fail: bytes.get(2).copied().unwrap_or(0) & 1 == 1,
        calls: AtomicU64::new(0),
    });
    let mut client = TxSubmitClient::new(
        Arc::new(StaticRecentBlockhashProvider::new(Some([9_u8; 32]))),
        Arc::new(StaticLeaderProvider::new(Some(direct_target.clone()), vec![direct_target.clone()])),
    )
    .with_direct_transport(direct.clone());

    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    let result = runtime.block_on(client.submit_signed(signed_tx, SubmitMode::DirectOnly));
    match result {
        Ok(result) => {
            assert_eq!(result.mode, SubmitMode::DirectOnly);
            assert_eq!(result.rpc_signature, None);
            assert_eq!(result.jito_signature, None);
            assert!(result.direct_target.is_some());
        }
        Err(error) => match error {
            sof_tx::SubmitError::DecodeSignedBytes { .. } => {
                assert_eq!(direct.calls.load(Ordering::Relaxed), 0);
            }
            sof_tx::SubmitError::Direct { .. } | sof_tx::SubmitError::NoDirectTargets => {}
            other => panic!("unexpected submit error: {other}"),
        },
    }
});
