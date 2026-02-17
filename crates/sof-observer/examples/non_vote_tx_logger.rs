//! Plugin example that logs non-vote transactions.
#![doc(hidden)]

use std::sync::atomic::{AtomicU64, Ordering};

use sof::{
    event::TxKind,
    framework::{DatasetEvent, Plugin, PluginHost, ShredEvent, TransactionEvent},
};
use thiserror::Error;

static DATASET_COUNT: AtomicU64 = AtomicU64::new(0);
static SHRED_COUNT: AtomicU64 = AtomicU64::new(0);
static VOTE_TX_COUNT: AtomicU64 = AtomicU64::new(0);
// Throttle progress logs so the example stays readable at high throughput.
const LOG_ON_FIRST_EVENT_COUNT: u64 = 1;
const DATASET_PROGRESS_LOG_EVERY: u64 = 200;
const SHRED_PROGRESS_LOG_EVERY: u64 = 10_000;
const VOTE_TX_PROGRESS_LOG_EVERY: u64 = 500;
const MISSING_SIGNATURE_LABEL: &str = "NO_SIGNATURE";

#[derive(Debug, Clone, Copy, Default)]
struct NonVoteTxLoggerPlugin;

impl Plugin for NonVoteTxLoggerPlugin {
    fn name(&self) -> &'static str {
        "non-vote-tx-logger"
    }

    fn on_dataset(&self, event: DatasetEvent) {
        let seen = DATASET_COUNT
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if seen == LOG_ON_FIRST_EVENT_COUNT || seen.is_multiple_of(DATASET_PROGRESS_LOG_EVERY) {
            tracing::info!(
                slot = event.slot,
                seen,
                "dataset observed while scanning for non-vote transactions"
            );
        }
    }

    fn on_shred(&self, event: ShredEvent<'_>) {
        let seen = SHRED_COUNT
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if seen == LOG_ON_FIRST_EVENT_COUNT || seen.is_multiple_of(SHRED_PROGRESS_LOG_EVERY) {
            tracing::info!(
                source = %event.source,
                seen,
                "shred observed while scanning for non-vote transactions"
            );
        }
    }

    fn on_transaction(&self, event: TransactionEvent<'_>) {
        if event.kind == TxKind::VoteOnly {
            let seen = VOTE_TX_COUNT
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);
            if seen == LOG_ON_FIRST_EVENT_COUNT || seen.is_multiple_of(VOTE_TX_PROGRESS_LOG_EVERY) {
                tracing::info!(
                    slot = event.slot,
                    seen,
                    "vote-only transaction observed while waiting for non-vote traffic"
                );
            }
            return;
        }

        let signature = event
            .signature
            .map(ToString::to_string)
            .unwrap_or_else(|| MISSING_SIGNATURE_LABEL.to_owned());

        tracing::info!(
            slot = event.slot,
            signature = %signature,
            tx_kind = ?event.kind,
            "non-vote transaction observed"
        );
    }
}

#[derive(Debug, Error)]
enum NonVoteTxLoggerExampleError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error(transparent)]
    Runtime(#[from] sof::runtime::RuntimeError),
}

const fn require_release_mode() -> Result<(), NonVoteTxLoggerExampleError> {
    if cfg!(debug_assertions) {
        return Err(NonVoteTxLoggerExampleError::ReleaseModeRequired {
            command: "cargo run --release -p sof --example non_vote_tx_logger",
        });
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), NonVoteTxLoggerExampleError> {
    require_release_mode()?;
    let host = PluginHost::builder()
        .add_plugin(NonVoteTxLoggerPlugin)
        .build();

    tracing::warn!(plugins = ?host.plugin_names(), "starting SOF runtime with plugin host");
    Ok(sof::runtime::run_async_with_plugin_host(host).await?)
}
