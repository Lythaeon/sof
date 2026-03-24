//! Standalone profiler for dataset fragment join and entry decode.

use std::time::Instant;

use sof::reassembly::dataset::PayloadFragmentBatch;
use solana_entry::entry::{Entry, MaxDataShredsLen};
use solana_hash::Hash;
use wincode::{
    Deserialize as _, Serialize as _,
    containers::{Elem, Vec as WincodeVec},
};

/// Fixed fragment count used by the fragmented fixture.
const FRAGMENT_COUNT: usize = 32;
/// Default profile iterations when not overridden by the environment.
const DEFAULT_ITERATIONS: usize = 20_000;
/// Number of synthetic entries per payload fixture.
const ENTRY_COUNT: usize = 64;
/// Bytes in the synthetic junk prefix used for suffix-retry profiling.
const PREFIX_JUNK_LEN: usize = 96;

/// One decode fixture plus its reusable scratch buffer.
struct DecodeFixture {
    /// Stable fixture name used in profiler output and filtering.
    name: &'static str,
    /// Reusable payload fragments for the decode path under test.
    batch: PayloadFragmentBatch,
    /// Scratch buffer reused across iterations.
    scratch: Vec<u8>,
}

impl DecodeFixture {
    /// Creates one fixture with scratch capacity sized to the full payload.
    fn new(name: &'static str, batch: PayloadFragmentBatch) -> Self {
        let scratch = Vec::with_capacity(batch.total_len());
        Self {
            name,
            batch,
            scratch,
        }
    }
}

/// Entry point for the standalone decode profiler.
fn main() -> Result<(), String> {
    let iterations = std::env::var("SOF_DATASET_DECODE_PROFILE_ITERS")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_ITERATIONS);
    let selected_fixture = std::env::args().nth(1);

    let payload = build_entry_payload(ENTRY_COUNT)?;
    let fixtures = [
        DecodeFixture::new(
            "single_fragment_entries_64",
            PayloadFragmentBatch::from_owned_fragments(vec![payload.clone()]),
        ),
        DecodeFixture::new(
            "fragmented_entries_64_fragments_32",
            PayloadFragmentBatch::from_owned_fragments(split_payload(&payload, FRAGMENT_COUNT)),
        ),
        DecodeFixture::new(
            "prefixed_retry_entries_64_fragments_33",
            PayloadFragmentBatch::from_owned_fragments(prefixed_payload_fragments(
                &payload,
                FRAGMENT_COUNT,
                PREFIX_JUNK_LEN,
            )),
        ),
    ];

    let mut matched_fixture = false;
    for fixture in fixtures {
        if let Some(selected_fixture) = selected_fixture.as_deref()
            && fixture.name != selected_fixture
        {
            continue;
        }
        matched_fixture = true;
        let result = profile_fixture(fixture, iterations)?;
        let ns_per_iteration_whole = result.ns_per_iteration_x1000 / 1000;
        let ns_per_iteration_fraction = result.ns_per_iteration_x1000 % 1000;
        println!(
            "{} iterations={} ns_per_iteration={}.{:03} decoded_entries={} skipped_prefix_shreds={}",
            result.name,
            iterations,
            ns_per_iteration_whole,
            ns_per_iteration_fraction,
            result.decoded_entries,
            result.skipped_prefix_shreds,
        );
    }
    if !matched_fixture {
        return Err(format!(
            "unknown fixture {:?}; expected one of single_fragment_entries_64, fragmented_entries_64_fragments_32, prefixed_retry_entries_64_fragments_33",
            selected_fixture,
        ));
    }

    Ok(())
}

/// One measured fixture result.
struct DecodeProfileResult {
    /// Stable fixture name used in profiler output.
    name: &'static str,
    /// Nanoseconds per iteration scaled by 1000 for fixed-point formatting.
    ns_per_iteration_x1000: u128,
    /// Number of entries recovered from the final successful decode.
    decoded_entries: usize,
    /// Number of leading prefix shreds skipped before decoding succeeded.
    skipped_prefix_shreds: u32,
}

/// Profiles one decode fixture for the requested iteration count.
fn profile_fixture(
    mut fixture: DecodeFixture,
    iterations: usize,
) -> Result<DecodeProfileResult, String> {
    let mut decoded_entries = 0_usize;
    let mut skipped_prefix_shreds = 0_u32;
    let started_at = Instant::now();
    for _ in 0..iterations {
        let (entries, _, skipped_prefix) =
            decode_entries_from_payload_fragments(&fixture.batch, &mut fixture.scratch)
                .ok_or_else(|| format!("failed to decode fixture {}", fixture.name))?;
        decoded_entries = entries.len();
        skipped_prefix_shreds = skipped_prefix;
    }
    let elapsed = started_at.elapsed();
    let ns_per_iteration_x1000 = elapsed
        .as_nanos()
        .saturating_mul(1000)
        .checked_div(u128::try_from(iterations).ok().unwrap_or(1))
        .ok_or_else(|| format!("invalid iteration count for fixture {}", fixture.name))?;
    Ok(DecodeProfileResult {
        name: fixture.name,
        ns_per_iteration_x1000,
        decoded_entries,
        skipped_prefix_shreds,
    })
}

