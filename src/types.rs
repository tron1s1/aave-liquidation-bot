// =============================================================================
// src/types.rs — Shared types, constants, and Aave V3 Arbitrum addresses
// =============================================================================
// All monetary values use U256 (no floating point anywhere in the codebase).
// Rates and prices use Ray (1e27) precision as defined by Aave V3.
// Addresses are verified against the official Aave Address Book.

use alloy::primitives::{Address, U256, address};
use serde::{Deserialize, Serialize};

// ─── Mathematical Constants (Aave V3 Precision) ─────────────────────────────

/// Ray = 1e27 — used for interest rates, health factors, indices
pub const RAY: U256 = U256::from_limbs([1_000_000_000_000_000_000_000_000_000u128 as u64, 
    (1_000_000_000_000_000_000_000_000_000u128 >> 64) as u64, 0, 0]);
/// WAD = 1e18 — used for token amounts, ETH values
pub const WAD: U256 = U256::from_limbs([1_000_000_000_000_000_000u64, 0, 0, 0]);
/// Basis points denominator
pub const BPS_DENOMINATOR: u64 = 10_000;
/// Half Ray for rounding (5e26)
pub const HALF_RAY: U256 = U256::from_limbs([500_000_000_000_000_000_000_000_000u128 as u64,
    (500_000_000_000_000_000_000_000_000u128 >> 64) as u64, 0, 0]);

// ─── Aave V3 Arbitrum Contract Addresses ─────────────────────────────────────
// Source: https://github.com/aave/aave-address-book (verified on Arbiscan)

/// PoolAddressesProvider — root registry, immutable. All other addresses derived from this.
pub const POOL_ADDRESSES_PROVIDER: Address = address!("a97684ead0e402dC232d5A977953DF7ECBaB3CDb");

/// Uniswap V3 SwapRouter02 on Arbitrum
pub const UNISWAP_V3_ROUTER: Address = address!("68b3465833fb72A70ecDF485E0e4C7bD8665Fc45");

/// Aave V3 Oracle on Arbitrum (Chainlink-based, prices in USD with 8 decimals)
pub const AAVE_ORACLE: Address = address!("b56c2F0B653B2e0b10C9b928C8580Ac5Df02C7C7");

/// Aave V3 flash loan premium in basis points (0.05%)
pub const FLASH_LOAN_PREMIUM_BPS: u64 = 5;

/// Default max slippage for Uniswap swaps in basis points (0.5%)
pub const DEFAULT_MAX_SLIPPAGE_BPS: u64 = 50;

/// Minimum net profit in basis points (2.8%)
pub const MIN_PROFIT_BPS: u64 = 280;

/// Health factor threshold for full liquidation (0.95 in WAD)
pub const FULL_LIQUIDATION_HF_THRESHOLD: U256 = U256::from_limbs([950_000_000_000_000_000u64, 0, 0, 0]);

/// Small position threshold for full liquidation ($2000 in 8-decimal USD)
pub const SMALL_POSITION_THRESHOLD: U256 = U256::from_limbs([200_000_000_000u64, 0, 0, 0]);

/// Default close factor (50% = 5000 bps)
pub const DEFAULT_CLOSE_FACTOR_BPS: u64 = 5_000;

/// Full close factor (100% = 10000 bps)
pub const FULL_CLOSE_FACTOR_BPS: u64 = 10_000;

// ─── Core Data Structures ────────────────────────────────────────────────────

/// Information about a single Aave V3 reserve (asset), cached locally
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReserveInfo {
    /// The underlying token address (e.g., WETH, USDC)
    pub asset: Address,
    /// Index in the Aave reserves list (used for bitmask decoding)
    pub reserve_index: u16,
    /// Token decimals (e.g., 18 for WETH, 6 for USDC)
    pub decimals: u8,
    /// Liquidation threshold in basis points (e.g., 8250 = 82.5%)
    pub liquidation_threshold_bps: u16,
    /// Liquidation bonus in basis points (e.g., 10500 = 105% means 5% bonus)
    pub liquidation_bonus_bps: u16,
    /// Loan-to-value ratio in basis points
    pub ltv_bps: u16,
    /// Current variable borrow rate (Ray)
    pub variable_borrow_rate: U256,
    /// Current liquidity rate (Ray)
    pub liquidity_rate: U256,
    /// Latest oracle price in base currency (USD, 8 decimals)
    pub price_in_base_currency: U256,
    /// Whether the reserve can be used as collateral
    pub usage_as_collateral_enabled: bool,
    /// Whether borrowing is enabled
    pub borrowing_enabled: bool,
    /// Whether the reserve is active and not frozen
    pub is_active: bool,
    /// Human-readable symbol (for logging)
    pub symbol: String,
}

