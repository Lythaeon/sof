use super::*;

impl GossipRepairClient {
    pub fn new(
        cluster_info: std::sync::Arc<ClusterInfo>,
        socket: UdpSocket,
        keypair: std::sync::Arc<Keypair>,
        peer_cache_ttl: Duration,
        peer_cache_capacity: usize,
        active_peer_count: usize,
    ) -> Self {
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
            peer_cache_ttl,
            peer_cache_capacity: peer_cache_capacity.max(1),
            active_peer_count: active_peer_count.max(1),
            peer_scores: HashMap::new(),
            addr_to_pubkey: HashMap::new(),
            ip_to_pubkeys: HashMap::new(),
            last_request_sent_at: HashMap::new(),
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

    fn pick_peer(&mut self, slot: u64, index: u32) -> Option<RepairPeer> {
        self.refresh_peers(slot);
        let peers = self.peers_by_slot.get(&slot)?.peers.as_slice();
        if peers.is_empty() {
            return None;
        }
        self.rr_counter = self.rr_counter.wrapping_add(1);
        let seed = self
            .rr_counter
            .wrapping_add(slot)
            .wrapping_add(u64::from(index));
        let weights: Vec<u64> = peers
            .iter()
            .map(|peer| self.weight_for(peer.pubkey))
            .collect();
        let total_weight: u64 = weights.iter().copied().sum();
        if total_weight == 0 {
            let peer_len = u64::try_from(peers.len()).ok()?;
            let reduced = seed.checked_rem(peer_len)?;
            let idx = usize::try_from(reduced).ok()?;
            return peers.get(idx).copied();
        }
        let mut target = seed.checked_rem(total_weight)?;
        for (peer, weight) in peers.iter().zip(weights.iter().copied()) {
            if target < weight {
                return Some(*peer);
            }
            target = target.saturating_sub(weight);
        }
        peers.last().copied()
    }

    fn refresh_peers(&mut self, slot: u64) {
        self.decay_peer_scores();
        self.peers_by_slot
            .retain(|_, cached| cached.updated_at.elapsed() < self.peer_cache_ttl);
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
            };
            if seen.insert((peer.pubkey, peer.addr)) {
                peers.push(peer);
            }
        }
        if peers.is_empty() {
            let self_pubkey = self.cluster_info.id();
            let self_shred_version = self.cluster_info.my_shred_version();
            for (contact_info, _) in self.cluster_info.all_peers() {
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

    fn weight_for(&self, pubkey: Pubkey) -> u64 {
        self.peer_scores
            .get(&pubkey)
            .copied()
            .unwrap_or_default()
            .weight()
    }

    fn next_nonce(&mut self) -> u32 {
        let nonce = self.nonce_counter;
        self.nonce_counter = self.nonce_counter.wrapping_add(1).max(1);
        nonce
    }
}
