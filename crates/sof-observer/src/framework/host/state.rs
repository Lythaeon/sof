use super::*;

/// Internal snapshot for latest observed recent blockhash state.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) struct ObservedRecentBlockhashState {
    /// Slot where the recent blockhash was most recently observed.
    pub(super) slot: u64,
    /// Observed recent blockhash bytes.
    pub(super) recent_blockhash: [u8; 32],
}

/// Internal snapshot for latest observed TPU leader state.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) struct ObservedTpuLeaderState {
    /// Slot for the latest observed TPU leader.
    pub(super) slot: u64,
    /// TPU leader identity.
    pub(super) leader: Pubkey,
}
