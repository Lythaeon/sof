use super::DataSetReassembler;

#[test]
fn completes_dataset_on_data_complete() {
    let mut reassembler = DataSetReassembler::new(16);
    let out0 = ingest(&mut reassembler, 7, 0, false, false, vec![0]);
    assert!(out0.is_empty());
    let out1 = ingest(&mut reassembler, 7, 1, true, false, vec![1]);
    assert_eq!(out1.len(), 1);
    assert_eq!(out1[0].serialized_shreds, vec![vec![0], vec![1]]);
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
    assert_eq!(out1[0].serialized_shreds, vec![vec![0], vec![1]]);
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
    assert_eq!(out[0].serialized_shreds, vec![vec![0], vec![1]]);
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
    assert_eq!(out1[0].serialized_shreds, vec![vec![2], vec![3]]);

    let out2 = ingest(&mut reassembler, 12, 0, false, false, vec![0]);
    assert!(out2.is_empty());
    let out3 = ingest(&mut reassembler, 12, 1, true, false, vec![1]);
    assert_eq!(out3.len(), 1);
    assert_eq!(out3[0].start_index, 0);
    assert_eq!(out3[0].end_index, 1);
    assert_eq!(out3[0].serialized_shreds, vec![vec![0], vec![1]]);
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
    assert_eq!(out1[0].serialized_shreds, vec![vec![5], vec![6]]);
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
    assert_eq!(out2[0].serialized_shreds, vec![vec![2], vec![3]]);
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
    assert_eq!(out[0].serialized_shreds, vec![vec![0], vec![1]]);
}

fn ingest(
    reassembler: &mut DataSetReassembler,
    slot: u64,
    index: u32,
    data_complete: bool,
    last_in_slot: bool,
    serialized_shred: Vec<u8>,
) -> Vec<super::CompletedDataSet> {
    reassembler.ingest_data_shred_meta(slot, index, data_complete, last_in_slot, serialized_shred)
}
