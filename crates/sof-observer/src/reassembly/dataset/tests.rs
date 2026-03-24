use super::DataSetReassembler;
use super::PayloadFragmentBatch;
use std::time::{Duration, Instant};

#[test]
fn completes_dataset_on_data_complete() {
    let mut reassembler = DataSetReassembler::new(16);
    let out0 = ingest(&mut reassembler, 7, 0, false, false, vec![0]);
    assert!(out0.is_empty());
    let out1 = ingest(&mut reassembler, 7, 1, true, false, vec![1]);
    assert_eq!(out1.len(), 1);
    assert_eq!(
        out1[0].payload_fragments,
        PayloadFragmentBatch::from_owned_fragments(vec![vec![0], vec![1]])
    );
    assert_eq!(out1[0].start_index, 0);
    assert_eq!(out1[0].end_index, 1);
}

#[test]
fn handles_out_of_order_fragments() {
    let mut reassembler = DataSetReassembler::new(16);
    let out0 = ingest(&mut reassembler, 9, 1, true, false, vec![1]);
    assert!(out0.is_empty());
    let out1 = ingest(&mut reassembler, 9, 0, false, false, vec![0]);
    assert_eq!(out1.len(), 1);
    assert_eq!(out1[0].start_index, 0);
    assert_eq!(out1[0].end_index, 1);
    assert_eq!(
        out1[0].payload_fragments,
        PayloadFragmentBatch::from_owned_fragments(vec![vec![0], vec![1]])
    );
}

#[test]
fn suppresses_single_shred_tail_without_anchor() {
    let mut reassembler = DataSetReassembler::new(16);
    let out = ingest(&mut reassembler, 10, 1, true, false, vec![1]);
    assert!(out.is_empty());
}

#[test]
fn reanchors_after_late_prefix_arrival() {
    let mut reassembler = DataSetReassembler::new(16);
    let _ = ingest(&mut reassembler, 11, 1, true, false, vec![1]);
    let out = ingest(&mut reassembler, 11, 0, false, false, vec![0]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].start_index, 0);
    assert_eq!(out[0].end_index, 1);
    assert_eq!(
        out[0].payload_fragments,
        PayloadFragmentBatch::from_owned_fragments(vec![vec![0], vec![1]])
    );
}

#[test]
fn reanchors_on_consecutive_completed_boundaries() {
    let mut reassembler = DataSetReassembler::new(16);
    let out0 = ingest(&mut reassembler, 12, 2, false, false, vec![2]);
    assert!(out0.is_empty());
    let out1 = ingest(&mut reassembler, 12, 3, true, false, vec![3]);
    assert_eq!(out1.len(), 1);
    assert_eq!(out1[0].start_index, 2);
    assert_eq!(out1[0].end_index, 3);
    assert_eq!(
        out1[0].payload_fragments,
        PayloadFragmentBatch::from_owned_fragments(vec![vec![2], vec![3]])
    );

    let out2 = ingest(&mut reassembler, 12, 0, false, false, vec![0]);
    assert!(out2.is_empty());
    let out3 = ingest(&mut reassembler, 12, 1, true, false, vec![1]);
    assert_eq!(out3.len(), 1);
    assert_eq!(out3[0].start_index, 0);
    assert_eq!(out3[0].end_index, 1);
    assert_eq!(
        out3[0].payload_fragments,
        PayloadFragmentBatch::from_owned_fragments(vec![vec![0], vec![1]])
    );
}

#[test]
fn emits_tail_without_known_boundary_anchor() {
    let mut reassembler = DataSetReassembler::new(16);
    let out0 = ingest(&mut reassembler, 15, 5, false, false, vec![5]);
    assert!(out0.is_empty());
    let out1 = ingest(&mut reassembler, 15, 6, true, false, vec![6]);
    assert_eq!(out1.len(), 1);
    assert_eq!(out1[0].start_index, 5);
    assert_eq!(out1[0].end_index, 6);
    assert_eq!(
        out1[0].payload_fragments,
        PayloadFragmentBatch::from_owned_fragments(vec![vec![5], vec![6]])
    );
}

