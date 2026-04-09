//! Routing policy, target selection, and signature-level duplicate suppression.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

use sof_types::SignatureBytes;

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
pub fn select_targets<P>(
    leader_provider: &P,
    backups: &[LeaderTarget],
    policy: RoutingPolicy,
) -> Vec<LeaderTarget>
where
    P: LeaderProvider + ?Sized,
{
    let policy = policy.normalized();
    let dynamic_backup_count = if backups.is_empty() {
        policy.backup_validators
    } else {
        0
    };
    let requested_next = policy.next_leaders.saturating_add(dynamic_backup_count);
    let estimated_targets = 1_usize
        .saturating_add(requested_next)
        .saturating_add(policy.backup_validators);
    let mut seen = HashSet::with_capacity(estimated_targets);
    let mut selected = Vec::with_capacity(estimated_targets);

    if let Some(current) = leader_provider.current_leader()
        && seen.insert(current.tpu_addr)
    {
        selected.push(current);
    }

    for target in leader_provider.next_leaders(requested_next) {
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
    seen: HashMap<SignatureBytes, Instant>,
    /// Arrival order for bounded eviction without rescanning the whole map.
    order: VecDeque<(SignatureBytes, Instant)>,
}

impl SignatureDeduper {
    /// Creates a dedupe window with a minimum TTL of one millisecond.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl: ttl.max(Duration::from_millis(1)),
            seen: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Returns true when signature is new (and records it), false when duplicate.
    pub fn check_and_insert(&mut self, signature: SignatureBytes, now: Instant) -> bool {
        self.evict_expired(now);
        if self.seen.contains_key(&signature) {
            return false;
        }
        let _ = self.seen.insert(signature, now);
        self.order.push_back((signature, now));
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
        while let Some((signature, first_seen)) = self.order.front().copied() {
            if now.saturating_duration_since(first_seen) < self.ttl {
                break;
            }
            self.order.pop_front();
            if self.seen.get(&signature).copied() == Some(first_seen) {
                let _ = self.seen.remove(&signature);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{LeaderTarget, StaticLeaderProvider};

    fn avg_ns_per_iteration(elapsed: Duration, iterations: usize) -> u128 {
        elapsed
            .as_nanos()
            .checked_div(u128::try_from(iterations.max(1)).unwrap_or(1))
            .unwrap_or(0)
    }

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
    fn select_targets_uses_dynamic_backups_when_static_backups_are_absent() {
        let provider = StaticLeaderProvider::new(
            Some(target(9010)),
            vec![target(9011), target(9012), target(9013), target(9014)],
        );
        let selected = select_targets(
            &provider,
            &[],
            RoutingPolicy {
                next_leaders: 2,
                backup_validators: 2,
                max_parallel_sends: 8,
            },
        );
        assert_eq!(
            selected,
            vec![
                target(9010),
                target(9011),
                target(9012),
                target(9013),
                target(9014)
            ]
        );
    }

    #[test]
    fn deduper_rejects_recent_duplicate_and_allows_after_ttl() {
        let signature = SignatureBytes::from([7_u8; 64]);
        let now = Instant::now();
        let mut deduper = SignatureDeduper::new(Duration::from_millis(25));
        assert!(deduper.check_and_insert(signature, now));
        assert!(!deduper.check_and_insert(signature, now + Duration::from_millis(5)));
        assert!(deduper.check_and_insert(signature, now + Duration::from_millis(30)));
    }

    #[test]
    #[ignore = "profiling fixture for signature dedupe churn"]
    fn signature_deduper_profile_fixture() {
        let iterations = std::env::var("SOF_TX_SIGNATURE_DEDUPER_PROFILE_ITERS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(50_000);
        let ttl_ms = std::env::var("SOF_TX_SIGNATURE_DEDUPER_PROFILE_TTL_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(10_000);
        let mut deduper = SignatureDeduper::new(Duration::from_millis(ttl_ms));
        let start = Instant::now();
        let now = Instant::now();

        for index in 0..iterations {
            let mut signature = [0_u8; 64];
            signature[..8].copy_from_slice(&(index as u64).to_le_bytes());
            assert!(deduper.check_and_insert(
                SignatureBytes::from(signature),
                now + Duration::from_nanos(index as u64)
            ));
        }

        let elapsed = start.elapsed();
        let avg_ns = avg_ns_per_iteration(elapsed, iterations);
        println!(
            "signature_deduper_profile_fixture iterations={} ttl_ms={} entries={} elapsed_us={} avg_ns_per_iteration={} avg_us_per_iteration={:.3}",
            iterations,
            ttl_ms,
            deduper.len(),
            elapsed.as_micros(),
            avg_ns,
            avg_ns as f64 / 1_000.0
        );
    }
}
