use super::*;
use crate::{
    protocol::shred_wire::SIZE_OF_CODING_SHRED_PAYLOAD,
    relay::SharedRelayCache,
    shred::wire::{ParsedShredHeader, parse_shred_header},
};
use solana_keypair::signable::Signable;

impl GossipRepairClient {
    pub fn new(
        cluster_info: std::sync::Arc<ClusterInfo>,
        socket: UdpSocket,
        keypair: std::sync::Arc<Keypair>,
        config: GossipRepairClientConfig,
    ) -> Self {
        let now = Instant::now();
        let peer_snapshot = ArcShift::new(RepairPeerSnapshot {
            updated_at_ms: unix_timestamp_ms(),
            total_candidates: 0,
            active_candidates: 0,
            known_pubkeys: vec![cluster_info.id().to_bytes()],
            ranked_addrs: Vec::new(),
        });
        Self {
            cluster_info,
            socket,
            keypair,
            peers_by_slot: HashMap::new(),
            sticky_peer_by_slot: HashMap::new(),
            peer_cache_ttl: config.peer_cache_ttl,
            peer_cache_capacity: config.peer_cache_capacity.max(1),
            active_peer_count: config.active_peer_count.max(1),
            peer_sample_size: config.peer_sample_size.max(1),
            serve_max_bytes_per_sec: config.serve_max_bytes_per_sec.max(1),
            serve_unstaked_max_bytes_per_sec: config.serve_unstaked_max_bytes_per_sec.max(1),
            serve_max_requests_per_peer_per_sec: config.serve_max_requests_per_peer_per_sec.max(1),
            peer_scores: HashMap::new(),
            stake_by_pubkey: HashMap::new(),
            addr_to_pubkey: HashMap::new(),
            ip_to_pubkeys: HashMap::new(),
            last_request_sent_at: HashMap::new(),
            serve_window_started: now,
            serve_bytes_sent_in_window: 0,
            serve_unstaked_bytes_sent_in_window: 0,
            serve_requests_by_addr: HashMap::new(),
            peer_snapshot,
            nonce_counter: 1,
            rr_counter: 0,
            last_score_decay: Instant::now(),
        }
    }

    pub fn peer_snapshot_handle(&self) -> ArcShift<RepairPeerSnapshot> {
        self.peer_snapshot.clone()
    }

    pub fn refresh_peer_snapshot(&mut self, slot_hint: u64) -> (usize, usize) {
        self.refresh_peers(slot_hint);
        let snapshot = self.peer_snapshot.shared_get();
        (snapshot.total_candidates, snapshot.active_candidates)
    }

    pub fn note_shred_source(&mut self, source_addr: std::net::SocketAddr) -> usize {
        let mut updated = 0_usize;
        let mut seen = HashSet::new();
        if let Some(pubkey) = self.addr_to_pubkey.get(&source_addr).copied() {
            let _ = seen.insert(pubkey);
        }
        if let Some(pubkeys) = self.ip_to_pubkeys.get(&source_addr.ip()) {
            for pubkey in pubkeys {
                let _ = seen.insert(*pubkey);
            }
        }
        for pubkey in seen {
            self.peer_scores
                .entry(pubkey)
                .or_default()
                .note_source_hit();
            updated = updated.saturating_add(1);
        }
        updated
    }

    pub fn note_shred_sources(&mut self, source_addrs: &[(std::net::SocketAddr, u16)]) -> usize {
        let mut updated = 0_usize;
        for (source_addr, hits) in source_addrs.iter().copied() {
            let mut seen = HashSet::new();
            if let Some(pubkey) = self.addr_to_pubkey.get(&source_addr).copied() {
                let _ = seen.insert(pubkey);
            }
            if let Some(pubkeys) = self.ip_to_pubkeys.get(&source_addr.ip()) {
                for pubkey in pubkeys {
                    let _ = seen.insert(*pubkey);
                }
            }
            for pubkey in seen {
                self.peer_scores
                    .entry(pubkey)
                    .or_default()
                    .note_source_hits(hits);
                updated = updated.saturating_add(1);
            }
        }
        updated
    }

