//! Standalone profiler for authoritative transaction decode alternatives.

use std::{str::FromStr, time::Instant};

use agave_transaction_view::transaction_view::{
    SanitizedTransactionView, UnsanitizedTransactionView,
};
use rand::thread_rng;
use solana_perf::test_tx::{new_test_vote_tx, test_tx};
use solana_transaction::versioned::VersionedTransaction;

/// Default profile iterations when not overridden by the environment.
const DEFAULT_ITERATIONS: usize = 200_000;
/// Default number of synthetic transactions in each fixture batch.
const DEFAULT_COUNT: usize = 64;
/// Mirrors `solana_transaction_context::MAX_INSTRUCTION_TRACE_LENGTH`.
const MAX_INSTRUCTION_TRACE_LENGTH: usize = 64;

/// One profiling stage supported by this binary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProfileStage {
    /// Measure full bincode deserialization into `VersionedTransaction`.
    VersionedDeserialize,
    /// Measure borrowed parse without sanitize.
    ViewUnsanitizedParse,
    /// Measure borrowed parse/sanitize through `TransactionView`.
    ViewSanitizedParse,
    /// Measure only signature sanitize checks on an already parsed view.
    SanitizeSignatures,
    /// Measure only account-access sanitize checks on an already parsed view.
    SanitizeAccountAccess,
    /// Measure only instruction sanitize checks on an already parsed view.
    SanitizeInstructions,
    /// Measure only address-table sanitize checks on an already parsed view.
    SanitizeAddressTableLookups,
    /// Measure one cheap classification pass over the sanitized view.
    ViewVoteClassify,
}

impl ProfileStage {
    /// Stable stage name used in profiler output.
    const fn as_str(self) -> &'static str {
        match self {
            Self::VersionedDeserialize => "versioned_deserialize",
            Self::ViewUnsanitizedParse => "view_unsanitized_parse",
            Self::ViewSanitizedParse => "view_sanitized_parse",
            Self::SanitizeSignatures => "sanitize_signatures",
            Self::SanitizeAccountAccess => "sanitize_account_access",
            Self::SanitizeInstructions => "sanitize_instructions",
            Self::SanitizeAddressTableLookups => "sanitize_address_table_lookups",
            Self::ViewVoteClassify => "view_vote_classify",
        }
    }
}

impl FromStr for ProfileStage {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "versioned_deserialize" => Ok(Self::VersionedDeserialize),
            "view_unsanitized_parse" => Ok(Self::ViewUnsanitizedParse),
            "view_sanitized_parse" => Ok(Self::ViewSanitizedParse),
            "sanitize_signatures" => Ok(Self::SanitizeSignatures),
            "sanitize_account_access" => Ok(Self::SanitizeAccountAccess),
            "sanitize_instructions" => Ok(Self::SanitizeInstructions),
            "sanitize_address_table_lookups" => Ok(Self::SanitizeAddressTableLookups),
            "view_vote_classify" => Ok(Self::ViewVoteClassify),
            _ => Err(format!(
                "unknown stage {value:?}; expected versioned_deserialize, view_unsanitized_parse, view_sanitized_parse, sanitize_signatures, sanitize_account_access, sanitize_instructions, sanitize_address_table_lookups, or view_vote_classify"
            )),
        }
    }
}

/// One synthetic transaction fixture family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixtureKind {
    /// System-transfer style simple transactions.
    Transfer,
    /// Vote transactions with more complex instruction payloads.
    Vote,
}

impl FixtureKind {
    /// Stable fixture name used in profiler output.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Transfer => "transfer",
            Self::Vote => "vote",
        }
    }
}

impl FromStr for FixtureKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "transfer" => Ok(Self::Transfer),
            "vote" => Ok(Self::Vote),
            _ => Err(format!(
                "unknown fixture {value:?}; expected transfer or vote"
            )),
        }
    }
}

/// Synthetic serialized transactions reused across the profile stages.
struct TxFixtures {
    /// Stable fixture family name used in output.
    name: &'static str,
    /// Serialized versioned transactions.
    tx_bytes: Vec<Vec<u8>>,
}

