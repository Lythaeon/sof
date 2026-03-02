use std::sync::atomic::{AtomicU64, Ordering};

const UNSET_SLOT: u64 = u64::MAX;

#[derive(Debug, Clone, Copy, Default)]
pub struct CommitmentSlotSnapshot {
    pub confirmed_slot: Option<u64>,
    pub finalized_slot: Option<u64>,
}

#[derive(Debug, Default)]
pub struct CommitmentSlotTracker {
    confirmed_slot: AtomicU64,
    finalized_slot: AtomicU64,
}

impl CommitmentSlotTracker {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            confirmed_slot: AtomicU64::new(UNSET_SLOT),
            finalized_slot: AtomicU64::new(UNSET_SLOT),
        }
    }

    pub fn update(&self, confirmed_slot: Option<u64>, finalized_slot: Option<u64>) {
        self.confirmed_slot
            .store(confirmed_slot.unwrap_or(UNSET_SLOT), Ordering::Relaxed);
        self.finalized_slot
            .store(finalized_slot.unwrap_or(UNSET_SLOT), Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot(&self) -> CommitmentSlotSnapshot {
        let confirmed_slot = self.confirmed_slot.load(Ordering::Relaxed);
        let finalized_slot = self.finalized_slot.load(Ordering::Relaxed);
        CommitmentSlotSnapshot {
            confirmed_slot: (confirmed_slot != UNSET_SLOT).then_some(confirmed_slot),
            finalized_slot: (finalized_slot != UNSET_SLOT).then_some(finalized_slot),
        }
    }
}
