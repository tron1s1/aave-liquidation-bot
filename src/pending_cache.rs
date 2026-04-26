// =============================================================================
// src/pending_cache.rs — Deduplication cache for in-flight liquidations
// =============================================================================
// Prevents the bot from spamming transactions for the same user while a
// previous liquidation attempt is still in-flight or recently completed.
//
// Without this cache, the bot would submit duplicate TXs for every scan cycle
// (~every 5 seconds), burning gas on redundant attempts. In paper mode testing
// this manifested as 3.8M duplicate log entries in a single run.
//
// Design:
//   - DashMap<Address, Instant> tracks when each user was last claimed
//   - TTL-based expiry: claims expire after `ttl` duration
//   - try_claim() is atomic: check + insert in one operation
//   - release() allows early removal (e.g., after TX failure)

use alloy::primitives::Address;
use dashmap::DashMap;
use std::time::{Duration, Instant};
use tracing::debug;

/// Tracks in-flight liquidation attempts to prevent duplicate submissions.
pub struct PendingCache {
    /// Map of user address → timestamp when they were claimed
    entries: DashMap<Address, Instant>,
    /// How long a claim stays valid before auto-expiring
    ttl: Duration,
}

impl PendingCache {
    /// Create a new cache with the given TTL for entries.
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: DashMap::with_capacity(256),
            ttl,
        }
    }

    /// Try to claim a user for liquidation.
    /// Returns `true` if the user is NOT already pending (proceed with liquidation).
    /// Returns `false` if the user IS already pending (skip this user).
    ///
    /// Automatically handles TTL expiry: if a previous claim has expired,
    /// the user can be re-claimed.
    ///
    /// Uses `DashMap::entry()` for atomic check-and-insert (no TOCTOU gap).
    pub fn try_claim(&self, user: &Address) -> bool {
        use dashmap::mapref::entry::Entry;
        let now = Instant::now();

        match self.entries.entry(*user) {
            Entry::Occupied(mut existing) => {
                if now.duration_since(*existing.get()) < self.ttl {
                    // Still within TTL — skip this user
                    false
                } else {
                    // Expired — reclaim atomically
                    existing.insert(now);
                    true
                }
            }
            Entry::Vacant(empty) => {
                // New entry — claim it
                empty.insert(now);
                true
            }
        }
    }

    /// Release a user from the pending cache.
    /// Call this after a transaction fails or is confirmed, allowing
    /// the user to be re-evaluated on the next scan.
    pub fn release(&self, user: &Address) {
        self.entries.remove(user);
    }

    /// Remove all expired entries from the cache.
    /// Should be called periodically (e.g., every 60 seconds) to prevent
    /// unbounded memory growth from stale entries.
    pub fn cleanup_expired(&self) {
        let now = Instant::now();
        let before = self.entries.len();
        self.entries.retain(|_, instant| {
            now.duration_since(*instant) < self.ttl
        });
        let removed = before - self.entries.len();
        if removed > 0 {
            debug!(removed, remaining = self.entries.len(), "Pending cache cleanup");
        }
    }

    /// Number of currently pending (non-expired) entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    #[test]
    fn test_claim_and_reject_duplicate() {
        let cache = PendingCache::new(Duration::from_secs(60));
        let user = address!("0000000000000000000000000000000000000001");

        // First claim should succeed
        assert!(cache.try_claim(&user));
        // Second claim within TTL should fail
        assert!(!cache.try_claim(&user));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_release_allows_reclaim() {
        let cache = PendingCache::new(Duration::from_secs(60));
        let user = address!("0000000000000000000000000000000000000001");

        assert!(cache.try_claim(&user));
        assert!(!cache.try_claim(&user)); // blocked

        cache.release(&user);
        assert!(cache.try_claim(&user)); // allowed again
    }

    #[test]
    fn test_different_users_independent() {
        let cache = PendingCache::new(Duration::from_secs(60));
        let user1 = address!("0000000000000000000000000000000000000001");
        let user2 = address!("0000000000000000000000000000000000000002");

        assert!(cache.try_claim(&user1));
        assert!(cache.try_claim(&user2)); // different user, should succeed
        assert!(!cache.try_claim(&user1)); // same user, blocked
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn test_expired_entry_can_be_reclaimed() {
        // Use a very short TTL for testing
        let cache = PendingCache::new(Duration::from_millis(1));
        let user = address!("0000000000000000000000000000000000000001");

        assert!(cache.try_claim(&user));
        // Wait for TTL to expire
        std::thread::sleep(Duration::from_millis(5));
        // Should be claimable again after TTL
        assert!(cache.try_claim(&user));
    }

    #[test]
    fn test_cleanup_removes_expired() {
        let cache = PendingCache::new(Duration::from_millis(1));
        let user = address!("0000000000000000000000000000000000000001");

        cache.try_claim(&user);
        assert_eq!(cache.len(), 1);

        std::thread::sleep(Duration::from_millis(5));
        cache.cleanup_expired();
        assert_eq!(cache.len(), 0);
    }
}
