use super::*;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct ShredDedupeIdentity {
    slot: u64,
    index: u32,
    fec_set_index: u32,
    version: u16,
    variant: u8,
}

impl ShredDedupeIdentity {
    #[must_use]
    pub const fn new(slot: u64, index: u32, fec_set_index: u32, version: u16, variant: u8) -> Self {
        Self {
            slot,
            index,
            fec_set_index,
            version,
            variant,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ShredDedupeObservation {
    Accepted,
    Duplicate,
    Conflict,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ShredDedupeStage {
    Ingress,
    Canonical,
}

/// Snapshot of bounded semantic shred dedupe cache occupancy and eviction totals.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct ShredDedupeCacheMetrics {
    /// Current number of canonical semantic shred identities retained in the cache.
    pub entries: u64,
    /// Maximum retained semantic shred identities observed since startup.
    pub max_entries: u64,
    /// Current eviction-queue depth, including stale observation records awaiting pruning.
    pub queue_depth: u64,
    /// Maximum eviction-queue depth observed since startup.
    pub max_queue_depth: u64,
    /// Total semantic shred identities evicted because the cache hit capacity pressure.
    pub capacity_evictions_total: u64,
    /// Total semantic shred identities evicted because they aged out past the retention window.
    pub expired_evictions_total: u64,
}

#[derive(Debug, Clone, Copy)]
struct ShredDedupeEntry {
    signature: [u8; 64],
    seen_at: Instant,
    ingress_seen: bool,
    canonical_seen: bool,
}

#[derive(Debug)]
pub struct ShredDedupeCache {
    entries: HashMap<ShredDedupeIdentity, ShredDedupeEntry>,
    order: VecDeque<(Instant, ShredDedupeIdentity)>,
    ttl: Duration,
    capacity: usize,
    retained_slot_lag: u64,
    newest_slot: u64,
    max_entries: usize,
    max_queue_depth: usize,
    capacity_evictions_total: u64,
    expired_evictions_total: u64,
}

impl ShredDedupeCache {
    pub fn new(capacity: usize, ttl: Duration, retained_slot_lag: u64) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity.min(65_536)),
            order: VecDeque::with_capacity(capacity.min(65_536)),
            ttl,
            capacity,
            retained_slot_lag: retained_slot_lag.max(1),
            newest_slot: 0,
            max_entries: 0,
            max_queue_depth: 0,
            capacity_evictions_total: 0,
            expired_evictions_total: 0,
        }
    }

    #[must_use]
    pub fn metrics(&self) -> ShredDedupeCacheMetrics {
        ShredDedupeCacheMetrics {
            entries: u64::try_from(self.entries.len()).unwrap_or(u64::MAX),
            max_entries: u64::try_from(self.max_entries).unwrap_or(u64::MAX),
            queue_depth: u64::try_from(self.order.len()).unwrap_or(u64::MAX),
            max_queue_depth: u64::try_from(self.max_queue_depth).unwrap_or(u64::MAX),
            capacity_evictions_total: self.capacity_evictions_total,
            expired_evictions_total: self.expired_evictions_total,
        }
    }