#[test]
fn emits_following_dataset_with_missing_prefix_dataset() {
    let mut reassembler = DataSetReassembler::new(16);
    let out0 = ingest(&mut reassembler, 30, 1, true, false, vec![1]);
    assert!(out0.is_empty());
    let out1 = ingest(&mut reassembler, 30, 2, false, false, vec![2]);
    assert!(out1.is_empty());
    let out2 = ingest(&mut reassembler, 30, 3, true, false, vec![3]);
    assert_eq!(out2.len(), 1);
    assert_eq!(out2[0].start_index, 2);
    assert_eq!(out2[0].end_index, 3);
    assert_eq!(
        out2[0].payload_fragments,
        PayloadFragmentBatch::from_owned_fragments(vec![vec![2], vec![3]])
    );
}

#[test]
fn emits_missing_prefix_dataset_if_gap_arrives_later() {
    let mut reassembler = DataSetReassembler::new(16);
    let _ = ingest(&mut reassembler, 31, 1, true, false, vec![1]);
    let _ = ingest(&mut reassembler, 31, 2, false, false, vec![2]);
    let _ = ingest(&mut reassembler, 31, 3, true, false, vec![3]);

    let out = ingest(&mut reassembler, 31, 0, false, false, vec![0]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].start_index, 0);
    assert_eq!(out[0].end_index, 1);
    assert_eq!(
        out[0].payload_fragments,
        PayloadFragmentBatch::from_owned_fragments(vec![vec![0], vec![1]])
    );
}

#[test]
fn late_prefix_after_last_boundary_still_finishes_slot() {
    let mut reassembler = DataSetReassembler::new(16);
    let _ = ingest(&mut reassembler, 40, 1, true, false, vec![1]);
    let tail_out = ingest(&mut reassembler, 40, 2, false, true, vec![2]);
    assert_eq!(tail_out.len(), 1);
    assert_eq!(tail_out[0].start_index, 2);
    assert_eq!(tail_out[0].end_index, 2);
    assert_eq!(reassembler.tracked_slots(), 1);

    let prefix_out = ingest(&mut reassembler, 40, 0, false, false, vec![0]);
    assert_eq!(prefix_out.len(), 1);
    assert_eq!(prefix_out[0].start_index, 0);
    assert_eq!(prefix_out[0].end_index, 1);
    assert_eq!(reassembler.tracked_slots(), 0);
}

#[test]
fn completed_dataset_tracks_first_and_last_shred_observed_times() {
    let mut reassembler = DataSetReassembler::new(16);
    let first = Instant::now();
    let Some(second) = first.checked_add(Duration::from_millis(2)) else {
        panic!("expected later instant");
    };
    let out0 = reassembler.ingest_data_shred_meta_at(
        50,
        0,
        false,
        false,
        super::SharedPayloadFragment::owned(vec![0]),
        first,
    );
    assert!(out0.is_empty());
    let out1 = reassembler.ingest_data_shred_meta_at(
        50,
        1,
        true,
        false,
        super::SharedPayloadFragment::owned(vec![1]),
        second,
    );
    assert_eq!(out1.len(), 1);
    assert_eq!(out1[0].first_shred_observed_at, first);
    assert_eq!(out1[0].last_shred_observed_at, second);
}