    pub async fn request_missing_shred(
        &mut self,
        slot: u64,
        index: u32,
        kind: MissingShredRequestKind,
    ) -> Result<Option<std::net::SocketAddr>, GossipRepairClientError> {
        let Some(peer) = self.pick_peer(slot, index) else {
            return Ok(None);
        };
        let nonce = self.next_nonce();
        let payload = build_repair_request(
            &self.keypair,
            peer.pubkey,
            slot,
            u64::from(index),
            nonce,
            kind,
        )
        .map_err(|source| GossipRepairClientError::BuildRepairRequest { source })?;
        match self.socket.send_to(&payload, peer.addr).await {
            Ok(_) => {
                self.peer_scores
                    .entry(peer.pubkey)
                    .or_default()
                    .note_send_ok();
                let _ = self.last_request_sent_at.insert(peer.addr, Instant::now());
                Ok(Some(peer.addr))
            }
            Err(error) => {
                self.peer_scores
                    .entry(peer.pubkey)
                    .or_default()
                    .note_send_error();
                Err(GossipRepairClientError::SendRepairRequest {
                    addr: peer.addr,
                    source: error,
                })
            }
        }
    }

    pub fn current_pubkeys(&self) -> Vec<[u8; 32]> {
        self.peer_snapshot.shared_get().known_pubkeys.clone()
    }

    pub async fn maybe_handle_response_ping(
        &mut self,
        packet: &[u8],
        from_addr: std::net::SocketAddr,
    ) -> Result<bool, GossipRepairClientError> {
        if !is_repair_response_ping_packet(packet) {
            return Ok(false);
        }
        let Ok(RepairResponse::Ping(ping)) = bincode::deserialize::<RepairResponse>(packet) else {
            return Ok(false);
        };
        if !ping.verify() {
            return Ok(false);
        }
        let pong = solana_gossip::ping_pong::Pong::new(&ping, self.keypair.as_ref());
        let payload = serialize_repair_pong(&pong)?;
        self.socket
            .send_to(&payload, from_addr)
            .await
            .map_err(|source| GossipRepairClientError::SendRepairPong {
                addr: from_addr,
                source,
            })?;
        self.note_peer_ping(from_addr, Instant::now());
        Ok(true)
    }

    pub async fn maybe_serve_repair_request(
        &mut self,
        packet: &[u8],
        from_addr: std::net::SocketAddr,
        relay_cache: Option<&SharedRelayCache>,
    ) -> Result<Option<ServedRepairRequest>, GossipRepairClientError> {
        let Some(request) = parse_signed_repair_request(
            packet,
            self.cluster_info.id(),
            unix_timestamp_ms(),
            signed_repair_request_time_window_ms(),
        )
        .map_err(|source| GossipRepairClientError::ParseSignedRepairRequest { source })?
        else {
            return Ok(None);
        };
        let requested_index = u32::try_from(request.shred_index).map_err(|source| {
            GossipRepairClientError::RepairRequestIndexOutOfRange {
                shred_index: request.shred_index,
                source,
            }
        })?;
        let kind = match request.kind {
            ParsedRepairRequestKind::WindowIndex => ServedRepairRequestKind::WindowIndex,
            ParsedRepairRequestKind::HighestWindowIndex => {
                ServedRepairRequestKind::HighestWindowIndex
            }
        };
        let now = Instant::now();
        let sender_stake_lamports = self.sender_stake_lamports(request.sender);
        let unstaked_sender = sender_stake_lamports == 0;
        if !self.reserve_serve_request_budget(from_addr, now) {
            return Ok(Some(ServedRepairRequest {
                kind,
                slot: request.slot,
                requested_index: request.shred_index,
                served_index: None,
                rate_limited: true,
                rate_limited_by_peer: true,
                rate_limited_by_bytes: false,
                unstaked_sender,
            }));
        }
        let response = match (relay_cache, request.kind) {
            (Some(cache), ParsedRepairRequestKind::WindowIndex) => cache
                .query_exact(request.slot, requested_index, now)
                .and_then(|bytes| build_repair_response_payload(&bytes, request.nonce))
                .map(|payload| (requested_index, payload)),
            (Some(cache), ParsedRepairRequestKind::HighestWindowIndex) => cache
                .query_highest_above(request.slot, requested_index, now)
                .and_then(|(index, bytes)| {
                    build_repair_response_payload(&bytes, request.nonce)
                        .map(|payload| (index, payload))
                }),
            (None, _) => None,
        };
        if let Some((served_index, payload)) = response {
            if !self.reserve_serve_bytes_budget(payload.len(), sender_stake_lamports, now) {
                return Ok(Some(ServedRepairRequest {
                    kind,
                    slot: request.slot,
                    requested_index: request.shred_index,
                    served_index: None,
                    rate_limited: true,
                    rate_limited_by_peer: false,
                    rate_limited_by_bytes: true,
                    unstaked_sender,
                }));
            }
            self.socket
                .send_to(&payload, from_addr)
                .await
                .map_err(|source| GossipRepairClientError::SendRepairResponse {
                    addr: from_addr,
                    source,
                })?;
            return Ok(Some(ServedRepairRequest {
                kind,
                slot: request.slot,
                requested_index: request.shred_index,
                served_index: Some(served_index),
                rate_limited: false,
                rate_limited_by_peer: false,
                rate_limited_by_bytes: false,
                unstaked_sender,
            }));
        }
        Ok(Some(ServedRepairRequest {
            kind,
            slot: request.slot,
            requested_index: request.shred_index,
            served_index: None,
            rate_limited: false,
            rate_limited_by_peer: false,
            rate_limited_by_bytes: false,
            unstaked_sender,
        }))
    }

