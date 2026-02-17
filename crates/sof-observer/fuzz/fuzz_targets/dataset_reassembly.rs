#![no_main]

use libfuzzer_sys::fuzz_target;
use sof::reassembly::dataset::DataSetReassembler;

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

fn take_u16(input: &mut &[u8]) -> Option<u16> {
    let bytes = take_bytes(input, 2)?;
    let bytes: [u8; 2] = bytes.try_into().ok()?;
    Some(u16::from_le_bytes(bytes))
}

fuzz_target!(|bytes: &[u8]| {
    let mut input = bytes;

    let max_tracked_slots = usize::from(take_u8(&mut input).unwrap_or(16) % 64).max(1);
    let tail_min_shreds = usize::from(take_u8(&mut input).unwrap_or(2) % 8).max(1);
    let op_count = usize::from(take_u8(&mut input).unwrap_or(0));

    let mut reassembler = DataSetReassembler::new(max_tracked_slots)
        .with_tail_min_shreds_without_anchor(tail_min_shreds);

    for _ in 0..op_count {
        let Some(slot_raw) = take_u16(&mut input) else {
            break;
        };
        let Some(index_raw) = take_u16(&mut input) else {
            break;
        };
        let Some(flags) = take_u8(&mut input) else {
            break;
        };
        let Some(payload_len) = take_u8(&mut input).map(usize::from) else {
            break;
        };
        let Some(payload) = take_bytes(&mut input, payload_len) else {
            break;
        };

        let slot = u64::from(slot_raw % 1024);
        let index = u32::from(index_raw);
        let data_complete = flags & 1 == 1;
        let last_in_slot = flags & 2 == 2;

        let completed = reassembler.ingest_data_shred_meta(
            slot,
            index,
            data_complete,
            last_in_slot,
            payload.to_vec(),
        );

        assert!(reassembler.tracked_slots() <= max_tracked_slots);
        for dataset in completed {
            assert_eq!(dataset.slot, slot);
            assert!(dataset.start_index <= dataset.end_index);

            let expected_len_u64 =
                u64::from(dataset.end_index).saturating_sub(u64::from(dataset.start_index)) + 1;
            let expected_len = usize::try_from(expected_len_u64).unwrap_or(usize::MAX);

            assert_eq!(dataset.serialized_shreds.len(), expected_len);
            assert!(!dataset.serialized_shreds.is_empty());
        }
    }
});
