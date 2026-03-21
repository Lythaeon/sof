//! Minimal SOF runtime example without plugins.
#![doc(hidden)]

use sof_gossip_tuning::{GossipTuningProfile, HostProfilePreset};
use thiserror::Error;

#[derive(Debug, Error)]
enum ObserverRuntimeExampleError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("unknown SOF_TUNING_PRESET `{value}`; expected one of: home, vps, dedicated")]
    UnknownTuningPreset { value: String },
    #[error(transparent)]
    Runtime(#[from] sof::runtime::RuntimeError),
}

const fn require_release_mode() -> Result<(), ObserverRuntimeExampleError> {
    if cfg!(debug_assertions) {
        return Err(ObserverRuntimeExampleError::ReleaseModeRequired {
            command: "cargo run --release -p sof --example observer_runtime",
        });
    }
    Ok(())
}

fn read_tuning_preset() -> Result<Option<HostProfilePreset>, ObserverRuntimeExampleError> {
    let Some(value) = std::env::var("SOF_TUNING_PRESET")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
    else {
        return Ok(None);
    };

    let preset = match value.as_str() {
        "" => return Ok(None),
        "home" => HostProfilePreset::Home,
        "vps" => HostProfilePreset::Vps,
        "dedicated" => HostProfilePreset::Dedicated,
        _ => {
            return Err(ObserverRuntimeExampleError::UnknownTuningPreset { value });
        }
    };
    Ok(Some(preset))
}

#[tokio::main]
async fn main() -> Result<(), ObserverRuntimeExampleError> {
    require_release_mode()?;

    let mut runtime = sof::runtime::ObserverRuntime::new();
    if let Some(preset) = read_tuning_preset()? {
        runtime = runtime.with_setup(
            sof::runtime::RuntimeSetup::new()
                .with_gossip_tuning_profile(GossipTuningProfile::preset(preset)),
        );
        tracing::info!(?preset, "applied typed SOF gossip tuning preset");
    }

    Ok(runtime.run_until_termination_signal().await?)
}
