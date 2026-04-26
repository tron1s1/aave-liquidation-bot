// =============================================================================
// src/simulator.rs — Pre-execution simulation via eth_call
// =============================================================================
// Before sending any real transaction, we simulate the entire liquidation
// using eth_call against the latest block state. This catches:
// - Position already liquidated by a competitor
// - Insufficient collateral
// - Swap slippage too high
// - Any unexpected revert
//
// Zero risk: eth_call costs no gas and changes no state.

use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use eyre::{Result, WrapErr};
use tracing::{debug, info, warn};

use crate::types::{LiquidationOpportunity, SimulationResult};

/// Simulate a liquidation by encoding the full transaction and running eth_call.
///
/// Returns SimulationResult with success/failure, gas used, and revert reason.
pub async fn simulate_liquidation<P: Provider>(
    provider: &P,
    executor_address: Address,
    opportunity: &LiquidationOpportunity,
    swap_pool_fee: u32,
    min_amount_out: U256,
    from: Address,
) -> Result<SimulationResult> {
    // Encode the initiateFlashLoan calldata
    let calldata = encode_initiate_flash_loan(
        opportunity.debt_asset,
        opportunity.debt_to_cover,
        opportunity.collateral_asset,
        opportunity.user,
        swap_pool_fee,
        min_amount_out,
    );

    // Build the transaction request for eth_call
    let tx_request = TransactionRequest::default()
        .from(from)
        .to(executor_address)
        .input(calldata.into());

    debug!(
        user = ?opportunity.user,
        debt = ?opportunity.debt_asset,
        collateral = ?opportunity.collateral_asset,
        debt_to_cover = ?opportunity.debt_to_cover,
        "Simulating liquidation via eth_call..."
    );

    // Execute the simulation
    match provider.call(tx_request).await {
        Ok(output) => {
            info!(
                user = ?opportunity.user,
                output_len = output.len(),
                "Simulation succeeded"
            );
            Ok(SimulationResult {
                success: true,
                gas_used: 0, // eth_call doesn't report gas by default
                revert_reason: None,
                actual_profit: None, // Would need to decode output
            })
        }
        Err(e) => {
            let error_msg = format!("{:#}", e);
            warn!(
                user = ?opportunity.user,
                error = %error_msg,
                "Simulation REVERTED"
            );
            Ok(SimulationResult {
                success: false,
                gas_used: 0,
                revert_reason: Some(error_msg),
                actual_profit: None,
            })
        }
    }
}

/// Encode the initiateFlashLoan function calldata.
/// Matches the Solidity function signature exactly.
fn encode_initiate_flash_loan(
    debt_asset: Address,
    debt_to_cover: U256,
    collateral_asset: Address,
    user: Address,
    swap_pool_fee: u32,
    min_amount_out: U256,
) -> Vec<u8> {
    use alloy::sol;
    use alloy::sol_types::SolCall;

    sol! {
        function initiateFlashLoan(
            address debtAsset,
            uint256 debtToCover,
            address collateralAsset,
            address user,
            uint24 swapPoolFee,
            uint256 minAmountOut
        ) external;
    }

    let call = initiateFlashLoanCall {
        debtAsset: debt_asset,
        debtToCover: debt_to_cover,
        collateralAsset: collateral_asset,
        user,
        swapPoolFee: alloy::primitives::Uint::<24, 1>::from(swap_pool_fee),
        minAmountOut: min_amount_out,
    };

    call.abi_encode()
}

/// Estimate the swap output for a given collateral → debt swap on Uniswap V3.
/// Used to calculate minAmountOut for slippage protection.
pub fn calculate_min_amount_out(
    collateral_amount: U256,
    collateral_price: U256,
    debt_price: U256,
    collateral_decimals: u8,
    debt_decimals: u8,
    max_slippage_bps: u64,
) -> U256 {
    if collateral_price.is_zero() || debt_price.is_zero() {
        return U256::ZERO;
    }

    let coll_decimals_factor = U256::from(10u64).pow(U256::from(collateral_decimals));
    let debt_decimals_factor = U256::from(10u64).pow(U256::from(debt_decimals));

    // expected_out = collateral_amount * collateral_price * debt_decimals / (debt_price * coll_decimals)
    // Multiply first!
    let numerator = collateral_amount
        .checked_mul(collateral_price)
        .unwrap_or(U256::MAX)
        .checked_mul(debt_decimals_factor)
        .unwrap_or(U256::MAX);

    let denominator = debt_price
        .checked_mul(coll_decimals_factor)
        .unwrap_or(U256::from(1u64));

    let expected_out = numerator / denominator;

    // Apply slippage: min_out = expected * (10000 - slippage) / 10000
    expected_out
        .checked_mul(U256::from(10_000u64 - max_slippage_bps))
        .unwrap_or(U256::ZERO)
        / U256::from(10_000u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_min_amount_out_simple() {
        // 1 WETH ($2000, 18 dec) → USDC ($1, 6 dec), 0.5% slippage
        let result = calculate_min_amount_out(
            U256::from(1_000_000_000_000_000_000u64), // 1 WETH
            U256::from(200_000_000_000u64),            // $2000 (8 dec)
            U256::from(100_000_000u64),                // $1 (8 dec)
            18,
            6,
            50, // 0.5%
        );
        // Expected: 2000 USDC = 2_000_000_000 (6 dec)
        // After 0.5% slippage: 2000 * 0.995 = 1990 USDC = 1_990_000_000
        assert_eq!(result, U256::from(1_990_000_000u64));
    }

    #[test]
    fn test_encode_calldata() {
        let calldata = encode_initiate_flash_loan(
            Address::ZERO,
            U256::from(1000u64),
            Address::ZERO,
            Address::ZERO,
            3000,
            U256::ZERO,
        );
        // Should produce valid ABI-encoded calldata (4 byte selector + params)
        assert!(calldata.len() >= 4 + 6 * 32); // selector + 6 params
    }
}
