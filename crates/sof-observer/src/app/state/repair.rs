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
    #[cfg(any(feature = "gossip-bootstrap", test))]
    timeout: Duration,
}

impl OutstandingRepairRequests {
    pub fn new(timeout: Duration) -> Self {
        #[cfg(not(any(feature = "gossip-bootstrap", test)))]
        let _ = timeout;
        Self {
            entries: HashMap::new(),
            #[cfg(any(feature = "gossip-bootstrap", test))]
            timeout,
        }
    }

    #[cfg(feature = "gossip-bootstrap")]
    pub fn purge_expired(&mut self, now: Instant) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|_, sent_at| now.saturating_duration_since(*sent_at) < self.timeout);
        before.saturating_sub(self.entries.len())
    }

    #[cfg(any(feature = "gossip-bootstrap", test))]
    pub fn try_reserve(&mut self, request: &MissingShredRequest, now: Instant) -> bool {
        let key = OutstandingRepairKey::from_request(request);
        if let Some(sent_at) = self.entries.get_mut(&key) {
            if now.saturating_duration_since(*sent_at) < self.timeout {
                return false;
            }
            *sent_at = now;
            return true;
        }
        let _ = self.entries.insert(key, now);
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
