use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use crossbeam_skiplist::SkipMap;

use crate::shred::wire::ParsedShredHeader;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct RelayRangeRequest {
    pub slot: u64,
    pub start_index: u32,
    pub end_index: u32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct RelayRangeLimits {
    pub max_request_span: u32,
    pub max_response_shreds: usize,
    pub max_response_bytes: usize,
}

#[derive(Debug, thiserror::Error, Clone, Copy, Eq, PartialEq)]
pub enum RelayRangeQueryError {
    #[error("invalid relay range request; start_index={start_index} end_index={end_index}")]
    InvalidRange { start_index: u32, end_index: u32 },
    #[error("relay range span {span} exceeds configured max_request_span {max_request_span}")]
    SpanTooLarge { span: u32, max_request_span: u32 },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Ord, PartialOrd)]
struct CachedShredKey {
    signature: [u8; 64],
    slot: u64,
    index: u32,
    fec_set_index: u32,
    version: u16,
    variant: u8,
}

#[derive(Debug, Clone)]
struct CachedShred {
    seen_at: Instant,
    bytes: Arc<[u8]>,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct CacheInsertOutcome {
    pub inserted: bool,
    pub replaced: bool,
    pub evicted: usize,
}

#[derive(Debug)]
pub struct RecentShredRingBuffer {
    entries: HashMap<CachedShredKey, CachedShred>,
    order: VecDeque<(Instant, CachedShredKey)>,
    ttl: Duration,
    capacity: usize,
}

impl RecentShredRingBuffer {
    #[must_use]
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity.min(65_536)),
            order: VecDeque::with_capacity(capacity.min(65_536)),
            ttl,
            capacity,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn insert(
        &mut self,
        packet: &[u8],
        parsed_shred: &ParsedShredHeader,
        now: Instant,
    ) -> CacheInsertOutcome {
        let mut evicted = 0usize;
        self.evict(now, &mut evicted);

        let Some(key) = make_cached_shred_key(packet, parsed_shred) else {
            return CacheInsertOutcome {
                inserted: false,
                replaced: false,
                evicted,
            };
        };

        let replaced = self.entries.insert(
            key,
            CachedShred {
                seen_at: now,
                bytes: Arc::from(packet),
            },
        );
        self.order.push_back((now, key));
        self.evict(now, &mut evicted);

        CacheInsertOutcome {
            inserted: replaced.is_none(),
            replaced: replaced.is_some(),
            evicted,
        }
    }

    pub fn query_range(
        &mut self,
        request: RelayRangeRequest,
        limits: RelayRangeLimits,
        now: Instant,
    ) -> Result<Vec<Vec<u8>>, RelayRangeQueryError> {
        let mut evicted = 0usize;
        self.evict(now, &mut evicted);

        if request.start_index > request.end_index {
            return Err(RelayRangeQueryError::InvalidRange {
                start_index: request.start_index,
                end_index: request.end_index,
            });
        }

        let span = request
            .end_index
            .saturating_sub(request.start_index)
            .saturating_add(1);
        if span > limits.max_request_span {
            return Err(RelayRangeQueryError::SpanTooLarge {
                span,
                max_request_span: limits.max_request_span,
            });
        }

        let mut matches: Vec<(u32, Arc<[u8]>)> = self
            .entries
            .iter()
            .filter(|(key, _)| {
                key.slot == request.slot
                    && key.index >= request.start_index
                    && key.index <= request.end_index
            })
            .map(|(key, entry)| (key.index, entry.bytes.clone()))
            .collect();
        matches.sort_unstable_by_key(|(index, _)| *index);

        let mut response = Vec::new();
        let mut response_bytes = 0usize;
        for (_, bytes) in matches {
            if response.len() >= limits.max_response_shreds {
                break;
            }
            let next_bytes = response_bytes.saturating_add(bytes.len());
            if next_bytes > limits.max_response_bytes {
                break;
            }
            response_bytes = next_bytes;
            response.push(bytes.as_ref().to_vec());
        }

        Ok(response)
    }

    #[must_use]
    pub fn query_exact(&mut self, slot: u64, index: u32, now: Instant) -> Option<Vec<u8>> {
        let mut evicted = 0usize;
        self.evict(now, &mut evicted);
        self.entries.iter().find_map(|(key, entry)| {
            (key.slot == slot && key.index == index).then(|| entry.bytes.as_ref().to_vec())
        })
    }

