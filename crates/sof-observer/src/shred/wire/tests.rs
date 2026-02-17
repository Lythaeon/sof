use super::{ParseError, ParsedShred, SIZE_OF_DATA_SHRED_HEADERS, parse_shred};

#[test]
fn parses_data_shred_header_and_payload() {
    let packet = build_data_shred_packet(42, 31, 0, 2, true, true, b"abc");
    let parsed = parse_shred(&packet).expect("parse should succeed");

    match parsed {
        ParsedShred::Data(data) => {
            assert_eq!(data.common.slot, 42);
            assert_eq!(data.common.index, 31);
            assert_eq!(data.common.fec_set_index, 0);
            assert_eq!(data.data_header.parent_offset, 2);
            assert_eq!(data.payload, b"abc");
            assert!(data.data_header.data_complete());
            assert!(data.data_header.last_in_slot());
        }
        ParsedShred::Code(_) => panic!("expected data shred"),
    }
}

#[test]
fn rejects_data_shred_with_declared_size_beyond_capacity() {
    let packet = build_data_shred_packet(42, 7, 0, 2, true, false, &vec![0_u8; 1100]);
    let error = parse_shred(&packet).expect_err("parse should fail");
    assert!(matches!(error, ParseError::InvalidDataSize(_)));
}

#[test]
fn rejects_coding_shred_with_invalid_position() {
    let packet = build_code_shred_packet(42, 9, 0, 32, 32, 32);
    let error = parse_shred(&packet).expect_err("parse should fail");
    assert!(matches!(error, ParseError::InvalidCodingHeader { .. }));
}

#[test]
fn accepts_misaligned_fec_set() {
    let packet = build_data_shred_packet(42, 5, 5, 2, false, false, b"abc");
    let parsed = parse_shred(&packet).expect("parse should succeed");
    assert!(matches!(parsed, ParsedShred::Data(_)));
}

#[test]
fn accepts_data_complete_at_non_boundary_index() {
    let packet = build_data_shred_packet(42, 10, 0, 2, true, false, b"abc");
    let parsed = parse_shred(&packet).expect("parse should succeed");
    match parsed {
        ParsedShred::Data(data) => {
            assert_eq!(data.common.index, 10);
            assert!(data.data_header.data_complete());
        }
        ParsedShred::Code(_) => panic!("expected data shred"),
    }
}

fn build_data_shred_packet(
    slot: u64,
    index: u32,
    fec_set_index: u32,
    parent_offset: u16,
    data_complete: bool,
    last_in_slot: bool,
    payload: &[u8],
) -> Vec<u8> {
    let total = SIZE_OF_DATA_SHRED_HEADERS.saturating_add(payload.len());
    let mut packet = vec![0_u8; super::SIZE_OF_DATA_SHRED_PAYLOAD];

    packet[64] = 0x90;
    packet[65..73].copy_from_slice(&slot.to_le_bytes());
    packet[73..77].copy_from_slice(&index.to_le_bytes());
    packet[77..79].copy_from_slice(&u16::to_le_bytes(1));
    packet[79..83].copy_from_slice(&fec_set_index.to_le_bytes());
    packet[83..85].copy_from_slice(&parent_offset.to_le_bytes());

    let mut flags = 0_u8;
    if data_complete {
        flags |= 0b0100_0000;
    }
    if last_in_slot {
        flags |= 0b1100_0000;
    }
    packet[85] = flags;

    let size = u16::try_from(total).expect("test packet too large");
    packet[86..88].copy_from_slice(&size.to_le_bytes());
    let payload_end = 88usize.saturating_add(payload.len());
    packet[88..payload_end].copy_from_slice(payload);
    packet
}

fn build_code_shred_packet(
    slot: u64,
    index: u32,
    fec_set_index: u32,
    num_data_shreds: u16,
    num_coding_shreds: u16,
    position: u16,
) -> Vec<u8> {
    let mut packet = vec![0_u8; super::SIZE_OF_CODING_SHRED_PAYLOAD];
    packet[64] = 0x60;
    packet[65..73].copy_from_slice(&slot.to_le_bytes());
    packet[73..77].copy_from_slice(&index.to_le_bytes());
    packet[77..79].copy_from_slice(&u16::to_le_bytes(1));
    packet[79..83].copy_from_slice(&fec_set_index.to_le_bytes());
    packet[83..85].copy_from_slice(&num_data_shreds.to_le_bytes());
    packet[85..87].copy_from_slice(&num_coding_shreds.to_le_bytes());
    packet[87..89].copy_from_slice(&position.to_le_bytes());
    packet
}
