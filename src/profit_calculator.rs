// =============================================================================
// src/profit_calculator.rs — Net profit calculation after all fees
// =============================================================================
// Determines whether a liquidation opportunity meets the 2.8% minimum threshold.
// Pure integer math, no allocations, deterministic.
//
// Profit = (seized_collateral_value * bonus / 10000) - flash_fee - gas_cost - swap_slippage
// All in base currency (USD, 8 decimals).

use alloy::primitives::U256;

/// Calculate net profit and determine if the opportunity is worth executing.
/// All values in base currency (USD with 8 decimals).
///
/// Returns (net_profit_usd, net_profit_bps, is_profitable)
#[inline]
pub fn calculate_net_profit(
    debt_to_cover_usd: U256,
    liquidation_bonus_bps: u16,
    flash_loan_premium_bps: u64,
    estimated_gas_cost_usd: U256,
    swap_slippage_bps: u64,
    min_profit_bps: u64,
) -> ProfitResult {
    // Gross profit from liquidation bonus
    // bonus_pct = (liquidation_bonus_bps - 10000) / 10000
    // gross_profit = debt_to_cover * bonus_pct
    // = debt_to_cover * (liq_bonus - 10000) / 10000
    let bonus_over_par = liquidation_bonus_bps.saturating_sub(10_000) as u64;
    let gross_profit = debt_to_cover_usd
        .checked_mul(U256::from(bonus_over_par))
        .unwrap_or(U256::ZERO)
        / U256::from(10_000u64);

    // Flash loan fee = debt_to_cover * premium_bps / 10000
    let flash_fee = debt_to_cover_usd
        .checked_mul(U256::from(flash_loan_premium_bps))
        .unwrap_or(U256::ZERO)
        / U256::from(10_000u64);

    // Slippage cost on swap = debt_to_cover * slippage_bps / 10000
    let slippage_cost = debt_to_cover_usd
        .checked_mul(U256::from(swap_slippage_bps))
        .unwrap_or(U256::ZERO)
        / U256::from(10_000u64);

    // Total costs
    let total_costs = flash_fee
        .checked_add(estimated_gas_cost_usd)
        .unwrap_or(U256::MAX)
        .checked_add(slippage_cost)
        .unwrap_or(U256::MAX);

    // Net profit
    let net_profit = if gross_profit > total_costs {
        gross_profit - total_costs
    } else {
        U256::ZERO
    };

    // Calculate net profit in bps relative to debt_to_cover
    // net_profit_bps = net_profit * 10000 / debt_to_cover
    // Multiply first!
    let net_profit_bps = if !debt_to_cover_usd.is_zero() {
        net_profit
            .checked_mul(U256::from(10_000u64))
            .unwrap_or(U256::ZERO)
            / debt_to_cover_usd
    } else {
        U256::ZERO
    };

    let bps_as_u64: u64 = net_profit_bps.try_into().unwrap_or(0);
    let is_profitable = bps_as_u64 >= min_profit_bps;

    ProfitResult {
        gross_profit_usd: gross_profit,
        flash_fee_usd: flash_fee,
        gas_cost_usd: estimated_gas_cost_usd,
        slippage_cost_usd: slippage_cost,
        net_profit_usd: net_profit,
        net_profit_bps: bps_as_u64,
        is_profitable,
    }
}

/// Result of a profit calculation
#[derive(Debug, Clone)]
pub struct ProfitResult {
    pub gross_profit_usd: U256,
    pub flash_fee_usd: U256,
    pub gas_cost_usd: U256,
    pub slippage_cost_usd: U256,
    pub net_profit_usd: U256,
    pub net_profit_bps: u64,
    pub is_profitable: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profitable_liquidation() {
        // $10,000 debt, 5% bonus, 0.05% flash fee, $0.50 gas, 0.3% slippage
        let result = calculate_net_profit(
            U256::from(1_000_000_000_000u64),  // $10,000 (8 dec)
            10_500,                              // 5% bonus
            5,                                   // 0.05% flash fee
            U256::from(50_000_000u64),           // $0.50 gas
            30,                                  // 0.3% slippage
            280,                                 // 2.8% min
        );

        // Gross: $10000 * 500/10000 = $500
        assert_eq!(result.gross_profit_usd, U256::from(50_000_000_000u64));
        // Flash fee: $10000 * 5/10000 = $5
        assert_eq!(result.flash_fee_usd, U256::from(500_000_000u64));
        // Slippage: $10000 * 30/10000 = $30
        assert_eq!(result.slippage_cost_usd, U256::from(3_000_000_000u64));
        // Net: $500 - $5 - $0.50 - $30 = $464.50
        assert_eq!(result.net_profit_usd, U256::from(46_450_000_000u64));
        // BPS: 464.5 / 10000 * 10000 = 464
        assert_eq!(result.net_profit_bps, 464);
        assert!(result.is_profitable);
    }

    #[test]
    fn test_unprofitable_small_bonus() {
        // $100 debt, 2% bonus (below Aave typical), high slippage
        let result = calculate_net_profit(
            U256::from(10_000_000_000u64),     // $100
            10_200,                              // 2% bonus
            5,
            U256::from(50_000_000u64),           // $0.50 gas
            100,                                 // 1% slippage
            280,
        );
        // Gross: $100 * 200/10000 = $2
        // Flash: $100 * 5/10000 = $0.05
        // Slippage: $100 * 100/10000 = $1
        // Net: $2 - $0.05 - $0.50 - $1 = $0.45
        // BPS: 0.45/100 * 10000 = 45 < 280
        assert!(!result.is_profitable);
    }

    #[test]
    fn test_zero_debt() {
        let result = calculate_net_profit(
            U256::ZERO,
            10_500,
            5,
            U256::ZERO,
            30,
            280,
        );
        assert_eq!(result.net_profit_bps, 0);
        assert!(!result.is_profitable);
    }
}