    #[must_use]
    pub fn query_highest_above(
        &mut self,
        slot: u64,
        min_index_exclusive: u32,
        now: Instant,
    ) -> Option<(u32, Vec<u8>)> {
        let mut evicted = 0usize;
        self.evict(now, &mut evicted);
        self.entries
            .iter()
            .filter(|(key, _)| key.slot == slot && key.index > min_index_exclusive)
            .max_by_key(|(key, entry)| (key.index, entry.seen_at))
            .map(|(key, entry)| (key.index, entry.bytes.as_ref().to_vec()))
    }

    fn evict(&mut self, now: Instant, evicted: &mut usize) {
        while let Some((seen_at, key)) = self.order.front().copied() {
            let expired = now.saturating_duration_since(seen_at) >= self.ttl;
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
                if let Some(removed) = self.entries.remove(&key) {
                    // Drop packet bytes as soon as entry is evicted from active set.
                    drop(removed.bytes);
                }
                *evicted = evicted.saturating_add(1);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct SharedRelayCache {
    entries: Arc<SkipMap<CachedShredKey, CachedShred>>,
    order: Arc<SkipMap<(Instant, CachedShredKey), ()>>,
    slot_index: Arc<SkipMap<(u64, u32, CachedShredKey), ()>>,
    ttl: Duration,
    capacity: usize,
    len: Arc<AtomicUsize>,
}

impl SharedRelayCache {
    #[must_use]
    pub fn new(cache: RecentShredRingBuffer) -> Self {
        let shared = Self {
            entries: Arc::new(SkipMap::new()),
            order: Arc::new(SkipMap::new()),
            slot_index: Arc::new(SkipMap::new()),
            ttl: cache.ttl,
            capacity: cache.capacity,
            len: Arc::new(AtomicUsize::new(0)),
        };
        for (key, entry) in cache.entries {
            let seen_at = entry.seen_at;
            shared.entries.insert(key, entry);
            shared.order.insert((seen_at, key), ());
            shared.slot_index.insert((key.slot, key.index, key), ());
            let _ = shared.len.fetch_add(1, Ordering::Relaxed);
        }
        shared
    }

    #[must_use]
    pub fn insert(
        &self,
        packet: &[u8],
        parsed_shred: &ParsedShredHeader,
        now: Instant,
    ) -> CacheInsertOutcome {
        let mut evicted = self.evict(now);
        let Some(key) = make_cached_shred_key(packet, parsed_shred) else {
            return CacheInsertOutcome {
                inserted: false,
                replaced: false,
                evicted,
            };
        };

        let mut replaced = false;
        if let Some(previous) = self.entries.remove(&key) {
            replaced = true;
            let previous_seen_at = previous.value().seen_at;
            let _ = self.order.remove(&(previous_seen_at, key));
            let _ = self.slot_index.remove(&(key.slot, key.index, key));
        } else {
            let _ = self.len.fetch_add(1, Ordering::Relaxed);
        }
        self.entries.insert(
            key,
            CachedShred {
                seen_at: now,
                bytes: Arc::from(packet),
            },
        );
        self.order.insert((now, key), ());
        self.slot_index.insert((key.slot, key.index, key), ());
        evicted = evicted.saturating_add(self.evict(now));

        CacheInsertOutcome {
            inserted: !replaced,
            replaced,
            evicted,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn query_range(
        &self,
        request: RelayRangeRequest,
        limits: RelayRangeLimits,
        now: Instant,
    ) -> Result<Vec<Vec<u8>>, RelayRangeQueryError> {
        self.query_range_shared(request, limits, now)
            .map(|response| {
                response
                    .into_iter()
                    .map(|bytes| bytes.as_ref().to_vec())
                    .collect()
            })
    }

    pub fn query_range_shared(
        &self,
        request: RelayRangeRequest,
        limits: RelayRangeLimits,
        now: Instant,
    ) -> Result<Vec<Arc<[u8]>>, RelayRangeQueryError> {
        let _ = self.evict(now);
        if request.start_index > request.end_index {
            return Err(RelayRangeQueryError::InvalidRange {
                start_index: request.start_index,
                end_index: request.end_index,
            });
        }
        let span = request
            .end_index
            .saturating_sub(request.start_index)
            .saturating_add(1);
        if span > limits.max_request_span {
            return Err(RelayRangeQueryError::SpanTooLarge {
                span,
                max_request_span: limits.max_request_span,
            });
        }

        let mut response: Vec<Arc<[u8]>> = Vec::new();
        let mut response_bytes = 0usize;
        let start = slot_index_range_start(request.slot, request.start_index);
        let end = slot_index_range_end(request.slot, request.end_index);
        for entry in self.slot_index.range(start..=end) {
            let key = entry.key().2;
            let Some(cached) = self.entries.get(&key) else {
                continue;
            };
            let bytes = cached.value().bytes.clone();
            if response.len() >= limits.max_response_shreds {
                break;
            }
            let next_bytes = response_bytes.saturating_add(bytes.len());
            if next_bytes > limits.max_response_bytes {
                break;
            }
            response_bytes = next_bytes;
            response.push(bytes);
        }
        Ok(response)
    }

    #[must_use]
    pub fn query_exact(&self, slot: u64, index: u32, now: Instant) -> Option<Vec<u8>> {
        self.query_exact_shared(slot, index, now)
            .map(|bytes| bytes.as_ref().to_vec())
    }

    #[must_use]
    pub fn query_exact_shared(&self, slot: u64, index: u32, now: Instant) -> Option<Arc<[u8]>> {
        let _ = self.evict(now);
        let start = slot_index_range_start(slot, index);
        let end = slot_index_range_end(slot, index);
        let mut latest_seen_at: Option<Instant> = None;
        let mut latest_bytes: Option<Arc<[u8]>> = None;
        for entry in self.slot_index.range(start..=end) {
            let key = entry.key().2;
            let Some(cached) = self.entries.get(&key) else {
                continue;
            };
            let seen_at = cached.value().seen_at;
            if latest_seen_at.is_none_or(|current| seen_at > current) {
                latest_seen_at = Some(seen_at);
                latest_bytes = Some(cached.value().bytes.clone());
            }
        }
        latest_bytes
    }

    #[must_use]
    pub fn query_highest_above(
        &self,
        slot: u64,
        min_index_exclusive: u32,
        now: Instant,
    ) -> Option<(u32, Vec<u8>)> {
        self.query_highest_above_shared(slot, min_index_exclusive, now)
            .map(|(index, bytes)| (index, bytes.as_ref().to_vec()))
    }

    #[must_use]
    pub fn query_highest_above_shared(
        &self,
        slot: u64,
        min_index_exclusive: u32,
        now: Instant,
    ) -> Option<(u32, Arc<[u8]>)> {
        let _ = self.evict(now);
        let start_index = min_index_exclusive.saturating_add(1);
        let start = slot_index_range_start(slot, start_index);
        let end = slot_index_range_end(slot, u32::MAX);
        let mut best_index: Option<u32> = None;
        let mut best_seen_at: Option<Instant> = None;
        let mut best_bytes: Option<Arc<[u8]>> = None;
        for entry in self.slot_index.range(start..=end).rev() {
            let (_, index, key) = *entry.key();
            if let Some(current_best_index) = best_index
                && index != current_best_index
            {
                break;
            }
            let Some(cached) = self.entries.get(&key) else {
                continue;
            };
            best_index = Some(index);
            let seen_at = cached.value().seen_at;
            if best_seen_at.is_none_or(|current| seen_at > current) {
                best_seen_at = Some(seen_at);
                best_bytes = Some(cached.value().bytes.clone());
            }
        }
        best_index.zip(best_bytes)
    }

    fn evict(&self, now: Instant) -> usize {
        let mut evicted = 0usize;
        loop {
            let Some(front) = self.order.front() else {
                break;
            };
            let (seen_at, key) = *front.key();
            drop(front);

            let expired = now.saturating_duration_since(seen_at) >= self.ttl;
            let over_capacity = self.len.load(Ordering::Relaxed) > self.capacity;
            if !expired && !over_capacity {
                break;
            }
            if self.order.remove(&(seen_at, key)).is_none() {
                continue;
            }
            let remove_entry = self
                .entries
                .get(&key)
                .is_some_and(|entry| entry.value().seen_at == seen_at);
            if remove_entry && self.entries.remove(&key).is_some() {
                let _ = self.slot_index.remove(&(key.slot, key.index, key));
                let _ = self.len.fetch_sub(1, Ordering::Relaxed);
                evicted = evicted.saturating_add(1);
            }
        }
        evicted
    }
}

fn make_cached_shred_key(
    packet: &[u8],
    parsed_shred: &ParsedShredHeader,
) -> Option<CachedShredKey> {
    let signature: [u8; 64] = packet.get(0..64)?.try_into().ok()?;
    let variant = *packet.get(64)?;
    let common = match parsed_shred {
        ParsedShredHeader::Data(data) => data.common,
        ParsedShredHeader::Code(code) => code.common,
    };
    Some(CachedShredKey {
        signature,
        slot: common.slot,
        index: common.index,
        fec_set_index: common.fec_set_index,
        version: common.version,
        variant,
    })
}

const fn slot_index_range_start(slot: u64, index: u32) -> (u64, u32, CachedShredKey) {
    (slot, index, min_cached_shred_key())
}

const fn slot_index_range_end(slot: u64, index: u32) -> (u64, u32, CachedShredKey) {
    (slot, index, max_cached_shred_key())
}

const fn min_cached_shred_key() -> CachedShredKey {
    CachedShredKey {
        signature: [0_u8; 64],
        slot: 0,
        index: 0,
        fec_set_index: 0,
        version: 0,
        variant: 0,
    }
}

const fn max_cached_shred_key() -> CachedShredKey {
    CachedShredKey {
        signature: [u8::MAX; 64],
        slot: u64::MAX,
        index: u32::MAX,
        fec_set_index: u32::MAX,
        version: u16::MAX,
        variant: u8::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        protocol::shred_wire::{
            SIZE_OF_DATA_SHRED_HEADERS, SIZE_OF_DATA_SHRED_PAYLOAD, VARIANT_MERKLE_DATA,
        },
        shred::wire::parse_shred_header,
    };

    #[test]
    fn cache_insert_replaces_existing_key_without_growing_len() {
        let packet = build_data_shred_packet(42, 7, 1, 9, b"hello");
        let parsed = parse_shred_header(&packet).expect("valid test shred");
        let mut cache = RecentShredRingBuffer::new(16, Duration::from_secs(2));

        let first = cache.insert(&packet, &parsed, Instant::now());
        assert!(first.inserted);
        assert!(!first.replaced);
        assert_eq!(cache.len(), 1);

        let second = cache.insert(&packet, &parsed, Instant::now());
        assert!(!second.inserted);
        assert!(second.replaced);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_evicts_by_capacity() {
        let p1 = build_data_shred_packet(100, 1, 1, 0, b"a");
        let p2 = build_data_shred_packet(100, 2, 2, 0, b"b");
        let p3 = build_data_shred_packet(100, 3, 3, 0, b"c");
        let h1 = parse_shred_header(&p1).expect("valid");
        let h2 = parse_shred_header(&p2).expect("valid");
        let h3 = parse_shred_header(&p3).expect("valid");
        let mut cache = RecentShredRingBuffer::new(2, Duration::from_secs(10));
        let t0 = Instant::now();

        let _ = cache.insert(&p1, &h1, t0);
        let _ = cache.insert(&p2, &h2, t0 + Duration::from_millis(1));
        let outcome = cache.insert(&p3, &h3, t0 + Duration::from_millis(2));

        assert_eq!(cache.len(), 2);
        assert_eq!(outcome.evicted, 1);
    }

    #[test]
    fn cache_evicts_by_ttl() {
        let packet = build_data_shred_packet(200, 9, 4, 0, b"payload");
        let header = parse_shred_header(&packet).expect("valid");
        let mut cache = RecentShredRingBuffer::new(8, Duration::from_millis(5));
        let base = Instant::now();

        let _ = cache.insert(&packet, &header, base);
        assert_eq!(cache.len(), 1);

        let newer_packet = build_data_shred_packet(200, 10, 5, 0, b"newer");
        let newer_header = parse_shred_header(&newer_packet).expect("valid");
        let outcome = cache.insert(
            &newer_packet,
            &newer_header,
            base + Duration::from_millis(6),
        );

        assert_eq!(outcome.evicted, 1);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn query_range_returns_sorted_indices_with_limits() {
        let p1 = build_data_shred_packet(9, 4, 1, 0, b"a");
        let p2 = build_data_shred_packet(9, 6, 2, 0, b"bbb");
        let p3 = build_data_shred_packet(9, 5, 3, 0, b"cc");
        let h1 = parse_shred_header(&p1).expect("valid");
        let h2 = parse_shred_header(&p2).expect("valid");
        let h3 = parse_shred_header(&p3).expect("valid");
        let mut cache = RecentShredRingBuffer::new(16, Duration::from_secs(2));
        let now = Instant::now();
        let _ = cache.insert(&p1, &h1, now);
        let _ = cache.insert(&p2, &h2, now);
        let _ = cache.insert(&p3, &h3, now);

        let request = RelayRangeRequest {
            slot: 9,
            start_index: 4,
            end_index: 6,
        };
        let limits = RelayRangeLimits {
            max_request_span: 32,
            max_response_shreds: 2,
            max_response_bytes: p1.len().saturating_add(p3.len()),
        };
        let result = cache
            .query_range(request, limits, now)
            .expect("query succeeds");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], p1);
        assert_eq!(result[1], p3);
    }

    #[test]
    fn query_range_respects_max_response_bytes_budget() {
        let p1 = build_data_shred_packet(9, 4, 1, 0, b"a");
        let p2 = build_data_shred_packet(9, 5, 2, 0, b"bbb");
        let p3 = build_data_shred_packet(9, 6, 3, 0, b"cc");
        let h1 = parse_shred_header(&p1).expect("valid");
        let h2 = parse_shred_header(&p2).expect("valid");
        let h3 = parse_shred_header(&p3).expect("valid");
        let mut cache = RecentShredRingBuffer::new(16, Duration::from_secs(2));
        let now = Instant::now();
        let _ = cache.insert(&p1, &h1, now);
        let _ = cache.insert(&p2, &h2, now);
        let _ = cache.insert(&p3, &h3, now);

        let request = RelayRangeRequest {
            slot: 9,
            start_index: 4,
            end_index: 6,
        };
        let limits = RelayRangeLimits {
            max_request_span: 32,
            max_response_shreds: 8,
            max_response_bytes: p1.len(),
        };
        let result = cache
            .query_range(request, limits, now)
            .expect("query succeeds");
        assert_eq!(result, vec![p1]);
    }

    #[test]
    fn query_exact_returns_matching_shred() {
        let p1 = build_data_shred_packet(10, 1, 1, 0, b"a");
        let p2 = build_data_shred_packet(10, 2, 1, 0, b"b");
        let h1 = parse_shred_header(&p1).expect("valid");
        let h2 = parse_shred_header(&p2).expect("valid");
        let mut cache = RecentShredRingBuffer::new(16, Duration::from_secs(2));
        let now = Instant::now();
        let _ = cache.insert(&p1, &h1, now);
        let _ = cache.insert(&p2, &h2, now);

        let found = cache.query_exact(10, 2, now).expect("exact match");
        assert_eq!(found, p2);
    }

    #[test]
    fn query_highest_above_returns_greatest_index() {
        let p1 = build_data_shred_packet(11, 5, 1, 0, b"a");
        let p2 = build_data_shred_packet(11, 9, 1, 0, b"b");
        let p3 = build_data_shred_packet(11, 7, 1, 0, b"c");
        let h1 = parse_shred_header(&p1).expect("valid");
        let h2 = parse_shred_header(&p2).expect("valid");
        let h3 = parse_shred_header(&p3).expect("valid");
        let mut cache = RecentShredRingBuffer::new(16, Duration::from_secs(2));
        let now = Instant::now();
        let _ = cache.insert(&p1, &h1, now);
        let _ = cache.insert(&p2, &h2, now + Duration::from_millis(1));
        let _ = cache.insert(&p3, &h3, now + Duration::from_millis(2));

        let (index, found) = cache
            .query_highest_above(11, 6, now + Duration::from_millis(3))
            .expect("highest above threshold");
        assert_eq!(index, 9);
        assert_eq!(found, p2);
    }

    #[test]
    fn query_range_rejects_span_above_limit() {
        let mut cache = RecentShredRingBuffer::new(4, Duration::from_secs(2));
        let request = RelayRangeRequest {
            slot: 1,
            start_index: 1,
            end_index: 4,
        };
        let limits = RelayRangeLimits {
            max_request_span: 2,
            max_response_shreds: 64,
            max_response_bytes: 1_000_000,
        };
        let error = cache
            .query_range(request, limits, Instant::now())
            .expect_err("span should fail");
        assert!(matches!(error, RelayRangeQueryError::SpanTooLarge { .. }));
    }

    #[test]
    fn shared_query_exact_prefers_latest_seen_at_for_same_slot_and_index() {
        let packet_old = build_data_shred_packet(12, 42, 1, 0, b"old");
        let packet_new = build_data_shred_packet(12, 42, 2, 0, b"new");
        let header_old = parse_shred_header(&packet_old).expect("valid");
        let header_new = parse_shred_header(&packet_new).expect("valid");
        let cache = SharedRelayCache::new(RecentShredRingBuffer::new(16, Duration::from_secs(2)));
        let now = Instant::now();
        assert!(cache.insert(&packet_old, &header_old, now).inserted);
        assert!(
            cache
                .insert(&packet_new, &header_new, now + Duration::from_millis(1))
                .inserted
        );

        let found = cache
            .query_exact(12, 42, now + Duration::from_millis(2))
            .expect("exact match");
        assert_eq!(found, packet_new);
    }

    #[test]
    fn shared_query_highest_above_prefers_highest_index_then_latest_seen_at() {
        let packet_mid = build_data_shred_packet(13, 7, 1, 0, b"mid");
        let packet_old_top = build_data_shred_packet(13, 9, 1, 0, b"old-top");
        let packet_new_top = build_data_shred_packet(13, 9, 2, 0, b"new-top");
        let header_mid = parse_shred_header(&packet_mid).expect("valid");
        let header_old_top = parse_shred_header(&packet_old_top).expect("valid");
        let header_new_top = parse_shred_header(&packet_new_top).expect("valid");
        let cache = SharedRelayCache::new(RecentShredRingBuffer::new(16, Duration::from_secs(2)));
        let now = Instant::now();
        assert!(cache.insert(&packet_mid, &header_mid, now).inserted);
        assert!(
            cache
                .insert(
                    &packet_old_top,
                    &header_old_top,
                    now + Duration::from_millis(1),
                )
                .inserted
        );
        assert!(
            cache
                .insert(
                    &packet_new_top,
                    &header_new_top,
                    now + Duration::from_millis(2),
                )
                .inserted
        );

        let (index, found) = cache
            .query_highest_above(13, 6, now + Duration::from_millis(3))
            .expect("highest above threshold");
        assert_eq!(index, 9);
        assert_eq!(found, packet_new_top);
    }

    fn build_data_shred_packet(
        slot: u64,
        index: u32,
        fec_set_index: u32,
        parent_offset: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let total = SIZE_OF_DATA_SHRED_HEADERS.saturating_add(payload.len());
        let size = u16::try_from(total).expect("test packet too large");
        let mut packet = vec![0_u8; SIZE_OF_DATA_SHRED_PAYLOAD];

        // Distinguish signatures across test packets without constructing real signatures.
        packet[0..8].copy_from_slice(&slot.to_le_bytes());
        packet[8..12].copy_from_slice(&index.to_le_bytes());
        packet[12..16].copy_from_slice(&fec_set_index.to_le_bytes());
        packet[64] = VARIANT_MERKLE_DATA;
        packet[65..73].copy_from_slice(&slot.to_le_bytes());
        packet[73..77].copy_from_slice(&index.to_le_bytes());
        packet[77..79].copy_from_slice(&1_u16.to_le_bytes());
        packet[79..83].copy_from_slice(&fec_set_index.to_le_bytes());
        packet[83..85].copy_from_slice(&parent_offset.to_le_bytes());
        packet[85] = 0b0100_0000;
        packet[86..88].copy_from_slice(&size.to_le_bytes());
        let end = 88usize.saturating_add(payload.len());
        packet[88..end].copy_from_slice(payload);
        packet
    }
}