    #[cfg(feature = "gossip-bootstrap")]
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.newest_slot = 0;
    }

    pub fn observe_shred(
        &mut self,
        packet: &[u8],
        parsed_shred: &ParsedShredHeader,
        now: Instant,
    ) -> ShredDedupeObservation {
        let Some((key, signature)) = make_shred_dedupe_key(packet, parsed_shred) else {
            return ShredDedupeObservation::Accepted;
        };
        self.observe_key(key, signature, now, ShredDedupeStage::Ingress)
    }

    pub fn observe_signature(
        &mut self,
        identity: ShredDedupeIdentity,
        signature: [u8; 64],
        now: Instant,
        stage: ShredDedupeStage,
    ) -> ShredDedupeObservation {
        self.observe_key(identity, signature, now, stage)
    }

    fn observe_key(
        &mut self,
        key: ShredDedupeIdentity,
        signature: [u8; 64],
        now: Instant,
        stage: ShredDedupeStage,
    ) -> ShredDedupeObservation {
        self.newest_slot = self.newest_slot.max(key.slot);
        self.evict(now);
        if let Some(existing) = self.entries.get_mut(&key) {
            let observation = if existing.signature != signature {
                ShredDedupeObservation::Conflict
            } else {
                match stage {
                    ShredDedupeStage::Ingress if existing.ingress_seen => {
                        ShredDedupeObservation::Duplicate
                    }
                    ShredDedupeStage::Canonical if existing.canonical_seen => {
                        ShredDedupeObservation::Duplicate
                    }
                    ShredDedupeStage::Ingress => {
                        existing.ingress_seen = true;
                        ShredDedupeObservation::Accepted
                    }
                    ShredDedupeStage::Canonical => {
                        existing.canonical_seen = true;
                        ShredDedupeObservation::Accepted
                    }
                }
            };
            if matches!(observation, ShredDedupeObservation::Accepted) {
                existing.seen_at = now;
                self.order.push_back((now, key));
                self.observe_depths();
            }
            return observation;
        }
        self.entries.insert(
            key,
            ShredDedupeEntry {
                signature,
                seen_at: now,
                ingress_seen: matches!(stage, ShredDedupeStage::Ingress),
                canonical_seen: matches!(stage, ShredDedupeStage::Canonical),
            },
        );
        self.order.push_back((now, key));
        self.evict(now);
        self.observe_depths();
        ShredDedupeObservation::Accepted
    }

    fn evict(&mut self, now: Instant) {
        let slot_floor = self.newest_slot.saturating_sub(self.retained_slot_lag);
        while let Some((seen_at, key)) = self.order.front().copied() {
            let expired =
                now.saturating_duration_since(seen_at) >= self.ttl && key.slot < slot_floor;
            let over_capacity = self.entries.len() > self.capacity;
            if !expired && !over_capacity {
                break;
            }
            self.order.pop_front();
            if self
                .entries
                .get(&key)
                .is_some_and(|entry| entry.seen_at == seen_at)
            {
                if expired {
                    self.expired_evictions_total = self.expired_evictions_total.saturating_add(1);
                } else if over_capacity {
                    self.capacity_evictions_total = self.capacity_evictions_total.saturating_add(1);
                }
                let _ = self.entries.remove(&key);
            }
        }
    }

    fn observe_depths(&mut self) {
        self.max_entries = self.max_entries.max(self.entries.len());
        self.max_queue_depth = self.max_queue_depth.max(self.order.len());
    }
}

fn make_shred_dedupe_key(
    packet: &[u8],
    parsed_shred: &ParsedShredHeader,
) -> Option<(ShredDedupeIdentity, [u8; 64])> {
    let signature_bytes: [u8; 64] = packet.get(0..64)?.try_into().ok()?;
    let variant = *packet.get(64)?;
    let common = match parsed_shred {
        ParsedShredHeader::Data(data) => data.common,
        ParsedShredHeader::Code(code) => code.common,
    };
    Some((
        ShredDedupeIdentity {
            slot: common.slot,
            index: common.index,
            fec_set_index: common.fec_set_index,
            version: common.version,
            variant,
        },
        signature_bytes,
    ))
}

#[cfg(test)]
mod tests {
    use sof_support::bench::avg_ns_per_iteration;

    use super::*;
    use crate::{
        protocol::shred_wire::SIZE_OF_DATA_SHRED_PAYLOAD, shred::wire::parse_shred_header,
    };

    fn test_key(slot: u64, index: u32) -> ShredDedupeIdentity {
        ShredDedupeIdentity {
            slot,
            index,
            fec_set_index: 0,
            version: 1,
            variant: 0xA5,
        }
    }

