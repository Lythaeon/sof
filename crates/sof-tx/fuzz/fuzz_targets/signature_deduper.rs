#![no_main]

use std::time::{Duration, Instant};

use libfuzzer_sys::fuzz_target;
use solana_signature::Signature;
use sof_tx::routing::SignatureDeduper;

fn take_bytes<'a>(input: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    if input.len() < len {
        return None;
    }
    let (head, tail) = input.split_at(len);
    *input = tail;
    Some(head)
}

fn take_u8(input: &mut &[u8]) -> Option<u8> {
    take_bytes(input, 1).map(|bytes| bytes[0])
}

fn take_signature(input: &mut &[u8]) -> Option<Signature> {
    let bytes = take_bytes(input, 64)?;
    let bytes: [u8; 64] = bytes.try_into().ok()?;
    Some(Signature::from(bytes))
}

fuzz_target!(|bytes: &[u8]| {
    let mut input = bytes;
    let ttl_ms = u64::from(take_u8(&mut input).unwrap_or(1)).max(1);
    let mut deduper = SignatureDeduper::new(Duration::from_millis(ttl_ms));
    let now = Instant::now();
    let op_count = usize::from(take_u8(&mut input).unwrap_or(0) % 32);
    for op_index in 0..op_count {
        let signature = take_signature(&mut input).unwrap_or_else(|| Signature::from([0_u8; 64]));
        let at = now + Duration::from_millis(u64::try_from(op_index).unwrap_or(0));
        let _ = deduper.check_and_insert(signature.into(), at);
        assert!(deduper.len() <= op_count);
    }
});
