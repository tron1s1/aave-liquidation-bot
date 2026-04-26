// =============================================================================
// src/health_calculator.rs — Pure-math health factor calculation engine
// =============================================================================
// HOT PATH: Zero allocations, integer-only math, no RPC calls.
// Recalculates health factors when oracle prices change.
//
// HF = Σ(collateral_i × price_i × liq_threshold_i / 10000) / Σ(debt_j × price_j)
// All values in base currency (USD, 8 decimals).
// No division before multiplication.

use alloy::primitives::U256;
use crate::types::{ReserveInfo, UserPosition, UserReservePosition};

/// Calculate health factor for a user given current prices and reserve configs.
/// Returns health factor in WAD (1e18) precision.
/// Returns U256::MAX if user has no debt (infinite health factor).
///
/// SECURITY: Pure integer math. No division before multiplication.
/// All intermediate products use U256 (256-bit) to prevent overflow.
#[inline]
pub fn calculate_health_factor(
    user: &UserPosition,
    reserves: &[ReserveInfo],
) -> U256 {
    let mut total_collateral_in_base = U256::ZERO;
    let mut total_debt_in_base = U256::ZERO;

    for user_reserve in &user.reserves {
        // Find matching reserve info by asset address
        let reserve = match reserves.iter().find(|r| r.asset == user_reserve.asset) {
            Some(r) => r,
            None => continue,
        };

        if !reserve.is_active {
            continue;
        }

        let decimals_factor = U256::from(10u64).pow(U256::from(reserve.decimals));

        // Calculate collateral contribution (if used as collateral)
        if user_reserve.is_collateral && !user_reserve.collateral_balance.is_zero() {
            // collateral_in_base = balance * price / 10^decimals
            // Then apply liquidation threshold: collateral_in_base * liq_threshold / 10000
            //
            // Combined: (balance * price * liq_threshold) / (10^decimals * 10000)
            // Multiplication first, division last!
            let numerator = user_reserve.collateral_balance
                .checked_mul(reserve.price_in_base_currency)
                .expect("collateral price overflow")
                .checked_mul(U256::from(reserve.liquidation_threshold_bps))
                .expect("collateral threshold overflow");

            let denominator = decimals_factor
                .checked_mul(U256::from(10_000u64))
                .expect("denominator overflow");

            total_collateral_in_base = total_collateral_in_base
                .checked_add(numerator / denominator)
                .expect("total collateral overflow");
        }

        // Calculate debt contribution
        if user_reserve.is_borrowing {
            let total_user_debt = user_reserve.variable_debt
                .checked_add(user_reserve.stable_debt)
                .expect("user debt sum overflow");

            if !total_user_debt.is_zero() {
                // debt_in_base = total_debt * price / 10^decimals
                let debt_value = total_user_debt
                    .checked_mul(reserve.price_in_base_currency)
                    .expect("debt price overflow");

                total_debt_in_base = total_debt_in_base
                    .checked_add(debt_value / decimals_factor)
                    .expect("total debt overflow");
            }
        }
    }

    // If no debt, health factor is infinite (safe)
    if total_debt_in_base.is_zero() {
        return U256::MAX;
    }

    // HF = total_collateral_in_base * 1e18 / total_debt_in_base
    // Multiply first (by WAD), then divide
    let wad = U256::from(1_000_000_000_000_000_000u64);
    total_collateral_in_base
        .checked_mul(wad)
        .expect("HF numerator overflow")
        / total_debt_in_base
}

/// Determine the best (most profitable) collateral-debt pair for liquidation.
/// Returns (collateral_reserve_index, debt_reserve_index, debt_to_cover, expected_collateral).
#[inline]
pub fn find_best_liquidation_pair(
    user: &UserPosition,
    reserves: &[ReserveInfo],
    close_factor_bps: u64,
) -> Option<LiquidationPair> {
    let mut best: Option<LiquidationPair> = None;
    let mut best_profit_usd = U256::ZERO;

    // Iterate all debt positions
    for debt_reserve_pos in user.reserves.iter().filter(|r| r.is_borrowing) {
        let debt_reserve = match reserves.iter().find(|r| r.asset == debt_reserve_pos.asset) {
            Some(r) => r,
            None => continue,
        };

        let total_debt = debt_reserve_pos.variable_debt
            .checked_add(debt_reserve_pos.stable_debt)
            .unwrap_or(U256::ZERO);
        if total_debt.is_zero() {
            continue;
        }

        // Apply close factor: debt_to_cover = total_debt * close_factor / 10000
        let debt_to_cover = total_debt
            .checked_mul(U256::from(close_factor_bps))
            .expect("close factor overflow")
            / U256::from(10_000u64);

        // Iterate all collateral positions
        for coll_reserve_pos in user.reserves.iter().filter(|r| r.is_collateral) {
            let coll_reserve = match reserves.iter().find(|r| r.asset == coll_reserve_pos.asset) {
                Some(r) => r,
                None => continue,
            };

            if coll_reserve_pos.collateral_balance.is_zero() || coll_reserve.liquidation_bonus_bps == 0 {
                continue;
            }

            // Calculate expected collateral to receive:
            // collateral_amount = (debt_to_cover * debt_price * bonus) / (collateral_price * 10000)
            // Multiply all numerator terms first!
            let debt_decimals = U256::from(10u64).pow(U256::from(debt_reserve.decimals));
            let coll_decimals = U256::from(10u64).pow(U256::from(coll_reserve.decimals));

            // debt value in base currency (8 dec USD)
            let debt_value_base = debt_to_cover
                .checked_mul(debt_reserve.price_in_base_currency)
                .expect("debt value overflow")
                / debt_decimals;

            // collateral amount including bonus
            // = debt_value_base * bonus_bps * 10^coll_decimals / (coll_price * 10000)
            let expected_collateral = debt_value_base
                .checked_mul(U256::from(coll_reserve.liquidation_bonus_bps))
                .expect("bonus overflow")
                .checked_mul(coll_decimals)
                .expect("coll decimals overflow")
                / (coll_reserve.price_in_base_currency
                    .checked_mul(U256::from(10_000u64))
                    .expect("coll price denom overflow"));

            // Check we wouldn't seize more than the user has
            if expected_collateral > coll_reserve_pos.collateral_balance {
                continue; // Skip — not enough collateral
            }

            // Profit in base currency = debt_value_base * (bonus - 10000) / 10000
            let bonus_profit_bps = coll_reserve.liquidation_bonus_bps.saturating_sub(10_000);
            let profit_usd = debt_value_base
                .checked_mul(U256::from(bonus_profit_bps))
                .unwrap_or(U256::ZERO)
                / U256::from(10_000u64);

            if profit_usd > best_profit_usd {
                best_profit_usd = profit_usd;
                best = Some(LiquidationPair {
                    collateral_asset: coll_reserve.asset,
                    debt_asset: debt_reserve.asset,
                    debt_to_cover,
                    expected_collateral,
                    liquidation_bonus_bps: coll_reserve.liquidation_bonus_bps,
                    estimated_profit_usd: profit_usd,
                });
            }
        }
    }

    best
}

