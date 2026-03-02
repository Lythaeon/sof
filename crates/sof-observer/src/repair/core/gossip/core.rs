use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    time::{Duration, Instant},
};

use arcshift::ArcShift;
use bincode::Options;
use serde::Deserialize;
use solana_gossip::{cluster_info::ClusterInfo, contact_info::Protocol};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::SIGNATURE_BYTES;
use thiserror::Error;
use tokio::net::UdpSocket;

use super::super::{
    MissingShredRequestKind, ParseRepairRequestError, ParsedRepairRequestKind, RepairRequestError,
    build_repair_request, parse_signed_repair_request, request::unix_timestamp_ms,
    signed_repair_request_time_window_ms,
};

const REPAIR_PING_TOKEN_SIZE: usize = 32;
const REPAIR_RESPONSE_SERIALIZED_PING_BYTES: usize =
    4 + 32 + REPAIR_PING_TOKEN_SIZE + SIGNATURE_BYTES;
const REPAIR_PROTOCOL_PONG_VARIANT: u32 = 7;
const SOL_LAMPORTS: u64 = 1_000_000_000;
// Heuristic scoring weights for ranking repair peers.
#[cfg(feature = "gossip-bootstrap")]
const REPAIR_PEER_RANK_DEFAULT_WITHOUT_RTT: i64 = 1_000;
#[cfg(feature = "gossip-bootstrap")]
const REPAIR_PEER_RANK_MAX_RTT_MS: u32 = 2_500;
#[cfg(feature = "gossip-bootstrap")]
const REPAIR_PEER_RANK_SEND_OK_CAP: u64 = 4_096;
#[cfg(feature = "gossip-bootstrap")]
const REPAIR_PEER_RANK_PING_OK_CAP: u64 = 4_096;
#[cfg(feature = "gossip-bootstrap")]
const REPAIR_PEER_RANK_SOURCE_HITS_CAP: u64 = 8_192;
#[cfg(feature = "gossip-bootstrap")]
const REPAIR_PEER_RANK_SOURCE_HIT_WEIGHT: i64 = 2;
#[cfg(feature = "gossip-bootstrap")]
const REPAIR_PEER_RANK_SEND_ERROR_CAP: u64 = 4_096;
#[cfg(feature = "gossip-bootstrap")]
const REPAIR_PEER_RANK_SEND_ERROR_WEIGHT: i64 = 8;
#[cfg(feature = "gossip-bootstrap")]
const REPAIR_PEER_WEIGHT_MIN: i64 = 1;
#[cfg(feature = "gossip-bootstrap")]
const REPAIR_PEER_WEIGHT_MAX: i64 = 100_000;
#[cfg(feature = "gossip-bootstrap")]
const REPAIR_PEER_RTT_EMA_PREV_WEIGHT: u64 = 7;
#[cfg(feature = "gossip-bootstrap")]
const REPAIR_PEER_RTT_EMA_TOTAL_WEIGHT: u64 = 8;
#[cfg(feature = "gossip-bootstrap")]
const REPAIR_PEER_STICKINESS_MS: u64 = 750;
#[cfg(feature = "gossip-bootstrap")]
const REPAIR_PEER_SWITCH_SCORE_MARGIN: i64 = 64;

