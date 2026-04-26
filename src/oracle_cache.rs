use alloy::primitives::{Address, U256};
use dashmap::DashMap;
use std::time::{Duration, Instant};

/// Cache for on-chain oracle prices.
/// Prices are fetched lazily during execution and cached for a TTL.
/// This prevents blocking the hot path with an RPC call for every liquidation.
pub struct OracleCache {
    prices: DashMap<Address, (U256, Instant)>,
    ttl: Duration,
}

impl OracleCache {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            prices: DashMap::new(),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    /// Retrieve the cached price if it exists and hasn't expired.
    pub fn get_price(&self, asset: &Address) -> Option<U256> {
        if let Some(entry) = self.prices.get(asset) {
            if entry.value().1.elapsed() < self.ttl {
                return Some(entry.value().0);
            }
        }
        None
    }

    /// Insert or update the cached price for an asset.
    pub fn set_price(&self, asset: Address, price: U256) {
        self.prices.insert(asset, (price, Instant::now()));
    }
}
