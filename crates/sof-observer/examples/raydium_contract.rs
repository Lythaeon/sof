//! Plugin example that filters and logs Raydium transactions by program ID.
#![doc(hidden)]

use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};

use sof::framework::{Plugin, PluginHost, TransactionEvent};
use solana_pubkey::Pubkey;
use thiserror::Error;

pub const RAYDIUM_STANDARD_AMM_PROGRAM_ID: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";
pub const RAYDIUM_V4_PROGRAM_ID: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
pub const RAYDIUM_STABLE_SWAP_AMM_PROGRAM_ID: &str = "5quBtoiQqxF9Jv6KYKctB59NT3gtJD2Y65kdnB1Uev3h";
pub const RAYDIUM_CLMM_PROGRAM_ID: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
pub const RAYDIUM_LAUNCHLAB_PROGRAM_ID: &str = "LanMV9sAd7wArD4vJFi2qDdfnVhFxYSUg6eADduJ3uj";
pub const RAYDIUM_BURN_EARN_PROGRAM_ID: &str = "LockrWmn6K5twhz3y9w1dQERbmgSaRkfnTeTKbpofwE";
pub const RAYDIUM_ROUTING_PROGRAM_ID: &str = "routeUGWgWzqBWFcrCfv8tritsqukccJPu3q5GPP3xS";
pub const RAYDIUM_STAKING_PROGRAM_ID: &str = "EhhTKczWMGQt46ynNeRX1WfeagwwJd7ufHvCDjRxjo5Q";
pub const RAYDIUM_FARM_STAKING_PROGRAM_ID: &str = "9KEPoZmtHUrBbhWN1v1KWLMkkvwY6WLtAVUCPRtRjP4z";
pub const RAYDIUM_ECOSYSTEM_FARM_PROGRAM_ID: &str = "FarmqiPv5eAj3j1GMdMCMUGXqPUvmquZtMy86QH6rzhG";
const MISSING_SIGNATURE_LABEL: &str = "NO_SIGNATURE";

static RAYDIUM_TX_COUNT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Default)]
struct RaydiumTxFilterLoggerPlugin;

#[derive(Debug, Clone, Copy, Default)]
struct RaydiumProgramsTouched {
    launchlab: bool,
    cpmm: bool,
    v4: bool,
    stable_swap: bool,
    clmm: bool,
    burn_earn: bool,
    routing: bool,
    staking: bool,
    farm_staking: bool,
    ecosystem_farm: bool,
    direct_program_invocation: bool,
    account_reference: bool,
    instruction_match_count: u64,
    account_match_count: u64,
}

impl RaydiumProgramsTouched {
    const fn any(self) -> bool {
        self.launchlab
            || self.cpmm
            || self.v4
            || self.stable_swap
            || self.clmm
            || self.burn_earn
            || self.routing
            || self.staking
            || self.farm_staking
            || self.ecosystem_farm
    }
}

impl Plugin for RaydiumTxFilterLoggerPlugin {
    fn name(&self) -> &'static str {
        "raydium-tx-filter-logger"
    }

    fn on_transaction(&self, event: TransactionEvent<'_>) {
        let Some(touched) = classify_raydium_transaction(event) else {
            return;
        };

        let signature = event
            .signature
            .map(ToString::to_string)
            .unwrap_or_else(|| MISSING_SIGNATURE_LABEL.to_owned());
        let seen = RAYDIUM_TX_COUNT
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);

        tracing::info!(
            slot = event.slot,
            signature = %signature,
            tx_kind = ?event.kind,
            seen,
            launchlab = touched.launchlab,
            cpmm = touched.cpmm,
            v4 = touched.v4,
            stable_swap = touched.stable_swap,
            clmm = touched.clmm,
            burn_earn = touched.burn_earn,
            routing = touched.routing,
            staking = touched.staking,
            farm_staking = touched.farm_staking,
            ecosystem_farm = touched.ecosystem_farm,
            direct_program_invocation = touched.direct_program_invocation,
            account_reference = touched.account_reference,
            instruction_match_count = touched.instruction_match_count,
            account_match_count = touched.account_match_count,
            "raydium transaction observed"
        );
    }
}