    fn pick_peer(&mut self, slot: u64, index: u32) -> Option<RepairPeer> {
        self.refresh_peers(slot);
        let peers = self.peers_by_slot.get(&slot)?.peers.clone();
        if peers.is_empty() {
            return None;
        }
        let sampled_indexes = self.sample_peer_indexes(&peers, slot, index);
        let now = Instant::now();
        let best_sampled = sampled_indexes
            .into_iter()
            .filter_map(|candidate_index| {
                peers
                    .get(candidate_index)
                    .copied()
                    .map(|peer| (candidate_index, peer))
            })
            .max_by_key(|(_, peer)| self.peer_selection_rank(now, *peer))
            .map(|(_, peer)| peer)?;

        let sticky_peer = self.sticky_peer_by_slot.get(&slot).and_then(|sticky| {
            peers
                .iter()
                .copied()
                .find(|peer| peer.pubkey == sticky.peer.pubkey && peer.addr == sticky.peer.addr)
                .map(|peer| (peer, now.saturating_duration_since(sticky.selected_at)))
        });

        let selected = sticky_peer.map_or(best_sampled, |(sticky, sticky_age)| {
            let sticky_score = self.score_for(sticky.pubkey);
            let best_score = self.score_for(best_sampled.pubkey);
            if should_keep_sticky_peer(sticky_age, sticky_score, best_score) {
                sticky
            } else {
                best_sampled
            }
        });
        let _ = self.sticky_peer_by_slot.insert(
            slot,
            StickyRepairPeer {
                peer: selected,
                selected_at: now,
            },
        );
        Some(selected)
    }