fn main() -> Result<(), String> {
    let iterations = std::env::var("SOF_TX_DECODE_PROFILE_ITERS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_ITERATIONS);
    let count = std::env::var("SOF_TX_DECODE_PROFILE_COUNT")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_COUNT);
    let fixture_kind = std::env::args()
        .nth(1)
        .as_deref()
        .map(FixtureKind::from_str)
        .transpose()?
        .unwrap_or(FixtureKind::Transfer);
    let stage = std::env::args()
        .nth(2)
        .as_deref()
        .map(ProfileStage::from_str)
        .transpose()?
        .unwrap_or(ProfileStage::VersionedDeserialize);

    let fixtures = build_fixtures(fixture_kind, count)?;
    let started_at = Instant::now();
    let mut work_items = 0usize;
    let mut vote_only_count = 0usize;
    for _ in 0..iterations {
        match stage {
            ProfileStage::VersionedDeserialize => {
                for bytes in &fixtures.tx_bytes {
                    let _: VersionedTransaction = bincode::deserialize(bytes).map_err(|error| {
                        format!(
                            "failed to deserialize {} fixture transaction: {error}",
                            fixtures.name
                        )
                    })?;
                    work_items = work_items.saturating_add(1);
                }
            }
            ProfileStage::ViewUnsanitizedParse => {
                for bytes in &fixtures.tx_bytes {
                    let _ = UnsanitizedTransactionView::try_new_unsanitized(bytes.as_slice())
                        .map_err(|error| {
                            format!(
                                "failed to parse unsanitized {} transaction view: {error:?}",
                                fixtures.name
                            )
                        })?;
                    work_items = work_items.saturating_add(1);
                }
            }
            ProfileStage::ViewSanitizedParse => {
                for bytes in &fixtures.tx_bytes {
                    let _ = SanitizedTransactionView::try_new_sanitized(bytes.as_slice(), true)
                        .map_err(|error| {
                            format!(
                                "failed to parse sanitized {} transaction view: {error:?}",
                                fixtures.name
                            )
                        })?;
                    work_items = work_items.saturating_add(1);
                }
            }
            ProfileStage::SanitizeSignatures => {
                for bytes in &fixtures.tx_bytes {
                    let view = UnsanitizedTransactionView::try_new_unsanitized(bytes.as_slice())
                        .map_err(|error| {
                            format!(
                                "failed to parse unsanitized {} transaction view: {error:?}",
                                fixtures.name
                            )
                        })?;
                    sanitize_signatures_only(&view)
                        .map_err(|error| format!("signature sanitize failed: {error}"))?;
                    work_items = work_items.saturating_add(1);
                }
            }
            ProfileStage::SanitizeAccountAccess => {
                for bytes in &fixtures.tx_bytes {
                    let view = UnsanitizedTransactionView::try_new_unsanitized(bytes.as_slice())
                        .map_err(|error| {
                            format!(
                                "failed to parse unsanitized {} transaction view: {error:?}",
                                fixtures.name
                            )
                        })?;
                    sanitize_account_access_only(&view)
                        .map_err(|error| format!("account sanitize failed: {error}"))?;
                    work_items = work_items.saturating_add(1);
                }
            }
            ProfileStage::SanitizeInstructions => {
                for bytes in &fixtures.tx_bytes {
                    let view = UnsanitizedTransactionView::try_new_unsanitized(bytes.as_slice())
                        .map_err(|error| {
                            format!(
                                "failed to parse unsanitized {} transaction view: {error:?}",
                                fixtures.name
                            )
                        })?;
                    sanitize_instructions_only(&view, true)
                        .map_err(|error| format!("instruction sanitize failed: {error}"))?;
                    work_items = work_items.saturating_add(1);
                }
            }
            ProfileStage::SanitizeAddressTableLookups => {
                for bytes in &fixtures.tx_bytes {
                    let view = UnsanitizedTransactionView::try_new_unsanitized(bytes.as_slice())
                        .map_err(|error| {
                            format!(
                                "failed to parse unsanitized {} transaction view: {error:?}",
                                fixtures.name
                            )
                        })?;
                    sanitize_address_table_lookups_only(&view)
                        .map_err(|error| format!("lookup sanitize failed: {error}"))?;
                    work_items = work_items.saturating_add(1);
                }
            }
            ProfileStage::ViewVoteClassify => {
                for bytes in &fixtures.tx_bytes {
                    let view = SanitizedTransactionView::try_new_sanitized(bytes.as_slice(), true)
                        .map_err(|error| {
                            format!(
                                "failed to parse sanitized {} transaction view: {error:?}",
                                fixtures.name
                            )
                        })?;
                    vote_only_count =
                        vote_only_count.saturating_add(usize::from(classify_vote_only(&view)));
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
        "fixture={} stage={} count={} iterations={} ns_per_iteration={}.{:03} work_items={} vote_only_count={}",
        fixtures.name,
        stage.as_str(),
        count,
        iterations,
        ns_per_iteration_x1000 / 1000,
        ns_per_iteration_x1000 % 1000,
        work_items,
        vote_only_count
    );

    Ok(())
}

/// Builds one reusable serialized transaction fixture batch for the selected family.
fn build_fixtures(kind: FixtureKind, count: usize) -> Result<TxFixtures, String> {
    let mut tx_bytes = Vec::with_capacity(count);
    let mut rng = thread_rng();
    for index in 0..count {
        let transaction = match kind {
            FixtureKind::Transfer => VersionedTransaction::from(test_tx()),
            FixtureKind::Vote => VersionedTransaction::from(new_test_vote_tx(&mut rng)),
        };
        let bytes = bincode::serialize(&transaction).map_err(|error| {
            format!(
                "failed to serialize {} fixture transaction {index}: {error}",
                kind.as_str()
            )
        })?;
        tx_bytes.push(bytes);
    }
    Ok(TxFixtures {
        name: kind.as_str(),
        tx_bytes,
    })
}

/// Returns true when the sanitized transaction view contains only vote instructions.
fn classify_vote_only(view: &SanitizedTransactionView<&[u8]>) -> bool {
    let mut has_vote = false;
    let mut has_non_vote_non_budget = false;
    for (program_id, _instruction) in view.program_instructions_iter() {
        if *program_id == solana_vote_program::id() {
            has_vote = true;
            continue;
        }
        if *program_id != solana_sdk_ids::compute_budget::id() {
            has_non_vote_non_budget = true;
        }
    }
    has_vote && !has_non_vote_non_budget
}

/// Mirrors only the signature-count sanitization from upstream transaction-view.
fn sanitize_signatures_only(view: &UnsanitizedTransactionView<&[u8]>) -> Result<(), &'static str> {
    if view.num_signatures() != view.num_required_signatures() {
        return Err("signature count mismatch");
    }
    if view.num_static_account_keys() < view.num_signatures() {
        return Err("not enough static keys");
    }
    Ok(())
}

/// Mirrors only the account-access sanitization from upstream transaction-view.
fn sanitize_account_access_only(
    view: &UnsanitizedTransactionView<&[u8]>,
) -> Result<(), &'static str> {
    if view.num_readonly_unsigned_static_accounts()
        > view
            .num_static_account_keys()
            .wrapping_sub(view.num_required_signatures())
    {
        return Err("readonly overlap");
    }
    if view.num_readonly_signed_static_accounts() >= view.num_required_signatures() {
        return Err("no writable fee payer");
    }
    if total_number_of_accounts(view) > 256 {
        return Err("too many accounts");
    }
    Ok(())
}

/// Mirrors only the instruction sanitization from upstream transaction-view.
fn sanitize_instructions_only(
    view: &UnsanitizedTransactionView<&[u8]>,
    enable_static_instruction_limit: bool,
) -> Result<(), &'static str> {
    if enable_static_instruction_limit
        && usize::from(view.num_instructions()) > MAX_INSTRUCTION_TRACE_LENGTH
    {
        return Err("too many instructions");
    }
    let max_program_id_index = view.num_static_account_keys().wrapping_sub(1);
    let max_account_index = total_number_of_accounts(view).wrapping_sub(1) as u8;
    for instruction in view.instructions_iter() {
        if instruction.program_id_index > max_program_id_index {
            return Err("program index out of bounds");
        }
        if instruction.program_id_index == 0 {
            return Err("fee payer as program");
        }
        for account_index in instruction.accounts.iter().copied() {
            if account_index > max_account_index {
                return Err("account index out of bounds");
            }
        }
    }
    Ok(())
}

/// Mirrors only the address-table lookup sanitization from upstream transaction-view.
fn sanitize_address_table_lookups_only(
    view: &UnsanitizedTransactionView<&[u8]>,
) -> Result<(), &'static str> {
    for address_table_lookup in view.address_table_lookup_iter() {
        if address_table_lookup.writable_indexes.is_empty()
            && address_table_lookup.readonly_indexes.is_empty()
        {
            return Err("empty address table lookup");
        }
    }
    Ok(())
}

/// Computes the total account count used by upstream transaction-view sanitization.
fn total_number_of_accounts(view: &UnsanitizedTransactionView<&[u8]>) -> u16 {
    u16::from(view.num_static_account_keys())
        .saturating_add(view.total_writable_lookup_accounts())
        .saturating_add(view.total_readonly_lookup_accounts())
}