#[derive(Debug, Error)]
pub enum GossipRepairClientError {
    #[error("failed to serialize repair pong variant tag: {source}")]
    SerializeRepairPongVariant { source: bincode::Error },
    #[error("failed to serialize repair pong payload: {source}")]
    SerializeRepairPongPayload { source: bincode::Error },
    #[error("failed to build repair request payload: {source}")]
    BuildRepairRequest { source: RepairRequestError },
    #[error("failed to send repair request to {addr}: {source}")]
    SendRepairRequest {
        addr: std::net::SocketAddr,
        source: std::io::Error,
    },
    #[error("failed to send repair pong to {addr}: {source}")]
    SendRepairPong {
        addr: std::net::SocketAddr,
        source: std::io::Error,
    },
    #[error("failed to parse signed repair request: {source}")]
    ParseSignedRepairRequest { source: ParseRepairRequestError },
    #[error("repair request shred index out of u32 range: {shred_index}; {source}")]
    RepairRequestIndexOutOfRange {
        shred_index: u64,
        source: std::num::TryFromIntError,
    },
    #[error("failed to send repair response packet to {addr}: {source}")]
    SendRepairResponse {
        addr: std::net::SocketAddr,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ServedRepairRequestKind {
    WindowIndex,
    HighestWindowIndex,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ServedRepairRequest {
    pub kind: ServedRepairRequestKind,
    pub slot: u64,
    pub requested_index: u64,
    pub served_index: Option<u32>,
    pub rate_limited: bool,
    pub rate_limited_by_peer: bool,
    pub rate_limited_by_bytes: bool,
    pub unstaked_sender: bool,
}

#[derive(Debug, Deserialize)]
enum RepairResponse {
    Ping(solana_gossip::ping_pong::Ping<REPAIR_PING_TOKEN_SIZE>),
}

fn serialize_repair_pong(
    pong: &solana_gossip::ping_pong::Pong,
) -> Result<Vec<u8>, GossipRepairClientError> {
    let mut payload = bincode::options()
        .with_fixint_encoding()
        .serialize(&REPAIR_PROTOCOL_PONG_VARIANT)
        .map_err(|source| GossipRepairClientError::SerializeRepairPongVariant { source })?;
    let serialized_pong = bincode::options()
        .with_fixint_encoding()
        .serialize(pong)
        .map_err(|source| GossipRepairClientError::SerializeRepairPongPayload { source })?;
    payload.extend_from_slice(&serialized_pong);
    Ok(payload)
}

#[derive(Debug, Clone, Copy)]
struct RepairPeer {
    pubkey: Pubkey,
    addr: std::net::SocketAddr,
    stake_lamports: u64,
}

#[cfg(feature = "gossip-bootstrap")]
#[derive(Debug, Clone, Copy)]
struct StickyRepairPeer {
    peer: RepairPeer,
    selected_at: Instant,
}

#[cfg(feature = "gossip-bootstrap")]
#[derive(Debug, Clone, Copy, Default)]
struct RepairPeerScore {
    last_ping_rtt_ms: Option<u32>,
    send_ok: u64,
    send_error: u64,
    ping_ok: u64,
    source_hits: u64,
}

#[cfg(feature = "gossip-bootstrap")]
impl RepairPeerScore {
    fn rank(self) -> i64 {
        // Base rank prefers lower RTT. If no RTT exists yet, start from a neutral baseline.
        let base = self
            .last_ping_rtt_ms
            .map_or(REPAIR_PEER_RANK_DEFAULT_WITHOUT_RTT, |rtt_ms| {
                i64::from(REPAIR_PEER_RANK_MAX_RTT_MS)
                    .saturating_sub(i64::from(rtt_ms.min(REPAIR_PEER_RANK_MAX_RTT_MS)))
            });
        let send_bonus =
            i64::try_from(self.send_ok.min(REPAIR_PEER_RANK_SEND_OK_CAP)).unwrap_or(i64::MAX);
        let ping_bonus =
            i64::try_from(self.ping_ok.min(REPAIR_PEER_RANK_PING_OK_CAP)).unwrap_or(i64::MAX);
        let source_bonus = i64::try_from(self.source_hits.min(REPAIR_PEER_RANK_SOURCE_HITS_CAP))
            .unwrap_or(i64::MAX)
            .saturating_mul(REPAIR_PEER_RANK_SOURCE_HIT_WEIGHT);
        let send_penalty = i64::try_from(self.send_error.min(REPAIR_PEER_RANK_SEND_ERROR_CAP))
            .unwrap_or(i64::MAX)
            .saturating_mul(REPAIR_PEER_RANK_SEND_ERROR_WEIGHT);
        base.saturating_add(send_bonus)
            .saturating_add(ping_bonus)
            .saturating_add(source_bonus)
            .saturating_sub(send_penalty)
    }

    fn weight(self) -> u64 {
        let rank = self.rank();
        let clamped = rank.clamp(REPAIR_PEER_WEIGHT_MIN, REPAIR_PEER_WEIGHT_MAX);
        u64::try_from(clamped).unwrap_or(REPAIR_PEER_WEIGHT_MIN as u64)
    }

    const fn note_send_ok(&mut self) {
        self.send_ok = self.send_ok.saturating_add(1);
    }

    const fn note_send_error(&mut self) {
        self.send_error = self.send_error.saturating_add(1);
    }

    fn note_ping_rtt(&mut self, rtt_ms: u32) {
        self.ping_ok = self.ping_ok.saturating_add(1);
        self.last_ping_rtt_ms = Some(self.last_ping_rtt_ms.map_or(rtt_ms, |previous| {
            // Smooth jitter with an EMA: next = (7 * prev + 1 * sample) / 8.
            let prev = u64::from(previous);
            let next = u64::from(rtt_ms);
            let blended = prev
                .saturating_mul(REPAIR_PEER_RTT_EMA_PREV_WEIGHT)
                .saturating_add(next)
                / REPAIR_PEER_RTT_EMA_TOTAL_WEIGHT;
            u32::try_from(blended).unwrap_or(u32::MAX)
        }));
    }

    fn note_source_hits(&mut self, hits: u16) {
        self.source_hits = self.source_hits.saturating_add(u64::from(hits));
    }

    fn note_source_hit(&mut self) {
        self.note_source_hits(1);
    }

    const fn decay(&mut self) {
        self.send_ok /= 2;
        self.send_error /= 2;
        self.ping_ok /= 2;
        self.source_hits /= 2;
    }
}

#[cfg(feature = "gossip-bootstrap")]
#[derive(Debug, Clone, Default)]
pub struct RepairPeerSnapshot {
    pub updated_at_ms: u64,
    pub total_candidates: usize,
    pub active_candidates: usize,
    pub known_pubkeys: Vec<[u8; 32]>,
    pub ranked_addrs: Vec<std::net::SocketAddr>,
}

#[cfg(feature = "gossip-bootstrap")]
#[derive(Debug)]
struct CachedPeers {
    updated_at: Instant,
    peers: Vec<RepairPeer>,
}

#[cfg(feature = "gossip-bootstrap")]
#[must_use]
pub const fn is_repair_response_ping_packet(packet: &[u8]) -> bool {
    packet.len() == REPAIR_RESPONSE_SERIALIZED_PING_BYTES
}

#[cfg(feature = "gossip-bootstrap")]
#[derive(Debug, Clone, Copy)]
pub struct GossipRepairClientConfig {
    pub peer_cache_ttl: Duration,
    pub peer_cache_capacity: usize,
    pub active_peer_count: usize,
    pub peer_sample_size: usize,
    pub serve_max_bytes_per_sec: usize,
    pub serve_unstaked_max_bytes_per_sec: usize,
    pub serve_max_requests_per_peer_per_sec: usize,
}

#[cfg(feature = "gossip-bootstrap")]
pub struct GossipRepairClient {
    cluster_info: std::sync::Arc<ClusterInfo>,
    socket: UdpSocket,
    keypair: std::sync::Arc<Keypair>,
    peers_by_slot: HashMap<u64, CachedPeers>,
    sticky_peer_by_slot: HashMap<u64, StickyRepairPeer>,
    peer_cache_ttl: Duration,
    peer_cache_capacity: usize,
    active_peer_count: usize,
    peer_sample_size: usize,
    serve_max_bytes_per_sec: usize,
    serve_unstaked_max_bytes_per_sec: usize,
    serve_max_requests_per_peer_per_sec: usize,
    peer_scores: HashMap<Pubkey, RepairPeerScore>,
    stake_by_pubkey: HashMap<Pubkey, u64>,
    addr_to_pubkey: HashMap<std::net::SocketAddr, Pubkey>,
    ip_to_pubkeys: HashMap<IpAddr, Vec<Pubkey>>,
    last_request_sent_at: HashMap<std::net::SocketAddr, Instant>,
    serve_window_started: Instant,
    serve_bytes_sent_in_window: usize,
    serve_unstaked_bytes_sent_in_window: usize,
    serve_requests_by_addr: HashMap<std::net::SocketAddr, usize>,
    peer_snapshot: ArcShift<RepairPeerSnapshot>,
    nonce_counter: u32,
    rr_counter: u64,
    last_score_decay: Instant,
}

#[cfg(feature = "gossip-bootstrap")]
#[path = "methods.rs"]
mod methods;

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Debug, Serialize)]
    enum LegacyRepairPongProtocol {
        LegacyWindowIndex,
        LegacyHighestWindowIndex,
        LegacyOrphan,
        LegacyWindowIndexWithNonce,
        LegacyHighestWindowIndexWithNonce,
        LegacyOrphanWithNonce,
        LegacyAncestorHashes,
        Pong(solana_gossip::ping_pong::Pong),
    }

