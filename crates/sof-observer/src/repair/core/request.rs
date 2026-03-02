use std::time::{SystemTime, UNIX_EPOCH};

use bincode::Options;
use serde::{Deserialize, Serialize};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::{SIGNATURE_BYTES, Signature};
use solana_signer::Signer;
use thiserror::Error;

use super::MissingShredRequestKind;

const SIGNATURE_OFFSET: usize = 4;
const SIGNED_REPAIR_REQUEST_TIME_WINDOW_MS: u64 = 10 * 60 * 1_000;
const MIN_SIGNED_REPAIR_REQUEST_BYTES: usize = 4 + SIGNATURE_BYTES + 32 + 32 + 8 + 4 + 8 + 8;
const REPAIR_PROTOCOL_WINDOW_INDEX_VARIANT: u32 = 8;
const REPAIR_PROTOCOL_HIGHEST_WINDOW_INDEX_VARIANT: u32 = 9;

#[derive(Debug, Error)]
pub enum RepairRequestError {
    #[error("failed to serialize repair request variant tag: {source}")]
    SerializeVariant { source: bincode::Error },
    #[error("failed to serialize repair request payload: {source}")]
    SerializePayload { source: bincode::Error },
    #[error(
        "repair request payload too short to patch signature: payload_len={payload_len}, required_len={required_len}"
    )]
    PayloadTooShort {
        payload_len: usize,
        required_len: usize,
    },
    #[error(
        "repair request payload signature range out of bounds: start={signature_start}, end={signature_end}, payload_len={payload_len}"
    )]
    SignatureRangeOutOfBounds {
        signature_start: usize,
        signature_end: usize,
        payload_len: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairRequestHeader {
    signature: Signature,
    sender: Pubkey,
    recipient: Pubkey,
    timestamp: u64,
    nonce: u32,
}

#[derive(Debug, Serialize)]
struct WindowIndexRequest {
    header: RepairRequestHeader,
    slot: u64,
    shred_index: u64,
}

#[derive(Debug, Serialize)]
struct HighestWindowIndexRequest {
    header: RepairRequestHeader,
    slot: u64,
    shred_index: u64,
}

#[derive(Debug, Deserialize)]
enum TaggedRepairRequest {
    LegacyWindowIndex,
    LegacyHighestWindowIndex,
    LegacyOrphan,
    LegacyWindowIndexWithNonce,
    LegacyHighestWindowIndexWithNonce,
    LegacyOrphanWithNonce,
    LegacyAncestorHashes,
    Pong,
    WindowIndex {
        header: RepairRequestHeader,
        slot: u64,
        shred_index: u64,
    },
    HighestWindowIndex {
        header: RepairRequestHeader,
        slot: u64,
        shred_index: u64,
    },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ParsedRepairRequestKind {
    WindowIndex,
    HighestWindowIndex,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ParsedRepairRequest {
    pub kind: ParsedRepairRequestKind,
    pub sender: Pubkey,
    pub slot: u64,
    pub shred_index: u64,
    pub nonce: u32,
}

#[derive(Debug, Error)]
pub enum ParseRepairRequestError {
    #[error("failed to deserialize repair request payload: {source}")]
    Deserialize { source: bincode::Error },
    #[error(
        "repair request payload too short to verify signature: payload_len={payload_len}, required_len={required_len}"
    )]
    PayloadTooShort {
        payload_len: usize,
        required_len: usize,
    },
    #[error(
        "repair request recipient mismatch; expected={expected_recipient} actual={actual_recipient}"
    )]
    RecipientMismatch {
        expected_recipient: Pubkey,
        actual_recipient: Pubkey,
    },
    #[error("repair request signature verification failed for sender={sender}")]
    SignatureVerificationFailed { sender: Pubkey },
    #[error(
        "repair request timestamp outside max age; request_timestamp_ms={request_timestamp_ms} now_ms={now_ms} max_age_ms={max_age_ms}"
    )]
    TimestampOutsideMaxAge {
        request_timestamp_ms: u64,
        now_ms: u64,
        max_age_ms: u64,
    },
}

fn serialize_tagged_repair_request<T: Serialize>(
    variant: u32,
    payload: &T,
) -> Result<Vec<u8>, RepairRequestError> {
    let mut serialized = bincode::options()
        .with_fixint_encoding()
        .serialize(&variant)
        .map_err(|source| RepairRequestError::SerializeVariant { source })?;
    let serialized_payload = bincode::options()
        .with_fixint_encoding()
        .serialize(payload)
        .map_err(|source| RepairRequestError::SerializePayload { source })?;
    serialized.extend_from_slice(&serialized_payload);
    Ok(serialized)
}

