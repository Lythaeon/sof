//! SOF runtime example with one non-vote transaction plugin.
#![doc(hidden)]

use sof::{
    event::TxKind,
    framework::{Plugin, PluginHost, TransactionEvent},
};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Default)]
struct NonVoteTxLoggerPlugin;

impl Plugin for NonVoteTxLoggerPlugin {
    fn name(&self) -> &'static str {
        "non-vote-tx-logger"
    }

    fn on_transaction(&self, event: TransactionEvent<'_>) {
        if event.kind == TxKind::VoteOnly {
            return;
        }
        let signature = event
            .signature
            .map(ToString::to_string)
            .unwrap_or_else(|| "NO_SIGNATURE".to_owned());
        tracing::info!(
            slot = event.slot,
            signature = %signature,
            tx_kind = ?event.kind,
            "non-vote transaction observed"
        );
    }
}

#[derive(Debug, Error)]
enum ObserverWithNonVotePluginError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error(transparent)]
    Runtime(#[from] sof::runtime::RuntimeError),
}

const fn require_release_mode() -> Result<(), ObserverWithNonVotePluginError> {
    if cfg!(debug_assertions) {
        return Err(ObserverWithNonVotePluginError::ReleaseModeRequired {
            command: "cargo run --release -p sof --example observer_with_non_vote_plugin",
        });
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), ObserverWithNonVotePluginError> {
    require_release_mode()?;
    let host = PluginHost::builder()
        .add_plugin(NonVoteTxLoggerPlugin)
        .build();
    println!("registered plugins: {:?}", host.plugin_names());
    Ok(sof::runtime::run_async_with_plugin_host(host).await?)
}