#[test]
fn inline_contiguous_dataset_tracks_open_prefix_after_completed_boundary() {
    let mut reassembler = DataSetReassembler::new(16);
    let first = Instant::now();
    let Some(second) = first.checked_add(Duration::from_millis(1)) else {
        panic!("expected later instant");
    };
    let Some(third) = second.checked_add(Duration::from_millis(1)) else {
        panic!("expected later instant");
    };
    let Some(fourth) = third.checked_add(Duration::from_millis(1)) else {
        panic!("expected later instant");
    };

    let out0 = reassembler.ingest_data_shred_meta_at(
        60,
        0,
        false,
        false,
        super::SharedPayloadFragment::owned(vec![0]),
        first,
    );
    assert!(out0.is_empty());
    let out1 = reassembler.ingest_data_shred_meta_at(
        60,
        1,
        true,
        false,
        super::SharedPayloadFragment::owned(vec![1]),
        second,
    );
    assert_eq!(out1.len(), 1);

    let out2 = reassembler.ingest_data_shred_meta_at(
        60,
        2,
        false,
        false,
        super::SharedPayloadFragment::owned(vec![2]),
        third,
    );
    assert!(out2.is_empty());
    let out3 = reassembler.ingest_data_shred_meta_at(
        60,
        3,
        false,
        false,
        super::SharedPayloadFragment::owned(vec![3]),
        fourth,
    );
    assert!(out3.is_empty());

    let inline = reassembler
        .inline_contiguous_dataset(60)
        .expect("inline contiguous dataset");
    assert_eq!(inline.start_index, 2);
    assert_eq!(inline.end_index, 3);
    assert_eq!(
        inline.payload_fragments,
        PayloadFragmentBatch::from_owned_fragments(vec![vec![2], vec![3]])
    );
    assert_eq!(inline.fragment_observed_ats, vec![third, fourth]);
}

#[test]
fn inline_contiguous_dataset_requires_anchor() {
    let mut reassembler = DataSetReassembler::new(16);
    let _ = ingest(&mut reassembler, 61, 2, false, false, vec![2]);
    let _ = ingest(&mut reassembler, 61, 3, false, false, vec![3]);
    assert!(reassembler.inline_contiguous_datasets(61).is_empty());
}

#[test]
fn inline_contiguous_datasets_include_following_chain_after_known_boundary() {
    let mut reassembler = DataSetReassembler::new(16);
    let _ = ingest(&mut reassembler, 62, 1, true, false, vec![1]);
    let _ = ingest(&mut reassembler, 62, 2, false, false, vec![2]);
    let _ = ingest(&mut reassembler, 62, 3, false, false, vec![3]);

    let inline = reassembler.inline_contiguous_datasets(62);
    assert_eq!(inline.len(), 1);
    assert_eq!(inline[0].start_index, 2);
    assert_eq!(inline[0].end_index, 3);
    assert_eq!(
        inline[0].payload_fragments,
        PayloadFragmentBatch::from_owned_fragments(vec![vec![2], vec![3]])
    );
}

#[test]
fn inline_contiguous_datasets_track_late_prefix_and_current_chain() {
    let mut reassembler = DataSetReassembler::new(16);
    let _ = ingest(&mut reassembler, 63, 2, false, false, vec![2]);
    let _ = ingest(&mut reassembler, 63, 3, true, false, vec![3]);
    let _ = ingest(&mut reassembler, 63, 4, false, false, vec![4]);
    let _ = ingest(&mut reassembler, 63, 5, false, false, vec![5]);
    let _ = ingest(&mut reassembler, 63, 0, false, false, vec![0]);

    let inline = reassembler.inline_contiguous_datasets(63);
    assert_eq!(inline.len(), 2);
    assert_eq!(inline[0].start_index, 0);
    assert_eq!(inline[0].end_index, 0);
    assert_eq!(
        inline[0].payload_fragments,
        PayloadFragmentBatch::from_owned_fragments(vec![vec![0]])
    );
    assert_eq!(inline[1].start_index, 4);
    assert_eq!(inline[1].end_index, 5);
    assert_eq!(
        inline[1].payload_fragments,
        PayloadFragmentBatch::from_owned_fragments(vec![vec![4], vec![5]])
    );
}

fn ingest(
    reassembler: &mut DataSetReassembler,
    slot: u64,
    index: u32,
    data_complete: bool,
    last_in_slot: bool,
    serialized_shred: Vec<u8>,
) -> Vec<super::CompletedDataSet> {
    reassembler.ingest_data_shred_meta(
        slot,
        index,
        data_complete,
        last_in_slot,
        super::SharedPayloadFragment::owned(serialized_shred),
    )
}