fn classify_raydium_transaction(event: TransactionEvent<'_>) -> Option<RaydiumProgramsTouched> {
    let message = &event.tx.message;
    let keys = message.static_account_keys();
    let raydium_launchlab = raydium_launchlab_pubkey()?;
    let raydium_cpmm = raydium_cpmm_pubkey()?;
    let raydium_v4 = raydium_v4_pubkey()?;
    let raydium_stable_swap = raydium_stable_swap_pubkey()?;
    let raydium_clmm = raydium_clmm_pubkey()?;
    let raydium_burn_earn = raydium_burn_earn_pubkey()?;
    let raydium_routing = raydium_routing_pubkey()?;
    let raydium_staking = raydium_staking_pubkey()?;
    let raydium_farm_staking = raydium_farm_staking_pubkey()?;
    let raydium_ecosystem_farm = raydium_ecosystem_farm_pubkey()?;
    let mut touched = RaydiumProgramsTouched::default();
    let mut match_program = |program_id: &Pubkey, direct_invocation: bool| {
        let mut matched = false;
        if *program_id == raydium_launchlab {
            touched.launchlab = true;
            matched = true;
        } else if *program_id == raydium_cpmm {
            touched.cpmm = true;
            matched = true;
        } else if *program_id == raydium_v4 {
            touched.v4 = true;
            matched = true;
        } else if *program_id == raydium_stable_swap {
            touched.stable_swap = true;
            matched = true;
        } else if *program_id == raydium_clmm {
            touched.clmm = true;
            matched = true;
        } else if *program_id == raydium_burn_earn {
            touched.burn_earn = true;
            matched = true;
        } else if *program_id == raydium_routing {
            touched.routing = true;
            matched = true;
        } else if *program_id == raydium_staking {
            touched.staking = true;
            matched = true;
        } else if *program_id == raydium_farm_staking {
            touched.farm_staking = true;
            matched = true;
        } else if *program_id == raydium_ecosystem_farm {
            touched.ecosystem_farm = true;
            matched = true;
        }
        if !matched {
            return;
        }
        if direct_invocation {
            touched.direct_program_invocation = true;
            touched.instruction_match_count = touched.instruction_match_count.saturating_add(1);
        } else {
            touched.account_reference = true;
            touched.account_match_count = touched.account_match_count.saturating_add(1);
        }
    };

    for ix in message.instructions() {
        let Some(program_id) = keys.get(usize::from(ix.program_id_index)) else {
            continue;
        };
        match_program(program_id, true);
    }

    for account_key in keys {
        match_program(account_key, false);
    }

    touched.any().then_some(touched)
}

fn parse_program_pubkey(value: &str) -> Option<Pubkey> {
    value.parse().ok()
}

fn raydium_launchlab_pubkey() -> Option<Pubkey> {
    static PK: OnceLock<Option<Pubkey>> = OnceLock::new();
    PK.get_or_init(|| parse_program_pubkey(RAYDIUM_LAUNCHLAB_PROGRAM_ID))
        .to_owned()
}

fn raydium_cpmm_pubkey() -> Option<Pubkey> {
    static PK: OnceLock<Option<Pubkey>> = OnceLock::new();
    PK.get_or_init(|| parse_program_pubkey(RAYDIUM_STANDARD_AMM_PROGRAM_ID))
        .to_owned()
}

fn raydium_v4_pubkey() -> Option<Pubkey> {
    static PK: OnceLock<Option<Pubkey>> = OnceLock::new();
    PK.get_or_init(|| parse_program_pubkey(RAYDIUM_V4_PROGRAM_ID))
        .to_owned()
}

fn raydium_stable_swap_pubkey() -> Option<Pubkey> {
    static PK: OnceLock<Option<Pubkey>> = OnceLock::new();
    PK.get_or_init(|| parse_program_pubkey(RAYDIUM_STABLE_SWAP_AMM_PROGRAM_ID))
        .to_owned()
}

fn raydium_clmm_pubkey() -> Option<Pubkey> {
    static PK: OnceLock<Option<Pubkey>> = OnceLock::new();
    PK.get_or_init(|| parse_program_pubkey(RAYDIUM_CLMM_PROGRAM_ID))
        .to_owned()
}

fn raydium_burn_earn_pubkey() -> Option<Pubkey> {
    static PK: OnceLock<Option<Pubkey>> = OnceLock::new();
    PK.get_or_init(|| parse_program_pubkey(RAYDIUM_BURN_EARN_PROGRAM_ID))
        .to_owned()
}

fn raydium_routing_pubkey() -> Option<Pubkey> {
    static PK: OnceLock<Option<Pubkey>> = OnceLock::new();
    PK.get_or_init(|| parse_program_pubkey(RAYDIUM_ROUTING_PROGRAM_ID))
        .to_owned()
}

fn raydium_staking_pubkey() -> Option<Pubkey> {
    static PK: OnceLock<Option<Pubkey>> = OnceLock::new();
    PK.get_or_init(|| parse_program_pubkey(RAYDIUM_STAKING_PROGRAM_ID))
        .to_owned()
}

fn raydium_farm_staking_pubkey() -> Option<Pubkey> {
    static PK: OnceLock<Option<Pubkey>> = OnceLock::new();
    PK.get_or_init(|| parse_program_pubkey(RAYDIUM_FARM_STAKING_PROGRAM_ID))
        .to_owned()
}

fn raydium_ecosystem_farm_pubkey() -> Option<Pubkey> {
    static PK: OnceLock<Option<Pubkey>> = OnceLock::new();
    PK.get_or_init(|| parse_program_pubkey(RAYDIUM_ECOSYSTEM_FARM_PROGRAM_ID))
        .to_owned()
}

#[derive(Debug, Error)]
enum RaydiumTxFilterExampleError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error(transparent)]
    Runtime(#[from] sof::runtime::RuntimeError),
}

const fn require_release_mode() -> Result<(), RaydiumTxFilterExampleError> {
    if cfg!(debug_assertions) {
        return Err(RaydiumTxFilterExampleError::ReleaseModeRequired {
            command: "cargo run --release -p sof --example raydium_contract",
        });
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), RaydiumTxFilterExampleError> {
    require_release_mode()?;
    let host = PluginHost::builder()
        .add_plugin(RaydiumTxFilterLoggerPlugin)
        .build();

    tracing::warn!(plugins = ?host.plugin_names(), "starting SOF runtime with plugin host");
    Ok(sof::runtime::run_async_with_plugin_host(host).await?)
}