pub fn build_repair_request(
    keypair: &Keypair,
    recipient: Pubkey,
    slot: u64,
    shred_index: u64,
    nonce: u32,
    kind: MissingShredRequestKind,
) -> Result<Vec<u8>, RepairRequestError> {
    let header = RepairRequestHeader {
        signature: Signature::default(),
        sender: keypair.pubkey(),
        recipient,
        timestamp: unix_timestamp_ms(),
        nonce,
    };
    let mut payload = match kind {
        MissingShredRequestKind::WindowIndex => {
            let request = WindowIndexRequest {
                header,
                slot,
                shred_index,
            };
            serialize_tagged_repair_request(REPAIR_PROTOCOL_WINDOW_INDEX_VARIANT, &request)?
        }
        MissingShredRequestKind::HighestWindowIndex => {
            let request = HighestWindowIndexRequest {
                header,
                slot,
                shred_index,
            };
            serialize_tagged_repair_request(REPAIR_PROTOCOL_HIGHEST_WINDOW_INDEX_VARIANT, &request)?
        }
    };
    let signature_start = SIGNATURE_OFFSET;
    let signature_end = signature_start.saturating_add(SIGNATURE_BYTES);
    if payload.len() < signature_end {
        return Err(RepairRequestError::PayloadTooShort {
            payload_len: payload.len(),
            required_len: signature_end,
        });
    }
    let (before_signature, rest) = payload.split_at(signature_start);
    let (_, after_signature) = rest.split_at(SIGNATURE_BYTES);
    let signable_data = [before_signature, after_signature].concat();
    let signature = keypair.sign_message(&signable_data);
    let payload_len = payload.len();
    let signature_slice = payload.get_mut(signature_start..signature_end).ok_or(
        RepairRequestError::SignatureRangeOutOfBounds {
            signature_start,
            signature_end,
            payload_len,
        },
    )?;
    signature_slice.copy_from_slice(signature.as_ref());
    Ok(payload)
}

#[must_use]
pub fn is_supported_repair_request_packet(packet: &[u8]) -> bool {
    if packet.len() < MIN_SIGNED_REPAIR_REQUEST_BYTES {
        return false;
    }
    let Some(variant_prefix) = packet.get(..4) else {
        return false;
    };
    let Ok(variant_bytes): Result<[u8; 4], _> = variant_prefix.try_into() else {
        return false;
    };
    let variant = u32::from_le_bytes(variant_bytes);
    matches!(
        variant,
        REPAIR_PROTOCOL_WINDOW_INDEX_VARIANT | REPAIR_PROTOCOL_HIGHEST_WINDOW_INDEX_VARIANT
    )
}

#[must_use]
pub const fn signed_repair_request_time_window_ms() -> u64 {
    SIGNED_REPAIR_REQUEST_TIME_WINDOW_MS
}

pub fn parse_signed_repair_request(
    packet: &[u8],
    expected_recipient: Pubkey,
    now_ms: u64,
    max_age_ms: u64,
) -> Result<Option<ParsedRepairRequest>, ParseRepairRequestError> {
    if !is_supported_repair_request_packet(packet) {
        return Ok(None);
    }
    let request = bincode::options()
        .with_fixint_encoding()
        .deserialize::<TaggedRepairRequest>(packet)
        .map_err(|source| ParseRepairRequestError::Deserialize { source })?;
    let (kind, header, slot, shred_index) = match request {
        TaggedRepairRequest::WindowIndex {
            header,
            slot,
            shred_index,
        } => (
            ParsedRepairRequestKind::WindowIndex,
            header,
            slot,
            shred_index,
        ),
        TaggedRepairRequest::HighestWindowIndex {
            header,
            slot,
            shred_index,
        } => (
            ParsedRepairRequestKind::HighestWindowIndex,
            header,
            slot,
            shred_index,
        ),
        TaggedRepairRequest::LegacyWindowIndex
        | TaggedRepairRequest::LegacyHighestWindowIndex
        | TaggedRepairRequest::LegacyOrphan
        | TaggedRepairRequest::LegacyWindowIndexWithNonce
        | TaggedRepairRequest::LegacyHighestWindowIndexWithNonce
        | TaggedRepairRequest::LegacyOrphanWithNonce
        | TaggedRepairRequest::LegacyAncestorHashes
        | TaggedRepairRequest::Pong => return Ok(None),
    };
    if header.recipient != expected_recipient {
        return Err(ParseRepairRequestError::RecipientMismatch {
            expected_recipient,
            actual_recipient: header.recipient,
        });
    }
    if now_ms.abs_diff(header.timestamp) > max_age_ms {
        return Err(ParseRepairRequestError::TimestampOutsideMaxAge {
            request_timestamp_ms: header.timestamp,
            now_ms,
            max_age_ms,
        });
    }
    let signature_end = SIGNATURE_OFFSET.saturating_add(SIGNATURE_BYTES);
    if packet.len() < signature_end {
        return Err(ParseRepairRequestError::PayloadTooShort {
            payload_len: packet.len(),
            required_len: signature_end,
        });
    }
    let Some(leading_data) = packet.get(..SIGNATURE_OFFSET) else {
        return Err(ParseRepairRequestError::PayloadTooShort {
            payload_len: packet.len(),
            required_len: SIGNATURE_OFFSET,
        });
    };
    let Some(trailing_data) = packet.get(signature_end..) else {
        return Err(ParseRepairRequestError::PayloadTooShort {
            payload_len: packet.len(),
            required_len: signature_end,
        });
    };
    let signed_data = [leading_data, trailing_data].concat();
    if !header
        .signature
        .verify(header.sender.as_ref(), &signed_data)
    {
        return Err(ParseRepairRequestError::SignatureVerificationFailed {
            sender: header.sender,
        });
    }
    Ok(Some(ParsedRepairRequest {
        kind,
        sender: header.sender,
        slot,
        shred_index,
        nonce: header.nonce,
    }))
}