/// A single user's reserve position (one row in userReserves)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserReservePosition {
    /// Reserve asset address
    pub asset: Address,
    /// Reserve index for bitmask lookup
    pub reserve_index: u16,
    /// Current aToken balance (collateral) — in token units
    pub collateral_balance: U256,
    /// Current variable debt — in token units
    pub variable_debt: U256,
    /// Current stable debt — in token units  
    pub stable_debt: U256,
    /// Whether this reserve is being used as collateral by the user
    pub is_collateral: bool,
    /// Whether the user is borrowing this reserve
    pub is_borrowing: bool,
}

/// A user's full position across all reserves
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPosition {
    /// User's wallet address
    pub user: Address,
    /// All reserve positions for this user
    pub reserves: Vec<UserReservePosition>,
    /// Total collateral value in base currency (USD, 8 decimals)
    pub total_collateral_base: U256,
    /// Total debt value in base currency (USD, 8 decimals)
    pub total_debt_base: U256,
    /// Health factor (WAD, 1e18 precision)
    pub health_factor: U256,
    /// Raw getUserConfiguration bitmask
    pub config_bitmask: U256,
    /// Timestamp of last update
    pub last_updated: u64,
}

/// A detected liquidation opportunity ready for execution
#[derive(Debug, Clone)]
pub struct LiquidationOpportunity {
    /// The borrower to liquidate
    pub user: Address,
    /// The collateral asset to seize
    pub collateral_asset: Address,
    /// The debt asset to repay
    pub debt_asset: Address,
    /// Amount of debt to repay (in token units)
    pub debt_to_cover: U256,
    /// Expected collateral to receive (in token units, including bonus)
    pub expected_collateral_amount: U256,
    /// Liquidation bonus for this collateral (bps, e.g., 10500)
    pub liquidation_bonus_bps: u16,
    /// Estimated net profit in base currency (USD, 8 decimals)
    pub estimated_profit_usd: U256,
    /// Estimated net profit in basis points
    pub estimated_profit_bps: u64,
    /// User's current health factor
    pub user_health_factor: U256,
    /// Whether full liquidation is allowed (HF < 0.95 or small position)
    pub full_liquidation: bool,
    /// Timestamp when this opportunity was detected
    pub detected_at_ms: u64,
}

/// Result of a simulation (eth_call dry run)
#[derive(Debug, Clone)]
pub struct SimulationResult {
    /// Whether the simulation succeeded
    pub success: bool,
    /// Gas used in the simulation
    pub gas_used: u64,
    /// Revert reason if failed
    pub revert_reason: Option<String>,
    /// Actual profit from simulation (if success)
    pub actual_profit: Option<U256>,
}

/// Result of an execution (real TX or paper mode)
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// Whether this was paper mode or live
    pub paper_mode: bool,
    /// Transaction hash (None in paper mode)
    pub tx_hash: Option<alloy::primitives::B256>,
    /// Gas used
    pub gas_used: u64,
    /// Gas price paid (in wei)
    pub gas_price: U256,
    /// Net profit (in debt asset token units)
    pub net_profit: U256,
    /// The opportunity that was executed
    pub opportunity: LiquidationOpportunity,
}

// ─── Bitmask Helpers ─────────────────────────────────────────────────────────

/// Decode the Aave V3 getUserConfiguration bitmask.
/// Each reserve uses 2 bits: bit (index * 2) = borrowing, bit (index * 2 + 1) = collateral.
/// Returns (is_borrowing, is_collateral) for the given reserve index.
#[inline(always)]
pub fn decode_user_config(bitmask: U256, reserve_index: u16) -> (bool, bool) {
    let borrow_bit = U256::from(1u64) << (reserve_index as usize * 2);
    let collateral_bit = U256::from(1u64) << (reserve_index as usize * 2 + 1);
    let is_borrowing = !(bitmask & borrow_bit).is_zero();
    let is_collateral = !(bitmask & collateral_bit).is_zero();
    (is_borrowing, is_collateral)
}