    fn refresh_peers(&mut self, slot: u64) {
        self.decay_peer_scores();
        self.refresh_stake_map();
        self.peers_by_slot
            .retain(|_, cached| cached.updated_at.elapsed() < self.peer_cache_ttl);
        self.sticky_peer_by_slot.retain(|slot_key, sticky| {
            self.peers_by_slot.contains_key(slot_key)
                && sticky.selected_at.elapsed() < self.peer_cache_ttl
        });
        if self.peers_by_slot.len() > self.peer_cache_capacity {
            let mut keys: Vec<_> = self.peers_by_slot.keys().copied().collect();
            keys.sort_unstable_by_key(|key| {
                self.peers_by_slot
                    .get(key)
                    .map(|cached| cached.updated_at)
                    .unwrap_or_else(Instant::now)
            });
            let overflow = self
                .peers_by_slot
                .len()
                .saturating_sub(self.peer_cache_capacity);
            for key in keys.into_iter().take(overflow) {
                let _ = self.peers_by_slot.remove(&key);
                let _ = self.sticky_peer_by_slot.remove(&key);
            }
        }
        let should_refresh = self
            .peers_by_slot
            .get(&slot)
            .map(|cached| cached.updated_at.elapsed() >= self.peer_cache_ttl)
            .unwrap_or(true);
        if !should_refresh {
            return;
        }
        let mut candidates = self.collect_candidate_peers(slot);
        let total_candidates = candidates.len();
        candidates.sort_unstable_by(|left, right| {
            self.score_for(right.pubkey)
                .cmp(&self.score_for(left.pubkey))
                .then_with(|| left.pubkey.to_bytes().cmp(&right.pubkey.to_bytes()))
        });
        let mut peers = candidates.clone();
        if peers.len() > self.active_peer_count {
            peers.truncate(self.active_peer_count);
        }
        self.addr_to_pubkey.clear();
        self.ip_to_pubkeys.clear();
        for peer in &candidates {
            let _ = self.addr_to_pubkey.insert(peer.addr, peer.pubkey);
            self.ip_to_pubkeys
                .entry(peer.addr.ip())
                .or_default()
                .push(peer.pubkey);
            let _ = self.peer_scores.entry(peer.pubkey).or_default();
        }
        for pubkeys in self.ip_to_pubkeys.values_mut() {
            pubkeys.sort_unstable_by_key(Pubkey::to_bytes);
            pubkeys.dedup();
        }
        let _ = self.peers_by_slot.insert(
            slot,
            CachedPeers {
                updated_at: Instant::now(),
                peers: peers.clone(),
            },
        );
        self.publish_peer_snapshot(total_candidates, &peers);
    }

    fn decay_peer_scores(&mut self) {
        if self.last_score_decay.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_score_decay = Instant::now();
        self.peer_scores.retain(|_, score| {
            score.decay();
            score.send_ok > 0
                || score.send_error > 0
                || score.ping_ok > 0
                || score.source_hits > 0
                || score.last_ping_rtt_ms.is_some()
        });
    }

    fn collect_candidate_peers(&self, slot: u64) -> Vec<RepairPeer> {
        let mut seen = HashSet::new();
        let mut peers = Vec::new();
        for contact_info in self.cluster_info.repair_peers(slot) {
            let Some(addr) = contact_info.serve_repair(Protocol::UDP) else {
                continue;
            };
            let peer = RepairPeer {
                pubkey: *contact_info.pubkey(),
                addr,
                stake_lamports: self
                    .stake_by_pubkey
                    .get(contact_info.pubkey())
                    .copied()
                    .unwrap_or_default(),
            };
            if seen.insert((peer.pubkey, peer.addr)) {
                peers.push(peer);
            }
        }
        if peers.is_empty() {
            let self_pubkey = self.cluster_info.id();
            let self_shred_version = self.cluster_info.my_shred_version();
            for (contact_info, stake_lamports) in self.cluster_info.all_peers() {
                if contact_info.pubkey() == &self_pubkey
                    || contact_info.shred_version() != self_shred_version
                    || contact_info.tvu(Protocol::UDP).is_none()
                {
                    continue;
                }
                let Some(addr) = contact_info.serve_repair(Protocol::UDP) else {
                    continue;
                };
                let peer = RepairPeer {
                    pubkey: *contact_info.pubkey(),
                    addr,
                    stake_lamports,
                };
                if seen.insert((peer.pubkey, peer.addr)) {
                    peers.push(peer);
                }
            }
        }
        peers
    }

    fn publish_peer_snapshot(&mut self, total_candidates: usize, peers: &[RepairPeer]) {
        let mut known_pubkeys = Vec::new();
        known_pubkeys.push(self.cluster_info.id().to_bytes());
        for (contact_info, _) in self.cluster_info.all_peers() {
            known_pubkeys.push(contact_info.pubkey().to_bytes());
        }
        known_pubkeys.sort_unstable();
        known_pubkeys.dedup();
        self.peer_snapshot.update(RepairPeerSnapshot {
            updated_at_ms: unix_timestamp_ms(),
            total_candidates,
            active_candidates: peers.len(),
            known_pubkeys,
            ranked_addrs: peers.iter().map(|peer| peer.addr).collect(),
        });
    }

