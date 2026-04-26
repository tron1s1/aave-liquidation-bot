// =============================================================================
// src/watchlist.rs — In-memory concurrent watchlist of at-risk borrowers
// =============================================================================
// Uses DashMap for lock-free concurrent reads/writes.
// Hot path (health factor checks) only reads — zero contention.
// Cold path (TheGraph updates) writes in background — partitioned locks.

use alloy::primitives::{Address, U256};
use dashmap::DashMap;
use std::sync::Arc;
use tracing::{debug, info};

use crate::types::UserPosition;
use crate::metrics::METRICS;

/// Thread-safe watchlist of at-risk Aave V3 borrowers.
/// Key: user address, Value: their full position data.
pub struct Watchlist {
    /// Lock-free concurrent map of monitored users
    positions: DashMap<Address, UserPosition>,
}

impl Watchlist {
    pub fn new() -> Self {
        Self {
            positions: DashMap::with_capacity(20_000),
        }
    }

    /// Insert or update a user's position
    pub fn upsert(&self, user: Address, position: UserPosition) {
        self.positions.insert(user, position);
    }

    /// Bulk insert/update from TheGraph cold start
    pub fn bulk_update(&self, users: Vec<UserPosition>) {
        let count = users.len();
        for user_pos in users {
            self.positions.insert(user_pos.user, user_pos);
        }
        METRICS.set_watchlist_size(self.positions.len() as u64);
        info!(count, total = self.positions.len(), "Watchlist bulk updated");
    }

    /// Remove a user (e.g., after successful liquidation or HF recovered)
    pub fn remove(&self, user: &Address) {
        self.positions.remove(user);
        METRICS.set_watchlist_size(self.positions.len() as u64);
    }

    /// Get a snapshot of a user's position (clones the data, no lock held)
    pub fn get(&self, user: &Address) -> Option<UserPosition> {
        self.positions.get(user).map(|entry| entry.value().clone())
    }

    /// Get all users with health factor below the given threshold.
    /// Used in the hot path when prices update — iterates without locking.
    pub fn get_underwater_users(&self, hf_threshold: U256) -> Vec<UserPosition> {
        let mut result = Vec::with_capacity(100);
        for entry in self.positions.iter() {
            if entry.value().health_factor < hf_threshold {
                result.push(entry.value().clone());
            }
        }
        debug!(count = result.len(), "Found underwater users");
        result
    }

    /// Get the total number of monitored users
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// Check if watchlist is empty
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Update a user's health factor in-place (called from local math engine)
    /// Returns true if the user is now underwater (HF < 1e18)
    pub fn update_health_factor(&self, user: &Address, new_hf: U256) -> bool {
        if let Some(mut entry) = self.positions.get_mut(user) {
            entry.health_factor = new_hf;
            // 1e18 = WAD = 1.0 health factor
            new_hf < U256::from(1_000_000_000_000_000_000u64)
        } else {
            false
        }
    }

    /// Update prices for all users holding a specific reserve (by index)
    /// and recalculate their health factors.
    /// This is the core hot-path function called when oracle prices update.
    pub fn update_reserve_price(
        &self,
        _reserve_index: u16,
        _new_price: U256,
    ) -> Vec<Address> {
        // TODO: Phase 4 will implement full HF recalculation here
        // For now, return empty vec
        Vec::new()
    }
}

/// Create a shared watchlist wrapped in Arc for multi-task access
pub fn create_watchlist() -> Arc<Watchlist> {
    Arc::new(Watchlist::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    fn make_test_position(user: Address, hf: u64) -> UserPosition {
        UserPosition {
            user,
            reserves: vec![],
            total_collateral_base: U256::from(10_000_000_000u64), // $100
            total_debt_base: U256::from(8_000_000_000u64),        // $80
            health_factor: U256::from(hf),
            config_bitmask: U256::ZERO,
            last_updated: 0,
        }
    }

    #[test]
    fn test_upsert_and_get() {
        let wl = Watchlist::new();
        let user = address!("0000000000000000000000000000000000000001");
        let pos = make_test_position(user, 1_050_000_000_000_000_000); // HF 1.05
        wl.upsert(user, pos.clone());
        assert_eq!(wl.len(), 1);
        let got = wl.get(&user).unwrap();
        assert_eq!(got.user, user);
    }

    #[test]
    fn test_get_underwater() {
        let wl = Watchlist::new();
        let user1 = address!("0000000000000000000000000000000000000001");
        let user2 = address!("0000000000000000000000000000000000000002");

        // user1: HF 0.95 (underwater)
        wl.upsert(user1, make_test_position(user1, 950_000_000_000_000_000));
        // user2: HF 1.05 (safe)
        wl.upsert(user2, make_test_position(user2, 1_050_000_000_000_000_000));

        let threshold = U256::from(1_000_000_000_000_000_000u64); // 1.0
        let underwater = wl.get_underwater_users(threshold);
        assert_eq!(underwater.len(), 1);
        assert_eq!(underwater[0].user, user1);
    }

    #[test]
    fn test_update_health_factor() {
        let wl = Watchlist::new();
        let user = address!("0000000000000000000000000000000000000001");
        wl.upsert(user, make_test_position(user, 1_050_000_000_000_000_000));

        // Update to underwater
        let is_underwater = wl.update_health_factor(
            &user,
            U256::from(900_000_000_000_000_000u64), // 0.9
        );
        assert!(is_underwater);

        // Verify updated
        let pos = wl.get(&user).unwrap();
        assert_eq!(pos.health_factor, U256::from(900_000_000_000_000_000u64));
    }
}