/// Determine the close factor for a given position.
/// Returns basis points (5000 = 50%, 10000 = 100%).
#[inline(always)]
pub fn get_close_factor(health_factor: U256, total_debt_base: U256) -> u64 {
    // Full liquidation if HF < 0.95 or position < $2000
    if health_factor < FULL_LIQUIDATION_HF_THRESHOLD || total_debt_base < SMALL_POSITION_THRESHOLD {
        FULL_CLOSE_FACTOR_BPS
    } else {
        DEFAULT_CLOSE_FACTOR_BPS
    }
}

/// Safe Ray multiplication: (a * b + HALF_RAY) / RAY
/// Follows Aave's WadRayMath pattern. No division before multiplication.
#[inline(always)]
pub fn ray_mul(a: U256, b: U256) -> U256 {
    // a * b first (no division before multiplication)
    let product = a.checked_mul(b).expect("ray_mul overflow");
    let rounded = product.checked_add(HALF_RAY).expect("ray_mul round overflow");
    rounded / RAY
}

/// Safe Ray division: (a * RAY + b/2) / b
#[inline(always)]
pub fn ray_div(a: U256, b: U256) -> U256 {
    assert!(!b.is_zero(), "ray_div by zero");
    let scaled = a.checked_mul(RAY).expect("ray_div overflow");
    let half_b = b / U256::from(2u64);
    let rounded = scaled.checked_add(half_b).expect("ray_div round overflow");
    rounded / b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_user_config_borrowing_only() {
        // Reserve index 0, bit 0 set = borrowing, bit 1 not set = not collateral
        let bitmask = U256::from(0b01u64);
        let (borrowing, collateral) = decode_user_config(bitmask, 0);
        assert!(borrowing);
        assert!(!collateral);
    }

    #[test]
    fn test_decode_user_config_collateral_only() {
        // Reserve index 0, bit 0 not set, bit 1 set = collateral
        let bitmask = U256::from(0b10u64);
        let (borrowing, collateral) = decode_user_config(bitmask, 0);
        assert!(!borrowing);
        assert!(collateral);
    }

    #[test]
    fn test_decode_user_config_both() {
        // Reserve index 0, both bits set
        let bitmask = U256::from(0b11u64);
        let (borrowing, collateral) = decode_user_config(bitmask, 0);
        assert!(borrowing);
        assert!(collateral);
    }

    #[test]
    fn test_decode_user_config_higher_index() {
        // Reserve index 3 = bits 6 and 7
        let bitmask = U256::from(0b11_000000u64); // bits 6,7 set
        let (borrowing, collateral) = decode_user_config(bitmask, 3);
        assert!(borrowing);
        assert!(collateral);
    }

    #[test]
    fn test_close_factor_normal() {
        // HF = 0.99 (above 0.95), debt = $10000 (above $2000)
        let hf = U256::from(990_000_000_000_000_000u64); // 0.99 WAD
        let debt = U256::from(1_000_000_000_000u64);      // $10000 (8 dec)
        assert_eq!(get_close_factor(hf, debt), 5000);
    }

    #[test]
    fn test_close_factor_full_low_hf() {
        // HF = 0.94 (below 0.95)
        let hf = U256::from(940_000_000_000_000_000u64);
        let debt = U256::from(1_000_000_000_000u64);
        assert_eq!(get_close_factor(hf, debt), 10_000);
    }

    #[test]
    fn test_close_factor_full_small_position() {
        // HF = 0.99, debt = $1500 (below $2000)
        let hf = U256::from(990_000_000_000_000_000u64);
        let debt = U256::from(150_000_000_000u64); // $1500 (8 dec)
        assert_eq!(get_close_factor(hf, debt), 10_000);
    }

    #[test]
    fn test_ray_mul_identity() {
        let result = ray_mul(RAY, RAY);
        assert_eq!(result, RAY);
    }

    #[test]
    fn test_ray_mul_half() {
        let half = RAY / U256::from(2u64);
        let result = ray_mul(RAY, half);
        assert_eq!(result, half);
    }
}