    fn note_peer_ping(&mut self, from_addr: std::net::SocketAddr, now: Instant) {
        let Some(sent_at) = self.last_request_sent_at.get(&from_addr).copied() else {
            return;
        };
        let elapsed_ms = now.saturating_duration_since(sent_at).as_millis();
        let rtt_ms = u32::try_from(elapsed_ms.min(u128::from(u32::MAX))).unwrap_or(u32::MAX);
        let Some(pubkey) = self.addr_to_pubkey.get(&from_addr).copied() else {
            return;
        };
        self.peer_scores
            .entry(pubkey)
            .or_default()
            .note_ping_rtt(rtt_ms);
    }

    fn score_for(&self, pubkey: Pubkey) -> i64 {
        self.peer_scores
            .get(&pubkey)
            .copied()
            .unwrap_or_default()
            .rank()
    }

    fn peer_selection_rank(
        &self,
        now: Instant,
        peer: RepairPeer,
    ) -> (i64, u64, u64, u64, [u8; 32]) {
        (
            self.score_for(peer.pubkey),
            self.weight_for(peer.pubkey),
            self.last_request_age_ms(now, peer.addr),
            peer.stake_lamports,
            peer.pubkey.to_bytes(),
        )
    }

    fn weight_for(&self, pubkey: Pubkey) -> u64 {
        self.peer_scores
            .get(&pubkey)
            .copied()
            .unwrap_or_default()
            .weight()
    }

    fn refresh_stake_map(&mut self) {
        self.stake_by_pubkey.clear();
        for (contact_info, stake_lamports) in self.cluster_info.all_peers() {
            let _ = self
                .stake_by_pubkey
                .insert(*contact_info.pubkey(), stake_lamports);
        }
    }

    fn sender_stake_lamports(&self, sender: Pubkey) -> u64 {
        self.stake_by_pubkey
            .get(&sender)
            .copied()
            .unwrap_or_default()
    }

    fn reset_serve_window_if_needed(&mut self, now: Instant) {
        if now.saturating_duration_since(self.serve_window_started) < Duration::from_secs(1) {
            return;
        }
        self.serve_window_started = now;
        self.serve_bytes_sent_in_window = 0;
        self.serve_unstaked_bytes_sent_in_window = 0;
        self.serve_requests_by_addr.clear();
    }

    fn reserve_serve_request_budget(
        &mut self,
        source_addr: std::net::SocketAddr,
        now: Instant,
    ) -> bool {
        self.reset_serve_window_if_needed(now);
        let requests = self.serve_requests_by_addr.entry(source_addr).or_default();
        if *requests >= self.serve_max_requests_per_peer_per_sec {
            return false;
        }
        *requests = requests.saturating_add(1);
        true
    }

    fn reserve_serve_bytes_budget(
        &mut self,
        response_bytes: usize,
        sender_stake_lamports: u64,
        now: Instant,
    ) -> bool {
        self.reset_serve_window_if_needed(now);
        let projected = self
            .serve_bytes_sent_in_window
            .saturating_add(response_bytes);
        if projected > self.serve_max_bytes_per_sec {
            return false;
        }
        if sender_stake_lamports == 0 {
            let projected_unstaked = self
                .serve_unstaked_bytes_sent_in_window
                .saturating_add(response_bytes);
            if projected_unstaked > self.serve_unstaked_max_bytes_per_sec {
                return false;
            }
            self.serve_unstaked_bytes_sent_in_window = projected_unstaked;
        }
        self.serve_bytes_sent_in_window = projected;
        true
    }