/// A specific collateral-debt pair for liquidation
#[derive(Debug, Clone)]
pub struct LiquidationPair {
    pub collateral_asset: alloy::primitives::Address,
    pub debt_asset: alloy::primitives::Address,
    pub debt_to_cover: U256,
    pub expected_collateral: U256,
    pub liquidation_bonus_bps: u16,
    pub estimated_profit_usd: U256,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    fn weth_reserve() -> ReserveInfo {
        ReserveInfo {
            asset: address!("82aF49447D8a07e3bd95BD0d56f35241523fBab1"),
            reserve_index: 0,
            decimals: 18,
            liquidation_threshold_bps: 8250,
            liquidation_bonus_bps: 10500,
            ltv_bps: 8000,
            variable_borrow_rate: U256::ZERO,
            liquidity_rate: U256::ZERO,
            price_in_base_currency: U256::from(200_000_000_000u64), // $2000
            usage_as_collateral_enabled: true,
            borrowing_enabled: true,
            is_active: true,
            symbol: "WETH".to_string(),
        }
    }

    fn usdc_reserve() -> ReserveInfo {
        ReserveInfo {
            asset: address!("AF88d065e77c8cC2239327C5EDb3A432268e5831"),
            reserve_index: 1,
            decimals: 6,
            liquidation_threshold_bps: 8600,
            liquidation_bonus_bps: 10400,
            ltv_bps: 8000,
            variable_borrow_rate: U256::ZERO,
            liquidity_rate: U256::ZERO,
            price_in_base_currency: U256::from(100_000_000u64), // $1.00
            usage_as_collateral_enabled: true,
            borrowing_enabled: true,
            is_active: true,
            symbol: "USDC".to_string(),
        }
    }

    #[test]
    fn test_hf_healthy_position() {
        let reserves = vec![weth_reserve(), usdc_reserve()];
        let user = UserPosition {
            user: address!("0000000000000000000000000000000000000001"),
            reserves: vec![
                UserReservePosition {
                    asset: reserves[0].asset,
                    reserve_index: 0,
                    collateral_balance: U256::from(1_000_000_000_000_000_000u64), // 1 WETH
                    variable_debt: U256::ZERO,
                    stable_debt: U256::ZERO,
                    is_collateral: true,
                    is_borrowing: false,
                },
                UserReservePosition {
                    asset: reserves[1].asset,
                    reserve_index: 1,
                    collateral_balance: U256::ZERO,
                    variable_debt: U256::from(1_000_000_000u64), // 1000 USDC debt
                    stable_debt: U256::ZERO,
                    is_collateral: false,
                    is_borrowing: true,
                },
            ],
            total_collateral_base: U256::ZERO,
            total_debt_base: U256::ZERO,
            health_factor: U256::ZERO,
            config_bitmask: U256::ZERO,
            last_updated: 0,
        };

        let hf = calculate_health_factor(&user, &reserves);
        // HF = (1 WETH * $2000 * 0.825) / (1000 USDC * $1) = 1650/1000 = 1.65
        let expected_hf = U256::from(1_650_000_000_000_000_000u64);
        assert_eq!(hf, expected_hf);
    }

    #[test]
    fn test_hf_no_debt_is_max() {
        let reserves = vec![weth_reserve()];
        let user = UserPosition {
            user: address!("0000000000000000000000000000000000000001"),
            reserves: vec![UserReservePosition {
                asset: reserves[0].asset,
                reserve_index: 0,
                collateral_balance: U256::from(1_000_000_000_000_000_000u64),
                variable_debt: U256::ZERO,
                stable_debt: U256::ZERO,
                is_collateral: true,
                is_borrowing: false,
            }],
            total_collateral_base: U256::ZERO,
            total_debt_base: U256::ZERO,
            health_factor: U256::ZERO,
            config_bitmask: U256::ZERO,
            last_updated: 0,
        };

        let hf = calculate_health_factor(&user, &reserves);
        assert_eq!(hf, U256::MAX);
    }
}
