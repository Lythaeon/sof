use super::*;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub(in crate::app::runtime) struct DatasetAttemptKey {
    pub(in crate::app::runtime) slot: u64,
    pub(in crate::app::runtime) start_index: u32,
    pub(in crate::app::runtime) end_index: u32,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(in crate::app::runtime) enum DatasetAttemptStatus {
    Success,
    Failure,
}

#[derive(Debug)]
pub(in crate::app::runtime) struct RecentDatasetAttemptCache {
    entries: HashMap<DatasetAttemptKey, (Instant, DatasetAttemptStatus)>,
    order: VecDeque<(Instant, DatasetAttemptKey, DatasetAttemptStatus)>,
    capacity: usize,
    success_ttl: Duration,
    failure_ttl: Duration,
}

impl RecentDatasetAttemptCache {
    pub(in crate::app::runtime) fn new(
        capacity: usize,
        success_ttl: Duration,
        failure_ttl: Duration,
    ) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity.min(32_768)),
            order: VecDeque::with_capacity(capacity.min(32_768)),
            capacity: capacity.max(1),
            success_ttl,
            failure_ttl,
        }
    }

    pub(in crate::app::runtime) fn is_recent_duplicate(
        &mut self,
        key: DatasetAttemptKey,
        now: Instant,
    ) -> bool {
        self.evict(now);
        if let Some((seen_at, status)) = self.entries.get(&key).copied() {
            return now.saturating_duration_since(seen_at) < self.ttl_for(status);
        }
        false
    }

    pub(in crate::app::runtime) fn record(
        &mut self,
        key: DatasetAttemptKey,
        now: Instant,
        status: DatasetAttemptStatus,
    ) {
        self.entries.insert(key, (now, status));
        self.order.push_back((now, key, status));
        self.evict(now);
    }

    const fn ttl_for(&self, status: DatasetAttemptStatus) -> Duration {
        match status {
            DatasetAttemptStatus::Success => self.success_ttl,
            DatasetAttemptStatus::Failure => self.failure_ttl,
        }
    }

    fn evict(&mut self, now: Instant) {
        while let Some((seen_at, key, status)) = self.order.front().copied() {
            let expired = now.saturating_duration_since(seen_at) >= self.ttl_for(status);
            let over_capacity = self.entries.len() > self.capacity;
            if !expired && !over_capacity {
                break;
            }
            self.order.pop_front();
            if self.entries.get(&key).copied() == Some((seen_at, status)) {
                let _ = self.entries.remove(&key);
            }
        }
    }
}