    fn sample_peer_indexes(&mut self, peers: &[RepairPeer], slot: u64, index: u32) -> Vec<usize> {
        if peers.is_empty() {
            return Vec::new();
        }
        self.rr_counter = self.rr_counter.wrapping_add(1);
        let sample_size = self.peer_sample_size.min(peers.len()).max(1);
        let mut seed = self
            .rr_counter
            .wrapping_add(slot)
            .wrapping_add(u64::from(index));
        let mut pool: Vec<(usize, u64)> = peers
            .iter()
            .enumerate()
            .map(|(peer_index, peer)| {
                (
                    peer_index,
                    peer.stake_lamports.saturating_div(SOL_LAMPORTS).max(1),
                )
            })
            .collect();
        let mut selected = Vec::with_capacity(sample_size);
        while selected.len() < sample_size && !pool.is_empty() {
            let total_weight = pool
                .iter()
                .fold(0_u64, |acc, (_, weight)| acc.saturating_add(*weight));
            if total_weight == 0 {
                for (peer_index, _) in pool {
                    selected.push(peer_index);
                    if selected.len() >= sample_size {
                        break;
                    }
                }
                break;
            }
            seed = mix_seed(seed);
            let mut target = seed.checked_rem(total_weight).unwrap_or(0);
            let mut picked_position = 0_usize;
            for (position, (_, weight)) in pool.iter().enumerate() {
                if target < *weight {
                    picked_position = position;
                    break;
                }
                target = target.saturating_sub(*weight);
            }
            let (peer_index, _) = pool.swap_remove(picked_position);
            selected.push(peer_index);
        }
        selected
    }

    fn last_request_age_ms(&self, now: Instant, addr: std::net::SocketAddr) -> u64 {
        self.last_request_sent_at
            .get(&addr)
            .copied()
            .map(|sent_at| {
                u64::try_from(now.saturating_duration_since(sent_at).as_millis())
                    .unwrap_or(u64::MAX)
            })
            .unwrap_or(u64::MAX)
    }

    fn next_nonce(&mut self) -> u32 {
        let nonce = self.nonce_counter;
        self.nonce_counter = self.nonce_counter.wrapping_add(1).max(1);
        nonce
    }
}

fn build_repair_response_payload(packet: &[u8], nonce: u32) -> Option<Vec<u8>> {
    let parsed = parse_shred_header(packet).ok()?;
    let shred_len = canonical_shred_len(&parsed);
    let shred = packet.get(..shred_len)?;
    let mut payload = Vec::with_capacity(shred_len.saturating_add(std::mem::size_of::<u32>()));
    payload.extend_from_slice(shred);
    payload.extend_from_slice(&nonce.to_le_bytes());
    Some(payload)
}

fn canonical_shred_len(parsed: &ParsedShredHeader) -> usize {
    match parsed {
        ParsedShredHeader::Data(data) => usize::from(data.data_header.size),
        ParsedShredHeader::Code(_) => SIZE_OF_CODING_SHRED_PAYLOAD,
    }
}

fn should_keep_sticky_peer(sticky_age: Duration, sticky_score: i64, best_score: i64) -> bool {
    if sticky_age > Duration::from_millis(REPAIR_PEER_STICKINESS_MS) {
        return false;
    }
    best_score.saturating_sub(sticky_score) < REPAIR_PEER_SWITCH_SCORE_MARGIN
}

const fn mix_seed(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sticky_peer_is_kept_within_window_when_score_gap_is_small() {
        assert!(should_keep_sticky_peer(
            Duration::from_millis(REPAIR_PEER_STICKINESS_MS),
            1_000,
            1_050,
        ));
    }

    #[test]
    fn sticky_peer_is_not_kept_after_window_expires() {
        assert!(!should_keep_sticky_peer(
            Duration::from_millis(REPAIR_PEER_STICKINESS_MS.saturating_add(1)),
            1_000,
            1_020,
        ));
    }

    #[test]
    fn sticky_peer_is_not_kept_when_score_gap_is_large() {
        assert!(!should_keep_sticky_peer(
            Duration::from_millis(REPAIR_PEER_STICKINESS_MS),
            1_000,
            1_000 + REPAIR_PEER_SWITCH_SCORE_MARGIN,
        ));
    }
}
