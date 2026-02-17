use super::*;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
struct ShredDedupeKey {
    signature: [u8; 64],
    slot: u64,
    index: u32,
    fec_set_index: u32,
    version: u16,
    variant: u8,
}

#[derive(Debug)]
pub struct RecentShredCache {
    entries: HashMap<ShredDedupeKey, Instant>,
    order: VecDeque<(Instant, ShredDedupeKey)>,
    ttl: Duration,
    capacity: usize,
}

impl RecentShredCache {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity.min(65_536)),
            order: VecDeque::with_capacity(capacity.min(65_536)),
            ttl,
            capacity,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(feature = "gossip-bootstrap")]
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    pub fn is_recent_duplicate(
        &mut self,
        packet: &[u8],
        parsed_shred: &ParsedShredHeader,
        now: Instant,
    ) -> bool {
        self.evict(now);
        let Some(key) = make_shred_dedupe_key(packet, parsed_shred) else {
            return false;
        };
        if let Some(last_seen) = self.entries.get(&key)
            && now.saturating_duration_since(*last_seen) < self.ttl
        {
            return true;
        }
        self.entries.insert(key, now);
        self.order.push_back((now, key));
        self.evict(now);
        false
    }

    fn evict(&mut self, now: Instant) {
        while let Some((seen_at, key)) = self.order.front().copied() {
            let expired = now.saturating_duration_since(seen_at) >= self.ttl;
            let over_capacity = self.entries.len() > self.capacity;
            if !expired && !over_capacity {
                break;
            }
            self.order.pop_front();
            if self.entries.get(&key).copied() == Some(seen_at) {
                let _ = self.entries.remove(&key);
            }
        }
    }
}

fn make_shred_dedupe_key(
    packet: &[u8],
    parsed_shred: &ParsedShredHeader,
) -> Option<ShredDedupeKey> {
    let signature_bytes: [u8; 64] = packet.get(0..64)?.try_into().ok()?;
    let variant = *packet.get(64)?;
    let common = match parsed_shred {
        ParsedShredHeader::Data(data) => data.common,
        ParsedShredHeader::Code(code) => code.common,
    };
    Some(ShredDedupeKey {
        signature: signature_bytes,
        slot: common.slot,
        index: common.index,
        fec_set_index: common.fec_set_index,
        version: common.version,
        variant,
    })
}