/// Builds one serialized entry payload with deterministic synthetic entries.
fn build_entry_payload(entry_count: usize) -> Result<Vec<u8>, String> {
    let mut entries = Vec::with_capacity(entry_count);
    let mut previous_hash = Hash::new_from_array([7_u8; 32]);
    for index in 0..entry_count {
        let entry = Entry::new(&previous_hash, 1, Vec::new());
        let index_byte = u8::try_from(index & 0xff).unwrap_or(u8::MAX);
        previous_hash = Hash::new_from_array([index_byte; 32]);
        entries.push(entry);
    }
    WincodeVec::<Elem<Entry>, MaxDataShredsLen>::serialize(&entries)
        .map_err(|error| format!("failed to serialize entry payload: {error}"))
}

/// Splits one payload into roughly even fragments.
fn split_payload(payload: &[u8], fragment_count: usize) -> Vec<Vec<u8>> {
    let fragment_count = fragment_count.max(1);
    let chunk_len = payload.len().div_ceil(fragment_count).max(1);
    payload.chunks(chunk_len).map(ToOwned::to_owned).collect()
}

/// Prepends one junk fragment before the valid serialized payload fragments.
fn prefixed_payload_fragments(
    payload: &[u8],
    fragment_count: usize,
    junk_len: usize,
) -> Vec<Vec<u8>> {
    let mut fragments = Vec::with_capacity(fragment_count.saturating_add(1));
    // Use an invalid wincode prefix so the runtime-style suffix retry path is exercised.
    fragments.push(vec![0xff_u8; junk_len.max(1)]);
    fragments.extend(split_payload(payload, fragment_count));
    fragments
}

/// Mirrors the runtime dataset decode path for profiling.
fn decode_entries_from_payload_fragments(
    payload_fragments: &PayloadFragmentBatch,
    scratch_payload: &mut Vec<u8>,
) -> Option<(Vec<Entry>, usize, u32)> {
    let total_payload_len = payload_fragments.total_len();
    if total_payload_len == 0 {
        return Some((Vec::new(), 0, 0));
    }
    if payload_fragments.len() > 1 {
        let all_fragments = payload_fragments.slice_from(0)?;
        join_payload_fragments_into(scratch_payload, all_fragments, total_payload_len);
    }
    for skipped_prefix in 0..payload_fragments.len() {
        let payload_len = payload_fragments.total_len_from(skipped_prefix)?;
        if let Some(fragment) = payload_fragments.single_fragment_from(skipped_prefix) {
            let payload = fragment.as_slice();
            if payload.is_empty() {
                let skipped_prefix_shreds = u32::try_from(skipped_prefix).ok()?;
                return Some((Vec::new(), 0, skipped_prefix_shreds));
            }
            let entries = <WincodeVec<Elem<Entry>, MaxDataShredsLen>>::deserialize(payload).ok();
            if let Some(entries) = entries {
                let skipped_prefix_shreds = u32::try_from(skipped_prefix).ok()?;
                return Some((entries, payload_len, skipped_prefix_shreds));
            }
            continue;
        }
        let Some(candidate) = payload_fragments.slice_from(skipped_prefix) else {
            continue;
        };
        let payload = if skipped_prefix == 0 {
            scratch_payload.as_slice()
        } else {
            let skipped_bytes = total_payload_len.saturating_sub(payload_len);
            scratch_payload.get(skipped_bytes..).unwrap_or_default()
        };
        if payload.is_empty() {
            let skipped_prefix_shreds = u32::try_from(skipped_prefix).ok()?;
            return Some((Vec::new(), 0, skipped_prefix_shreds));
        }
        debug_assert_eq!(
            candidate.len(),
            payload_fragments.len().saturating_sub(skipped_prefix)
        );
        let entries = <WincodeVec<Elem<Entry>, MaxDataShredsLen>>::deserialize(payload).ok();
        if let Some(entries) = entries {
            let skipped_prefix_shreds = u32::try_from(skipped_prefix).ok()?;
            return Some((entries, payload_len, skipped_prefix_shreds));
        }
    }
    None
}

/// Reuses one scratch buffer when joining fragmented payloads.
fn join_payload_fragments_into(
    buffer: &mut Vec<u8>,
    fragments: &[sof::reassembly::dataset::SharedPayloadFragment],
    payload_len: usize,
) {
    buffer.clear();
    if buffer.capacity() < payload_len {
        buffer.reserve(payload_len.saturating_sub(buffer.capacity()));
    }
    for fragment in fragments {
        buffer.extend_from_slice(fragment.as_slice());
    }
}
