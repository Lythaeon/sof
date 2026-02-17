//! Routing policy, target selection, and signature-level duplicate suppression.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use solana_signature::Signature;

use crate::providers::{LeaderProvider, LeaderTarget};

/// Routing controls used for direct and hybrid submit paths.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct RoutingPolicy {
    /// Number of upcoming leaders to include after current leader.
    pub next_leaders: usize,
    /// Number of static backup validators to include.
    pub backup_validators: usize,
    /// Maximum concurrent direct sends.
    pub max_parallel_sends: usize,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            next_leaders: 2,
            backup_validators: 1,
            max_parallel_sends: 4,
        }
    }
}

impl RoutingPolicy {
    /// Returns a normalized policy with bounded minimums.
    #[must_use]
    pub fn normalized(self) -> Self {
        Self {
            next_leaders: self.next_leaders,
            backup_validators: self.backup_validators,
            max_parallel_sends: self.max_parallel_sends.max(1),
        }
    }
}

/// Selects leader/backup targets in deterministic order.
#[must_use]
pub fn select_targets(
    leader_provider: &dyn LeaderProvider,
    backups: &[LeaderTarget],
    policy: RoutingPolicy,
) -> Vec<LeaderTarget> {
    let policy = policy.normalized();
    let mut seen = HashSet::new();
    let mut selected = Vec::new();

    if let Some(current) = leader_provider.current_leader()
        && seen.insert(current.tpu_addr)
    {
        selected.push(current);
    }

    for target in leader_provider.next_leaders(policy.next_leaders) {
        if seen.insert(target.tpu_addr) {
            selected.push(target);
        }
    }

    for target in backups.iter().take(policy.backup_validators) {
        if seen.insert(target.tpu_addr) {
            selected.push(target.clone());
        }
    }

    selected
}

/// Deduplicates transaction signatures for a bounded time window.
#[derive(Debug)]
pub struct SignatureDeduper {
    /// Time-to-live for seen signatures.
    ttl: Duration,
    /// Last seen timestamps by signature.
    seen: HashMap<Signature, Instant>,
}

impl SignatureDeduper {
    /// Creates a dedupe window with a minimum TTL of one millisecond.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl: ttl.max(Duration::from_millis(1)),
            seen: HashMap::new(),
        }
    }

    /// Returns true when signature is new (and records it), false when duplicate.
    pub fn check_and_insert(&mut self, signature: Signature, now: Instant) -> bool {
        self.evict_expired(now);
        if self.seen.contains_key(&signature) {
            return false;
        }
        let _ = self.seen.insert(signature, now);
        true
    }

    /// Returns number of signatures currently tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Returns true when no signatures are currently tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Removes all expired signature entries.
    fn evict_expired(&mut self, now: Instant) {
        let ttl = self.ttl;
        self.seen
            .retain(|_, first_seen| now.saturating_duration_since(*first_seen) < ttl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{LeaderTarget, StaticLeaderProvider};

    fn target(port: u16) -> LeaderTarget {
        LeaderTarget::new(None, std::net::SocketAddr::from(([127, 0, 0, 1], port)))
    }

    #[test]
    fn select_targets_prefers_current_next_then_backups() {
        let provider =
            StaticLeaderProvider::new(Some(target(9001)), vec![target(9002), target(9003)]);
        let backups = vec![target(9004), target(9005)];
        let selected = select_targets(
            &provider,
            &backups,
            RoutingPolicy {
                next_leaders: 2,
                backup_validators: 1,
                max_parallel_sends: 8,
            },
        );
        assert_eq!(
            selected,
            vec![target(9001), target(9002), target(9003), target(9004)]
        );
    }

    #[test]
    fn deduper_rejects_recent_duplicate_and_allows_after_ttl() {
        let signature = Signature::from([7_u8; 64]);
        let now = Instant::now();
        let mut deduper = SignatureDeduper::new(Duration::from_millis(25));
        assert!(deduper.check_and_insert(signature, now));
        assert!(!deduper.check_and_insert(signature, now + Duration::from_millis(5)));
        assert!(deduper.check_and_insert(signature, now + Duration::from_millis(30)));
    }
}