    #[test]
    fn semantic_duplicate_with_same_signature_is_dropped() {
        let now = Instant::now();
        let mut cache = ShredDedupeCache::new(16, Duration::from_millis(250), 64);
        let key = test_key(42, 7);

        assert_eq!(
            cache.observe_key(key, [1_u8; 64], now, ShredDedupeStage::Ingress),
            ShredDedupeObservation::Accepted
        );
        assert_eq!(
            cache.observe_key(
                key,
                [1_u8; 64],
                now + Duration::from_millis(1),
                ShredDedupeStage::Ingress,
            ),
            ShredDedupeObservation::Duplicate
        );
    }

    #[test]
    fn semantic_duplicate_with_different_signature_is_conflict() {
        let now = Instant::now();
        let mut cache = ShredDedupeCache::new(16, Duration::from_millis(250), 64);
        let key = test_key(42, 7);

        assert_eq!(
            cache.observe_key(key, [1_u8; 64], now, ShredDedupeStage::Ingress),
            ShredDedupeObservation::Accepted
        );
        assert_eq!(
            cache.observe_key(
                key,
                [2_u8; 64],
                now + Duration::from_millis(1),
                ShredDedupeStage::Ingress,
            ),
            ShredDedupeObservation::Conflict
        );
    }

    #[test]
    fn recent_slots_are_retained_past_ttl() {
        let now = Instant::now();
        let mut cache = ShredDedupeCache::new(16, Duration::from_millis(1), 64);

        assert_eq!(
            cache.observe_key(test_key(100, 1), [1_u8; 64], now, ShredDedupeStage::Ingress,),
            ShredDedupeObservation::Accepted
        );
        assert_eq!(
            cache.observe_key(
                test_key(101, 1),
                [2_u8; 64],
                now + Duration::from_millis(2),
                ShredDedupeStage::Ingress,
            ),
            ShredDedupeObservation::Accepted
        );
        assert_eq!(
            cache.observe_key(
                test_key(100, 1),
                [1_u8; 64],
                now + Duration::from_millis(3),
                ShredDedupeStage::Ingress,
            ),
            ShredDedupeObservation::Duplicate
        );
    }

    #[test]
    fn old_slots_evict_after_ttl_once_tip_moves_forward() {
        let now = Instant::now();
        let mut cache = ShredDedupeCache::new(16, Duration::from_millis(1), 1);

        assert_eq!(
            cache.observe_key(test_key(100, 1), [1_u8; 64], now, ShredDedupeStage::Ingress,),
            ShredDedupeObservation::Accepted
        );
        assert_eq!(
            cache.observe_key(
                test_key(102, 1),
                [2_u8; 64],
                now + Duration::from_millis(2),
                ShredDedupeStage::Ingress,
            ),
            ShredDedupeObservation::Accepted
        );
        assert_eq!(cache.metrics().entries, 1);
        assert_eq!(
            cache.observe_key(
                test_key(100, 1),
                [1_u8; 64],
                now + Duration::from_millis(3),
                ShredDedupeStage::Ingress,
            ),
            ShredDedupeObservation::Accepted
        );
    }

    #[test]
    fn capacity_evictions_are_tracked_separately_from_expiry() {
        let now = Instant::now();
        let mut cache = ShredDedupeCache::new(1, Duration::from_secs(60), 64);

        assert_eq!(
            cache.observe_key(test_key(10, 1), [1_u8; 64], now, ShredDedupeStage::Ingress,),
            ShredDedupeObservation::Accepted
        );
        assert_eq!(
            cache.observe_key(
                test_key(10, 2),
                [2_u8; 64],
                now + Duration::from_millis(1),
                ShredDedupeStage::Ingress,
            ),
            ShredDedupeObservation::Accepted
        );

        assert_eq!(
            cache.metrics(),
            ShredDedupeCacheMetrics {
                entries: 1,
                max_entries: 1,
                queue_depth: 1,
                max_queue_depth: 1,
                capacity_evictions_total: 1,
                expired_evictions_total: 0,
            }
        );
    }

