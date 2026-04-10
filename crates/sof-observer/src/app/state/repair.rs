use super::*;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
struct OutstandingRepairKey {
    slot: u64,
    index: u32,
    kind: MissingShredRequestKind,
}

impl OutstandingRepairKey {
    #[cfg(any(feature = "gossip-bootstrap", test))]
    const fn from_request(request: &MissingShredRequest) -> Self {
        Self {
            slot: request.slot,
            index: request.index,
            kind: request.kind,
        }
    }
}

#[derive(Debug)]
pub struct OutstandingRepairRequests {
    entries: HashMap<OutstandingRepairKey, Instant>,
    /// Insertion order for expiry, including stale superseded timestamps.
    order: VecDeque<(OutstandingRepairKey, Instant)>,
    #[cfg(any(feature = "gossip-bootstrap", test))]
    timeout: Duration,
}

impl OutstandingRepairRequests {
    pub fn new(timeout: Duration) -> Self {
        #[cfg(not(any(feature = "gossip-bootstrap", test)))]
        let _ = timeout;
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            #[cfg(any(feature = "gossip-bootstrap", test))]
            timeout,
        }
    }

    #[cfg(feature = "gossip-bootstrap")]
    pub fn purge_expired(&mut self, now: Instant) -> usize {
        let mut removed = 0_usize;
        while let Some((_, front_sent_at)) = self.order.front() {
            if now.saturating_duration_since(*front_sent_at) < self.timeout {
                break;
            }
            let Some((key, queued_sent_at)) = self.order.pop_front() else {
                break;
            };
            if self.entries.get(&key) == Some(&queued_sent_at) {
                let _ = self.entries.remove(&key);
                removed = removed.saturating_add(1);
            }
        }
        removed
    }

    #[cfg(any(feature = "gossip-bootstrap", test))]
    pub fn try_reserve(&mut self, request: &MissingShredRequest, now: Instant) -> bool {
        let key = OutstandingRepairKey::from_request(request);
        if let Some(sent_at) = self.entries.get_mut(&key) {
            if now.saturating_duration_since(*sent_at) < self.timeout {
                return false;
            }
            *sent_at = now;
            self.order.push_back((key, now));
            return true;
        }
        let _ = self.entries.insert(key, now);
        self.order.push_back((key, now));
        true
    }

    #[cfg(feature = "gossip-bootstrap")]
    pub fn release(&mut self, request: &MissingShredRequest) {
        let _ = self
            .entries
            .remove(&OutstandingRepairKey::from_request(request));
    }

    pub fn on_shred_received(&mut self, slot: u64, index: u32) -> usize {
        let mut removed = 0_usize;
        if self
            .entries
            .remove(&OutstandingRepairKey {
                slot,
                index,
                kind: MissingShredRequestKind::WindowIndex,
            })
            .is_some()
        {
            removed = removed.saturating_add(1);
        }
        let before = self.entries.len();
        self.entries.retain(|key, _| {
            !(key.kind == MissingShredRequestKind::HighestWindowIndex
                && key.slot == slot
                && key.index <= index)
        });
        removed.saturating_add(before.saturating_sub(self.entries.len()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