    fn construct_all_legacy_pong_variants(pong: solana_gossip::ping_pong::Pong) {
        let all_variants = [
            LegacyRepairPongProtocol::LegacyWindowIndex,
            LegacyRepairPongProtocol::LegacyHighestWindowIndex,
            LegacyRepairPongProtocol::LegacyOrphan,
            LegacyRepairPongProtocol::LegacyWindowIndexWithNonce,
            LegacyRepairPongProtocol::LegacyHighestWindowIndexWithNonce,
            LegacyRepairPongProtocol::LegacyOrphanWithNonce,
            LegacyRepairPongProtocol::LegacyAncestorHashes,
            LegacyRepairPongProtocol::Pong(pong),
        ];
        std::hint::black_box(all_variants);
    }

    #[test]
    fn repair_pong_tagged_serialization_matches_legacy_enum() {
        let ping_signer = Keypair::new();
        let pong_signer = Keypair::new();
        let ping = solana_gossip::ping_pong::Ping::<REPAIR_PING_TOKEN_SIZE>::new(
            [5_u8; REPAIR_PING_TOKEN_SIZE],
            &ping_signer,
        );
        construct_all_legacy_pong_variants(solana_gossip::ping_pong::Pong::new(
            &ping,
            &pong_signer,
        ));
        let pong_for_actual = solana_gossip::ping_pong::Pong::new(&ping, &pong_signer);
        let pong_for_expected = solana_gossip::ping_pong::Pong::new(&ping, &pong_signer);
        let actual = match serialize_repair_pong(&pong_for_actual) {
            Ok(payload) => payload,
            Err(error) => panic!("repair pong serialization failed: {error}"),
        };
        let expected = match bincode::options()
            .with_fixint_encoding()
            .serialize(&LegacyRepairPongProtocol::Pong(pong_for_expected))
        {
            Ok(payload) => payload,
            Err(error) => panic!("legacy repair pong serialization failed: {error}"),
        };
        assert_eq!(actual, expected);
    }
}