pub fn build_window_index_request(
    keypair: &Keypair,
    recipient: Pubkey,
    slot: u64,
    shred_index: u64,
    nonce: u32,
) -> Result<Vec<u8>, RepairRequestError> {
    build_repair_request(
        keypair,
        recipient,
        slot,
        shred_index,
        nonce,
        MissingShredRequestKind::WindowIndex,
    )
}

pub(super) fn unix_timestamp_ms() -> u64 {
    let now = SystemTime::now();
    let Ok(duration) = now.duration_since(UNIX_EPOCH) else {
        return 0;
    };
    let millis = duration.as_millis();
    if millis > u128::from(u64::MAX) {
        u64::MAX
    } else {
        millis as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_signer::Signer;

    #[derive(Debug, Clone, Serialize)]
    struct LegacyRepairRequestHeader {
        signature: Signature,
        sender: Pubkey,
        recipient: Pubkey,
        timestamp: u64,
        nonce: u32,
    }

    #[derive(Debug, Serialize)]
    enum LegacyRepairProtocol {
        LegacyWindowIndex,
        LegacyHighestWindowIndex,
        LegacyOrphan,
        LegacyWindowIndexWithNonce,
        LegacyHighestWindowIndexWithNonce,
        LegacyOrphanWithNonce,
        LegacyAncestorHashes,
        Pong,
        WindowIndex {
            header: LegacyRepairRequestHeader,
            slot: u64,
            shred_index: u64,
        },
        HighestWindowIndex {
            header: LegacyRepairRequestHeader,
            slot: u64,
            shred_index: u64,
        },
        Orphan {
            header: LegacyRepairRequestHeader,
            slot: u64,
        },
        AncestorHashes {
            header: LegacyRepairRequestHeader,
            slot: u64,
        },
    }

    fn to_legacy_header(header: &RepairRequestHeader) -> LegacyRepairRequestHeader {
        LegacyRepairRequestHeader {
            signature: header.signature,
            sender: header.sender,
            recipient: header.recipient,
            timestamp: header.timestamp,
            nonce: header.nonce,
        }
    }

    fn construct_all_legacy_variants(header: &LegacyRepairRequestHeader) {
        let all_variants = [
            LegacyRepairProtocol::LegacyWindowIndex,
            LegacyRepairProtocol::LegacyHighestWindowIndex,
            LegacyRepairProtocol::LegacyOrphan,
            LegacyRepairProtocol::LegacyWindowIndexWithNonce,
            LegacyRepairProtocol::LegacyHighestWindowIndexWithNonce,
            LegacyRepairProtocol::LegacyOrphanWithNonce,
            LegacyRepairProtocol::LegacyAncestorHashes,
            LegacyRepairProtocol::Pong,
            LegacyRepairProtocol::WindowIndex {
                header: header.clone(),
                slot: 0,
                shred_index: 0,
            },
            LegacyRepairProtocol::HighestWindowIndex {
                header: header.clone(),
                slot: 0,
                shred_index: 0,
            },
            LegacyRepairProtocol::Orphan {
                header: header.clone(),
                slot: 0,
            },
            LegacyRepairProtocol::AncestorHashes {
                header: header.clone(),
                slot: 0,
            },
        ];
        std::hint::black_box(all_variants);
    }

    #[test]
    fn window_index_tagged_serialization_matches_legacy_enum() {
        let header = RepairRequestHeader {
            signature: Signature::default(),
            sender: Pubkey::new_unique(),
            recipient: Pubkey::new_unique(),
            timestamp: 42,
            nonce: 7,
        };
        let request = WindowIndexRequest {
            header: header.clone(),
            slot: 1_234,
            shred_index: 567,
        };
        let legacy_header = to_legacy_header(&header);
        construct_all_legacy_variants(&legacy_header);
        let actual =
            match serialize_tagged_repair_request(REPAIR_PROTOCOL_WINDOW_INDEX_VARIANT, &request) {
                Ok(payload) => payload,
                Err(error) => panic!("window index request serialization failed: {error}"),
            };
        let expected = match bincode::options().with_fixint_encoding().serialize(
            &LegacyRepairProtocol::WindowIndex {
                header: legacy_header,
                slot: 1_234,
                shred_index: 567,
            },
        ) {
            Ok(payload) => payload,
            Err(error) => panic!("legacy window index request serialization failed: {error}"),
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn highest_window_index_tagged_serialization_matches_legacy_enum() {
        let header = RepairRequestHeader {
            signature: Signature::default(),
            sender: Pubkey::new_unique(),
            recipient: Pubkey::new_unique(),
            timestamp: 77,
            nonce: 11,
        };
        let request = HighestWindowIndexRequest {
            header: header.clone(),
            slot: 9_876,
            shred_index: 543,
        };
        let legacy_header = to_legacy_header(&header);
        construct_all_legacy_variants(&legacy_header);
        let actual = match serialize_tagged_repair_request(
            REPAIR_PROTOCOL_HIGHEST_WINDOW_INDEX_VARIANT,
            &request,
        ) {
            Ok(payload) => payload,
            Err(error) => panic!("highest-window-index request serialization failed: {error}"),
        };
        let expected = match bincode::options().with_fixint_encoding().serialize(
            &LegacyRepairProtocol::HighestWindowIndex {
                header: legacy_header,
                slot: 9_876,
                shred_index: 543,
            },
        ) {
            Ok(payload) => payload,
            Err(error) => {
                panic!("legacy highest-window-index request serialization failed: {error}")
            }
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn parse_signed_window_index_request_roundtrips() {
        let requester = Keypair::new();
        let recipient = Keypair::new().pubkey();
        let payload = build_window_index_request(&requester, recipient, 123, 456, 9)
            .expect("request payload");
        let parsed = parse_signed_repair_request(
            &payload,
            recipient,
            unix_timestamp_ms(),
            signed_repair_request_time_window_ms(),
        )
        .expect("parse succeeds")
        .expect("request recognized");
        assert_eq!(parsed.kind, ParsedRepairRequestKind::WindowIndex);
        assert_eq!(parsed.sender, requester.pubkey());
        assert_eq!(parsed.slot, 123);
        assert_eq!(parsed.shred_index, 456);
        assert_eq!(parsed.nonce, 9);
    }

    #[test]
    fn parse_signed_request_rejects_wrong_recipient() {
        let requester = Keypair::new();
        let recipient = Keypair::new().pubkey();
        let wrong_recipient = Keypair::new().pubkey();
        let payload =
            build_window_index_request(&requester, recipient, 12, 3, 4).expect("request payload");
        let error = parse_signed_repair_request(
            &payload,
            wrong_recipient,
            unix_timestamp_ms(),
            signed_repair_request_time_window_ms(),
        )
        .expect_err("recipient mismatch must fail");
        assert!(matches!(
            error,
            ParseRepairRequestError::RecipientMismatch { .. }
        ));
    }

    #[test]
    fn parse_signed_request_rejects_timestamp_outside_window() {
        let requester = Keypair::new();
        let recipient = Keypair::new().pubkey();
        let payload =
            build_window_index_request(&requester, recipient, 12, 3, 4).expect("request payload");
        let too_late = unix_timestamp_ms()
            .saturating_add(signed_repair_request_time_window_ms().saturating_mul(2));
        let error = parse_signed_repair_request(
            &payload,
            recipient,
            too_late,
            signed_repair_request_time_window_ms(),
        )
        .expect_err("stale request must fail");
        assert!(matches!(
            error,
            ParseRepairRequestError::TimestampOutsideMaxAge { .. }
        ));
    }
}