    #[test]
    fn canonical_stage_accepts_first_emit_after_ingress_observation() {
        let now = Instant::now();
        let mut cache = ShredDedupeCache::new(16, Duration::from_secs(1), 64);
        let key = test_key(42, 7);

        assert_eq!(
            cache.observe_key(key, [1_u8; 64], now, ShredDedupeStage::Ingress),
            ShredDedupeObservation::Accepted
        );
        assert_eq!(
            cache.observe_key(
                key,
                [1_u8; 64],
                now + Duration::from_millis(1),
                ShredDedupeStage::Canonical,
            ),
            ShredDedupeObservation::Accepted
        );
        assert_eq!(
            cache.observe_key(
                key,
                [1_u8; 64],
                now + Duration::from_millis(2),
                ShredDedupeStage::Canonical,
            ),
            ShredDedupeObservation::Duplicate
        );
    }

    #[test]
    fn duplicate_observations_do_not_refresh_eviction_queue() {
        let now = Instant::now();
        let mut cache = ShredDedupeCache::new(16, Duration::from_secs(1), 64);
        let key = test_key(42, 7);

        assert_eq!(
            cache.observe_key(key, [1_u8; 64], now, ShredDedupeStage::Ingress),
            ShredDedupeObservation::Accepted
        );
        assert_eq!(cache.metrics().queue_depth, 1);

        assert_eq!(
            cache.observe_key(
                key,
                [1_u8; 64],
                now + Duration::from_millis(1),
                ShredDedupeStage::Ingress,
            ),
            ShredDedupeObservation::Duplicate
        );
        assert_eq!(cache.metrics().queue_depth, 1);

        assert_eq!(
            cache.observe_key(
                key,
                [2_u8; 64],
                now + Duration::from_millis(2),
                ShredDedupeStage::Ingress,
            ),
            ShredDedupeObservation::Conflict
        );
        assert_eq!(cache.metrics().queue_depth, 1);
    }

    #[test]
    fn canonical_transition_refreshes_queue_once() {
        let now = Instant::now();
        let mut cache = ShredDedupeCache::new(16, Duration::from_secs(1), 64);
        let key = test_key(42, 7);

        assert_eq!(
            cache.observe_key(key, [1_u8; 64], now, ShredDedupeStage::Ingress),
            ShredDedupeObservation::Accepted
        );
        assert_eq!(cache.metrics().queue_depth, 1);

        assert_eq!(
            cache.observe_key(
                key,
                [1_u8; 64],
                now + Duration::from_millis(1),
                ShredDedupeStage::Canonical,
            ),
            ShredDedupeObservation::Accepted
        );
        assert_eq!(cache.metrics().queue_depth, 2);

        assert_eq!(
            cache.observe_key(
                key,
                [1_u8; 64],
                now + Duration::from_millis(2),
                ShredDedupeStage::Canonical,
            ),
            ShredDedupeObservation::Duplicate
        );
        assert_eq!(cache.metrics().queue_depth, 2);
    }

