use solana_signature::Signature;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
/// Transaction classification used by runtime and plugins.
pub enum TxKind {
    /// Contains only vote program instructions.
    VoteOnly,
    /// Contains a mix of vote and non-vote instructions.
    Mixed,
    /// Contains no vote program instructions.
    NonVote,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
/// Runtime slot status classification for local fork/canonical tracking.
pub enum ForkSlotStatus {
    /// Slot has been observed but is not yet locally confirmed/finalized.
    Processed,
    /// Slot is on the locally canonical fork and at/below confirmed watermark.
    Confirmed,
    /// Slot is on the locally canonical fork and at/below finalized watermark.
    Finalized,
    /// Slot was previously on the locally canonical fork and is now detached.
    Orphaned,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
/// Runtime commitment state for an observed transaction slot.
pub enum TxCommitmentStatus {
    /// Transaction was observed from live shred stream, but slot is not yet confirmed.
    Processed,
    /// Transaction slot is at or below current confirmed slot watermark.
    Confirmed,
    /// Transaction slot is at or below current finalized slot watermark.
    Finalized,
}

impl TxCommitmentStatus {
    /// Classifies one transaction slot against current confirmed/finalized slot watermarks.
    #[must_use]
    pub fn from_slot(
        tx_slot: u64,
        confirmed_slot: Option<u64>,
        finalized_slot: Option<u64>,
    ) -> Self {
        if finalized_slot.is_some_and(|slot| tx_slot <= slot) {
            return Self::Finalized;
        }
        if confirmed_slot.is_some_and(|slot| tx_slot <= slot) {
            return Self::Confirmed;
        }
        Self::Processed
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
/// Event emitted by runtime when a transaction is observed.
pub struct TxObservedEvent {
    /// Slot where transaction was observed.
    pub slot: u64,
    /// Transaction signature.
    pub signature: Signature,
    /// Transaction kind classification.
    pub kind: TxKind,
    /// Commitment status at observation time.
    pub commitment_status: TxCommitmentStatus,
}

#[cfg(test)]
mod tests {
    use super::TxCommitmentStatus;

    #[test]
    fn commitment_from_slot_prefers_finalized_when_both_match() {
        let status = TxCommitmentStatus::from_slot(100, Some(120), Some(150));
        assert_eq!(status, TxCommitmentStatus::Finalized);
    }

    #[test]
    fn commitment_from_slot_falls_back_to_processed() {
        let status = TxCommitmentStatus::from_slot(120, Some(119), Some(110));
        assert_eq!(status, TxCommitmentStatus::Processed);
    }
}
