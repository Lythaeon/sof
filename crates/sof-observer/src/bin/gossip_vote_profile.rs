//! Standalone profiler for gossip vote transaction decode sub-steps.

use std::{str::FromStr, time::Instant};

use bincode::Options as _;
use rand::thread_rng;
use solana_perf::test_tx::new_test_vote_tx;
use solana_sanitize::Sanitize as _;
use solana_transaction::Transaction;
use solana_vote::vote_parser;
use solana_vote_program::vote_instruction::VoteInstruction;

/// Default profile iterations when not overridden by the environment.
const DEFAULT_ITERATIONS: usize = 200_000;
/// Default number of synthetic vote transactions in each fixture batch.
const DEFAULT_COUNT: usize = 32;

/// One vote-decode stage supported by this profiler.
enum ProfileStage {
    /// Measure full bincode deserialization of the nested vote transaction.
    TransactionDeserialize,
    /// Measure `Transaction::sanitize()` on an already-deserialized transaction.
    TransactionSanitize,
    /// Measure slot extraction through the upstream vote parser on a parsed transaction.
    ParsedVoteSlot,
    /// Measure direct vote-instruction deserialization plus slot extraction.
    InstructionDeserialize,
}

impl ProfileStage {
    /// Stable stage name used in profiler output.
    const fn as_str(&self) -> &'static str {
        match self {
            Self::TransactionDeserialize => "transaction_deserialize",
            Self::TransactionSanitize => "transaction_sanitize",
            Self::ParsedVoteSlot => "parsed_vote_slot",
            Self::InstructionDeserialize => "instruction_deserialize",
        }
    }
}

impl FromStr for ProfileStage {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "transaction_deserialize" => Ok(Self::TransactionDeserialize),
            "transaction_sanitize" => Ok(Self::TransactionSanitize),
            "parsed_vote_slot" => Ok(Self::ParsedVoteSlot),
            "instruction_deserialize" => Ok(Self::InstructionDeserialize),
            _ => Err(format!(
                "unknown stage {value:?}; expected transaction_deserialize, transaction_sanitize, parsed_vote_slot, or instruction_deserialize"
            )),
        }
    }
}

fn main() -> Result<(), String> {
    let iterations = std::env::var("SOF_GOSSIP_VOTE_PROFILE_ITERS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_ITERATIONS);
    let count = std::env::var("SOF_GOSSIP_VOTE_PROFILE_COUNT")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_COUNT);
    let stage = std::env::args()
        .nth(1)
        .as_deref()
        .map(ProfileStage::from_str)
        .transpose()?
        .unwrap_or(ProfileStage::TransactionDeserialize);

    let fixtures = build_vote_fixtures(count)?;
    let started_at = Instant::now();
    let mut work_items = 0usize;
    for _ in 0..iterations {
        match stage {
            ProfileStage::TransactionDeserialize => {
                for bytes in &fixtures.transaction_bytes {
                    let _: Transaction = bincode::deserialize(bytes).map_err(|error| {
                        format!("failed to deserialize vote transaction fixture: {error}")
                    })?;
                    work_items = work_items.saturating_add(1);
                }
            }
            ProfileStage::TransactionSanitize => {
                for transaction in &fixtures.transactions {
                    transaction.sanitize().map_err(|error| {
                        format!("failed to sanitize vote transaction fixture: {error}")
                    })?;
                    work_items = work_items.saturating_add(1);
                }
            }
            ProfileStage::ParsedVoteSlot => {
                for transaction in &fixtures.transactions {
                    let (_, vote_transaction, ..) =
                        vote_parser::parse_vote_transaction(transaction)
                            .ok_or_else(|| "failed to parse vote transaction fixture".to_owned())?;
                    let _ = vote_transaction.last_voted_slot();
                    work_items = work_items.saturating_add(1);
                }
            }
            ProfileStage::InstructionDeserialize => {
                for bytes in &fixtures.instruction_bytes {
                    let vote_instruction = bincode::options()
                        .with_limit(solana_packet::PACKET_DATA_SIZE as u64)
                        .with_fixint_encoding()
                        .allow_trailing_bytes()
                        .deserialize::<VoteInstruction>(bytes)
                        .map_err(|error| {
                            format!("failed to deserialize vote instruction fixture: {error}")
                        })?;
                    let _ = vote_instruction.last_voted_slot();
                    work_items = work_items.saturating_add(1);
                }
            }
        }
    }

    let ns_per_iteration_x1000 = started_at
        .elapsed()
        .as_nanos()
        .saturating_mul(1000)
        .checked_div(iterations as u128)
        .ok_or_else(|| "invalid iteration count".to_owned())?;

    println!(
        "vote_fixtures={} stage={} iterations={} ns_per_iteration={}.{:03} work_items={}",
        count,
        stage.as_str(),
        iterations,
        ns_per_iteration_x1000 / 1000,
        ns_per_iteration_x1000 % 1000,
        work_items
    );
    Ok(())
}

/// Synthetic vote fixtures reused across the profile stages.
struct VoteFixtures {
    /// Parsed vote transactions reused by the parser-only stage.
    transactions: Vec<Transaction>,
    /// Serialized vote transactions reused by the transaction-deserialize stage.
    transaction_bytes: Vec<Vec<u8>>,
    /// Serialized first-instruction payloads reused by the instruction-only stage.
    instruction_bytes: Vec<Vec<u8>>,
}

/// Builds one deterministic batch of vote fixtures for profiling.
fn build_vote_fixtures(count: usize) -> Result<VoteFixtures, String> {
    let mut rng = thread_rng();
    let mut transactions = Vec::with_capacity(count);
    let mut transaction_bytes = Vec::with_capacity(count);
    let mut instruction_bytes = Vec::with_capacity(count);
    for index in 0..count {
        let transaction = new_test_vote_tx(&mut rng);
        let first_instruction_data = transaction
            .message()
            .instructions
            .first()
            .map(|instruction| instruction.data.clone())
            .ok_or_else(|| format!("vote transaction fixture {index} missing first instruction"))?;
        let tx_bytes = bincode::serialize(&transaction).map_err(|error| {
            format!("failed to serialize vote transaction fixture {index}: {error}")
        })?;
        transactions.push(transaction);
        transaction_bytes.push(tx_bytes);
        instruction_bytes.push(first_instruction_data);
    }
    Ok(VoteFixtures {
        transactions,
        transaction_bytes,
        instruction_bytes,
    })
}
