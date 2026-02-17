//! Provider traits and basic in-memory adapters used by the transaction SDK.

use std::net::SocketAddr;

use solana_pubkey::Pubkey;

/// One leader/validator target that can receive transactions directly.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct LeaderTarget {
    /// Optional validator identity.
    pub identity: Option<Pubkey>,
    /// TPU/ingress socket used for direct submit.
    pub tpu_addr: SocketAddr,
}

impl LeaderTarget {
    /// Creates a leader target with optional identity.
    #[must_use]
    pub const fn new(identity: Option<Pubkey>, tpu_addr: SocketAddr) -> Self {
        Self { identity, tpu_addr }
    }
}

/// Source of the latest recent blockhash bytes.
pub trait RecentBlockhashProvider: Send + Sync {
    /// Returns the newest blockhash bytes when available.
    fn latest_blockhash(&self) -> Option<[u8; 32]>;
}

/// Source of current/next leader targets.
pub trait LeaderProvider: Send + Sync {
    /// Returns the currently scheduled leader target.
    fn current_leader(&self) -> Option<LeaderTarget>;

    /// Returns up to `n` upcoming leader targets.
    fn next_leaders(&self, n: usize) -> Vec<LeaderTarget>;
}

/// In-memory blockhash provider for tests and static configurations.
#[derive(Debug, Clone)]
pub struct StaticRecentBlockhashProvider {
    /// Optional static blockhash bytes.
    value: Option<[u8; 32]>,
}

impl StaticRecentBlockhashProvider {
    /// Creates a provider with an optional static blockhash.
    #[must_use]
    pub const fn new(value: Option<[u8; 32]>) -> Self {
        Self { value }
    }
}

impl RecentBlockhashProvider for StaticRecentBlockhashProvider {
    fn latest_blockhash(&self) -> Option<[u8; 32]> {
        self.value
    }
}

/// In-memory leader provider for tests and static configurations.
#[derive(Debug, Clone, Default)]
pub struct StaticLeaderProvider {
    /// Optional current leader.
    current: Option<LeaderTarget>,
    /// Ordered next leaders.
    next: Vec<LeaderTarget>,
}

impl StaticLeaderProvider {
    /// Creates a static leader provider.
    #[must_use]
    pub const fn new(current: Option<LeaderTarget>, next: Vec<LeaderTarget>) -> Self {
        Self { current, next }
    }
}

impl LeaderProvider for StaticLeaderProvider {
    fn current_leader(&self) -> Option<LeaderTarget> {
        self.current.clone()
    }

    fn next_leaders(&self, n: usize) -> Vec<LeaderTarget> {
        self.next.iter().take(n).cloned().collect()
    }
}