    #[test]
    #[ignore = "profiling fixture for duplicate ingress dedupe churn"]
    fn duplicate_ingress_profile_fixture() {
        let now = Instant::now();
        let iterations = std::env::var("SOF_DEDUPE_PROFILE_ITERS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(2_000_000);
        let unique_keys = std::env::var("SOF_DEDUPE_PROFILE_KEYS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(1_024);
        let mut cache = ShredDedupeCache::new(
            unique_keys.saturating_mul(4),
            Duration::from_secs(60),
            4_096,
        );

        for key_index in 0..unique_keys {
            let key = test_key(10_000, u32::try_from(key_index).unwrap_or(u32::MAX));
            let signature = [u8::try_from(key_index % 251).unwrap_or(0); 64];
            assert_eq!(
                cache.observe_key(key, signature, now, ShredDedupeStage::Ingress),
                ShredDedupeObservation::Accepted
            );
        }

        let start = Instant::now();
        for duplicate_index in 0..iterations {
            let key_index = duplicate_index % unique_keys;
            let key = test_key(10_000, u32::try_from(key_index).unwrap_or(u32::MAX));
            let signature = [u8::try_from(key_index % 251).unwrap_or(0); 64];
            let observed_at =
                now + Duration::from_micros(u64::try_from(duplicate_index).unwrap_or(u64::MAX));
            assert_eq!(
                cache.observe_key(key, signature, observed_at, ShredDedupeStage::Ingress),
                ShredDedupeObservation::Duplicate
            );
        }
        let elapsed = start.elapsed();
        let avg_ns = avg_ns_per_iteration(elapsed, iterations);
        let metrics = cache.metrics();
        println!(
            "duplicate_ingress_profile_fixture iterations={} unique_keys={} elapsed_us={} avg_ns_per_iteration={} avg_us_per_iteration={:.3} queue_depth={} entries={}",
            iterations,
            unique_keys,
            elapsed.as_micros(),
            avg_ns,
            avg_ns as f64 / 1_000.0,
            metrics.queue_depth,
            metrics.entries,
        );
    }

    #[test]
    #[ignore = "profiling fixture for duplicate parse plus ingress dedupe churn"]
    fn duplicate_parse_and_ingress_profile_fixture() {
        let now = Instant::now();
        let iterations = std::env::var("SOF_DEDUPE_PROFILE_ITERS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(2_000_000);
        let packet = build_data_shred_packet(10_000, 7, 0, b"dup");
        let parsed = parse_shred_header(&packet).expect("valid test shred");
        let mut cache = ShredDedupeCache::new(4, Duration::from_secs(60), 4_096);

        assert_eq!(
            cache.observe_shred(&packet, &parsed, now),
            ShredDedupeObservation::Accepted
        );

        let start = Instant::now();
        for duplicate_index in 0..iterations {
            let observed_at =
                now + Duration::from_micros(u64::try_from(duplicate_index).unwrap_or(u64::MAX));
            let parsed_duplicate = parse_shred_header(&packet).expect("valid test shred");
            assert_eq!(
                cache.observe_shred(&packet, &parsed_duplicate, observed_at),
                ShredDedupeObservation::Duplicate
            );
        }
        let elapsed = start.elapsed();
        let avg_ns = avg_ns_per_iteration(elapsed, iterations);
        let metrics = cache.metrics();
        println!(
            "duplicate_parse_and_ingress_profile_fixture iterations={} elapsed_us={} avg_ns_per_iteration={} avg_us_per_iteration={:.3} queue_depth={} entries={}",
            iterations,
            elapsed.as_micros(),
            avg_ns,
            avg_ns as f64 / 1_000.0,
            metrics.queue_depth,
            metrics.entries,
        );
    }

    fn build_data_shred_packet(
        slot: u64,
        index: u32,
        fec_set_index: u32,
        payload: &[u8],
    ) -> Vec<u8> {
        let total = 88usize.saturating_add(payload.len());
        let mut packet = vec![0_u8; SIZE_OF_DATA_SHRED_PAYLOAD];
        packet[64] = 0x90;
        packet[65..73].copy_from_slice(&slot.to_le_bytes());
        packet[73..77].copy_from_slice(&index.to_le_bytes());
        packet[77..79].copy_from_slice(&u16::to_le_bytes(1));
        packet[79..83].copy_from_slice(&fec_set_index.to_le_bytes());
        packet[83..85].copy_from_slice(&u16::to_le_bytes(1));
        packet[85] = 0b0100_0000;
        let size = u16::try_from(total).expect("test packet too large");
        packet[86..88].copy_from_slice(&size.to_le_bytes());
        let payload_end = 88usize.saturating_add(payload.len());
        packet[88..payload_end].copy_from_slice(payload);
        packet
    }
}
