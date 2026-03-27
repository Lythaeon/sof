#![no_main]

use libfuzzer_sys::fuzz_target;
use solana_message::VersionedMessage;
use solana_pubkey::Pubkey;
use sof_solana_compat::{TxBuilder, TxMessageVersion};

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

fn take_u32(input: &mut &[u8]) -> Option<u32> {
    let bytes = take_bytes(input, 4)?;
    let bytes: [u8; 4] = bytes.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

fn take_u64(input: &mut &[u8]) -> Option<u64> {
    let bytes = take_bytes(input, 8)?;
    let bytes: [u8; 8] = bytes.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

fn take_pubkey(input: &mut &[u8]) -> Option<Pubkey> {
    let bytes = take_bytes(input, 32)?;
    let bytes: [u8; 32] = bytes.try_into().ok()?;
    Some(Pubkey::new_from_array(bytes))
}

fuzz_target!(|bytes: &[u8]| {
    let mut input = bytes;
    let payer = take_pubkey(&mut input).unwrap_or(Pubkey::new_from_array([1_u8; 32]));
    let mut builder = TxBuilder::new(payer);
    let mut expect_compute_limit = false;
    let mut expect_priority_fee = false;
    let mut expect_tip = false;
    let mut expected_version = TxMessageVersion::V0;

    let op_count = usize::from(take_u8(&mut input).unwrap_or(0) % 64);
    for _ in 0..op_count {
        let Some(op) = take_u8(&mut input) else {
            break;
        };
        match op % 8 {
            0 => {
                builder = builder.with_compute_unit_limit(take_u32(&mut input).unwrap_or(1));
                expect_compute_limit = true;
            }
            1 => {
                builder = builder.without_compute_unit_limit();
                expect_compute_limit = false;
            }
            2 => {
                builder =
                    builder.with_priority_fee_micro_lamports(take_u64(&mut input).unwrap_or(1));
                expect_priority_fee = true;
            }
            3 => {
                builder = builder.without_priority_fee_micro_lamports();
                expect_priority_fee = false;
            }
            4 => {
                builder = builder.tip_developer_lamports(take_u64(&mut input).unwrap_or(1));
                expect_tip = true;
            }
            5 => {
                let recipient = take_pubkey(&mut input).unwrap_or(Pubkey::new_from_array([2_u8; 32]));
                let lamports = take_u64(&mut input).unwrap_or(1);
                builder = builder.tip_to(recipient, lamports);
                expect_tip = true;
            }
            6 => {
                builder = builder.with_legacy_message();
                expected_version = TxMessageVersion::Legacy;
            }
            _ => {
                builder = builder.with_v0_message();
                expected_version = TxMessageVersion::V0;
            }
        }
    }

    let message = builder.build_message([9_u8; 32]);
    let instruction_count = match &message {
        VersionedMessage::Legacy(message) => {
            assert_eq!(expected_version, TxMessageVersion::Legacy);
            message.instructions.len()
        }
        VersionedMessage::V0(message) => {
            assert_eq!(expected_version, TxMessageVersion::V0);
            message.instructions.len()
        }
    };

    let expected_instruction_count =
        usize::from(expect_compute_limit) + usize::from(expect_priority_fee) + usize::from(expect_tip);
    assert_eq!(instruction_count, expected_instruction_count);
});
