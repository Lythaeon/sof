//! Standalone profiler for CRDS value deserialize and sanitize stages.

use std::{str::FromStr, time::Instant};

use rand::thread_rng;
use solana_gossip::{contact_info::ContactInfo, crds_data::CrdsData, crds_value::CrdsValue};
use solana_keypair::Keypair;
use solana_perf::test_tx::new_test_vote_tx;
use solana_sanitize::Sanitize as _;
use solana_signer::Signer as _;

/// Default profile iterations when not overridden by the environment.
const DEFAULT_ITERATIONS: usize = 200_000;
/// Default synthetic wallclock used by the fixture values.
const DEFAULT_WALLCLOCK: u64 = 1_234_567_890;

/// One CRDS-value fixture plus its serialized bytes.
struct ProtocolFixture {
    /// Stable fixture name used in profiler output and filtering.
    name: &'static str,
    /// Bincode-serialized CRDS values as received from gossip.
    bytes: Vec<u8>,
}

impl ProtocolFixture {
    /// Creates one serialized CRDS-value fixture.
    fn new(name: &'static str, values: &[CrdsValue]) -> Result<Self, String> {
        let bytes = bincode::serialize(values)
            .map_err(|error| format!("failed to serialize fixture {name}: {error}"))?;
        Ok(Self { name, bytes })
    }
}

/// One profiling stage supported by this binary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProfileStage {
    /// Measure CRDS-value deserialization only.
    Deserialize,
    /// Measure CRDS-value deserialization plus sanitize.
    DeserializeAndSanitize,
}

impl ProfileStage {
    /// Stable stage name used in output.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Deserialize => "deserialize",
            Self::DeserializeAndSanitize => "deserialize_and_sanitize",
        }
    }
}

impl FromStr for ProfileStage {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "deserialize" => Ok(Self::Deserialize),
            "deserialize_and_sanitize" => Ok(Self::DeserializeAndSanitize),
            _ => Err(format!(
                "unknown stage {value:?}; expected deserialize or deserialize_and_sanitize",
            )),
        }
    }
}

/// One measured fixture result.
struct ProtocolProfileResult {
    /// Stable fixture name used in profiler output.
    fixture_name: &'static str,
    /// Stable stage name used in profiler output.
    stage_name: &'static str,
    /// Nanoseconds per iteration scaled by 1000 for fixed-point formatting.
    ns_per_iteration_x1000: u128,
    /// Number of CRDS values decoded from the fixture.
    decoded_values: usize,
}

/// Entry point for the standalone CRDS-value profiler.
fn main() -> Result<(), String> {
    let iterations = std::env::var("SOF_GOSSIP_PROTOCOL_PROFILE_ITERS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_ITERATIONS);
    let selected_fixture = std::env::args().nth(1);
    let selected_stage = std::env::args()
        .nth(2)
        .map(|value| ProfileStage::from_str(&value))
        .transpose()?;

    let fixtures = [
        ProtocolFixture::new("crds_values_contact_info_1", &build_contact_info_values(1))?,
        ProtocolFixture::new(
            "crds_values_contact_info_32",
            &build_contact_info_values(32),
        )?,
        ProtocolFixture::new(
            "crds_values_contact_info_64",
            &build_contact_info_values(64),
        )?,
        ProtocolFixture::new("crds_values_vote_1", &build_vote_values(1)?)?,
        ProtocolFixture::new("crds_values_vote_32", &build_vote_values(32)?)?,
        ProtocolFixture::new("crds_values_vote_64", &build_vote_values(64)?)?,
    ];
    let stages = [
        ProfileStage::Deserialize,
        ProfileStage::DeserializeAndSanitize,
    ];

    let mut matched_fixture = false;
    for fixture in fixtures {
        if let Some(selected_fixture) = selected_fixture.as_deref()
            && fixture.name != selected_fixture
        {
            continue;
        }
        matched_fixture = true;
        for stage in stages {
            if let Some(selected_stage) = selected_stage
                && stage != selected_stage
            {
                continue;
            }
            let result = profile_fixture(&fixture, stage, iterations)?;
            let ns_per_iteration_whole = result.ns_per_iteration_x1000 / 1000;
            let ns_per_iteration_fraction = result.ns_per_iteration_x1000 % 1000;
            println!(
                "{} stage={} iterations={} ns_per_iteration={}.{:03} decoded_values={}",
                result.fixture_name,
                result.stage_name,
                iterations,
                ns_per_iteration_whole,
                ns_per_iteration_fraction,
                result.decoded_values,
            );
        }
    }
    if !matched_fixture {
        return Err(format!(
            "unknown fixture {:?}; expected one of crds_values_contact_info_1, crds_values_contact_info_32, crds_values_contact_info_64, crds_values_vote_1, crds_values_vote_32, crds_values_vote_64",
            selected_fixture,
        ));
    }

    Ok(())
}

/// Profiles one fixture for the requested iteration count.
fn profile_fixture(
    fixture: &ProtocolFixture,
    stage: ProfileStage,
    iterations: usize,
) -> Result<ProtocolProfileResult, String> {
    let mut decoded_values = 0usize;
    let started_at = Instant::now();
    for _ in 0..iterations {
        let values: Vec<CrdsValue> = bincode::deserialize(&fixture.bytes)
            .map_err(|error| format!("failed to deserialize fixture {}: {error}", fixture.name))?;
        if let ProfileStage::DeserializeAndSanitize = stage {
            for value in &values {
                value.sanitize().map_err(|error| {
                    format!("failed to sanitize fixture {}: {error:?}", fixture.name)
                })?;
            }
        }
        decoded_values = values.len();
    }
    let ns_per_iteration_x1000 = started_at
        .elapsed()
        .as_nanos()
        .saturating_mul(1000)
        .checked_div(iterations as u128)
        .ok_or_else(|| format!("invalid iteration count for fixture {}", fixture.name))?;
    Ok(ProtocolProfileResult {
        fixture_name: fixture.name,
        stage_name: stage.as_str(),
        ns_per_iteration_x1000,
        decoded_values,
    })
}

/// Builds deterministic contact-info CRDS values for profiling.
fn build_contact_info_values(count: usize) -> Vec<CrdsValue> {
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let keypair = Keypair::new();
        let wallclock = DEFAULT_WALLCLOCK.saturating_add(index as u64);
        let contact_info = ContactInfo::new_localhost(&keypair.pubkey(), wallclock);
        values.push(CrdsValue::new(
            CrdsData::ContactInfo(contact_info),
            &keypair,
        ));
    }
    values
}

/// Builds deterministic vote CRDS values for profiling.
fn build_vote_values(count: usize) -> Result<Vec<CrdsValue>, String> {
    let mut rng = thread_rng();
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let keypair = Keypair::new();
        let wallclock = DEFAULT_WALLCLOCK.saturating_add(index as u64);
        let vote = solana_gossip::crds_data::Vote::new(
            keypair.pubkey(),
            new_test_vote_tx(&mut rng),
            wallclock,
        )
        .ok_or_else(|| format!("failed to construct vote fixture at index {index}"))?;
        values.push(CrdsValue::new(CrdsData::Vote(0, vote), &keypair));
    }
    Ok(values)
}
