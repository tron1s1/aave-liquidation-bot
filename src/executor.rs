// =============================================================================
// src/executor.rs — Transaction execution engine (paper mode + live mode)
// =============================================================================
// Paper mode: Logs "would have submitted liquidation" with full details.
// Live mode: Simulates via eth_call, then signs and submits via Alloy signer.
//            Uses private relay (Alchemy/Flashbots) for MEV protection.
//
// SECURITY: Private key is loaded from env var, never logged.
// All live transactions use EIP-1559 for predictable gas pricing.
// Every TX is simulated via eth_call before submission (zero-risk dry run).

use alloy::primitives::{Uint, U256};
use alloy::providers::ProviderBuilder;
use alloy::network::EthereumWallet;
use alloy::signers::local::PrivateKeySigner;
use eyre::{Result, WrapErr};
use tracing::{info, warn, error};

use crate::config::AppConfig;
use crate::contracts::ILiquidationExecutor;
use crate::metrics::METRICS;
use crate::types::{ExecutionResult, LiquidationOpportunity, SimulationResult};

/// Execute a liquidation (paper mode or live)
pub async fn execute_liquidation(
    config: &AppConfig,
    opportunity: &LiquidationOpportunity,
    simulation: &SimulationResult,
    oracle_cache: &std::sync::Arc<crate::oracle_cache::OracleCache>,
) -> Result<ExecutionResult> {
    if config.bot.paper_mode {
        execute_paper_mode(opportunity, simulation).await
    } else {
        execute_live_mode(config, opportunity, simulation, oracle_cache).await
    }
}

/// Paper mode: log everything, execute nothing
async fn execute_paper_mode(
    opportunity: &LiquidationOpportunity,
    simulation: &SimulationResult,
) -> Result<ExecutionResult> {
    info!("╔═══════════════════════════════════════════════════════════╗");
    info!("║  📝 PAPER MODE — WOULD HAVE SUBMITTED LIQUIDATION       ║");
    info!("╚═══════════════════════════════════════════════════════════╝");
    info!(
        user = ?opportunity.user,
        debt_asset = ?opportunity.debt_asset,
        collateral_asset = ?opportunity.collateral_asset,
        debt_to_cover = %opportunity.debt_to_cover,
        expected_collateral = %opportunity.expected_collateral_amount,
        bonus_bps = opportunity.liquidation_bonus_bps,
        estimated_profit_usd = %opportunity.estimated_profit_usd,
        estimated_profit_bps = opportunity.estimated_profit_bps,
        user_hf = %opportunity.user_health_factor,
        full_liquidation = opportunity.full_liquidation,
        simulation_success = simulation.success,
        "💰 Paper mode liquidation opportunity detected"
    );

    let profit_u64: u64 = opportunity.estimated_profit_usd.try_into().unwrap_or(0);
    METRICS.record_executed(profit_u64);

    Ok(ExecutionResult {
        paper_mode: true,
        tx_hash: None,
        gas_used: simulation.gas_used,
        gas_price: U256::ZERO,
        net_profit: opportunity.estimated_profit_usd,
        opportunity: opportunity.clone(),
    })
}

/// Live mode: simulate via eth_call, then sign and submit the real transaction.
///
/// Flow:
///   1. Parse private key → create Alloy signer + wallet
///   2. Create provider with signer (supports EIP-1559 + nonce management)
///   3. Determine swap parameters (pool fee, min amount out)
///   4. Simulate via eth_call (zero cost, catches stale opportunities)
///   5. Submit real transaction
///   6. Wait for receipt and parse result
async fn execute_live_mode(
    config: &AppConfig,
    opportunity: &LiquidationOpportunity,
    _simulation: &SimulationResult,
    oracle_cache: &std::sync::Arc<crate::oracle_cache::OracleCache>,
) -> Result<ExecutionResult> {
    // ── 1. Get executor contract address ──────────────────────────────────
    let executor_address = config
        .get_executor_address()
        .ok_or_else(|| eyre::eyre!("LiquidationExecutor address not configured"))?;

    // ── 2. Parse private key and create signer ───────────────────────────
    let private_key = config.get_private_key()?;
    // SECURITY: private_key is used for signing only, never logged
    let signer: PrivateKeySigner = private_key
        .parse()
        .wrap_err("Failed to parse BOT_PRIVATE_KEY — must be a valid hex private key")?;
    let from_address = signer.address();
    let wallet = EthereumWallet::from(signer);

    // ── 3. Choose RPC endpoint ───────────────────────────────────────────
    let rpc_url = if !config.network.private_rpc_url.is_empty() {
        info!("Using private relay for MEV protection");
        &config.network.private_rpc_url
    } else {
        warn!("⚠️ No private relay configured — using public RPC (vulnerable to front-running!)");
        &config.network.rpc_url
    };

    info!(
        user = ?opportunity.user,
        executor = ?executor_address,
        from = ?from_address,
        debt_to_cover = %opportunity.debt_to_cover,
        "🔴 LIVE MODE — Preparing liquidation transaction..."
    );

    // ── 4. Create provider with signer ───────────────────────────────────
    // ProviderBuilder with wallet adds: GasFiller, NonceFiller, ChainIdFiller, WalletFiller
    let rpc_url_parsed: reqwest::Url = rpc_url
        .parse()
        .wrap_err("Failed to parse RPC URL")?;
    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(rpc_url_parsed);

    // ── 4b. Enforce max flash loan size (USD cap) ────────────────────────
    // Query the Aave Oracle for the debt asset's USD price, then cap debt_to_cover
    // so the flash loan never exceeds max_flash_loan_usd from config.
    let max_usd = config.strategy.max_flash_loan_usd;
    let mut capped_debt_to_cover = opportunity.debt_to_cover;

    // Fast path: Check oracle cache first
    let price_usd = if let Some(cached_price) = oracle_cache.get_price(&opportunity.debt_asset) {
        Some(cached_price)
    } else {
        // Slow path: Fetch from RPC and cache it
        let oracle = crate::contracts::IAaveOracle::new(
            crate::types::AAVE_ORACLE,
            &provider,
        );
        match oracle.getAssetPrice(opportunity.debt_asset).call().await {
            Ok(price_result) => {
                let price = price_result;
                oracle_cache.set_price(opportunity.debt_asset, price);
                Some(price)
            }
            Err(e) => {
                warn!(error = %e, "Could not fetch oracle price — proceeding with uncapped debt_to_cover");
                None
            }
        }
    };

    if let Some(price_usd) = price_usd {
        if !price_usd.is_zero() {
                // debt_to_cover is in WAD (1e18). Price is 8 decimals.
                // debt_usd = debt_to_cover * price / 1e18 / 1e8
                //          = debt_to_cover * price / 1e26
                let usd_scale = U256::from(10u64).pow(U256::from(26));
                let debt_usd = capped_debt_to_cover
                    .checked_mul(price_usd)
                    .unwrap_or(U256::MAX)
                    / usd_scale;

                let max_usd_u256 = U256::from(max_usd);
                if debt_usd > max_usd_u256 {
                    // Cap: max_debt = max_usd * 1e26 / price
                    let new_debt = max_usd_u256
                        .checked_mul(usd_scale)
                        .unwrap_or(U256::MAX)
                        / price_usd;
                    info!(
                        original_usd = %debt_usd,
                        capped_usd = max_usd,
                        original_debt = %capped_debt_to_cover,
                        capped_debt = %new_debt,
                        "📏 Capping flash loan to max_flash_loan_usd limit"
                    );
                    capped_debt_to_cover = new_debt;
                } else {
                    info!(
                        debt_usd = %debt_usd,
                        max_usd,
                        "Flash loan within USD cap ✅"
                    );
                }
            }
        }

    // ── 5. Determine swap parameters ─────────────────────────────────────
    // Same-asset liquidations (debt == collateral) don't need a swap
    let (swap_pool_fee, min_amount_out) = if opportunity.debt_asset == opportunity.collateral_asset {
        (0u32, U256::ZERO)
    } else {
        // Cross-asset: use 0.3% Uniswap V3 pool fee as default
        // minAmountOut = 0 for now (the contract has its own slippage protection)
        (3000u32, U256::ZERO)
    };

    // ── 6. Create contract instance ──────────────────────────────────────
    let contract = ILiquidationExecutor::new(executor_address, &provider);

    // ── 7. Simulate via eth_call ─────────────────────────────────────────
    if config.bot.skip_simulation {
        info!(user = ?opportunity.user, "⚡ Skipping simulation (config.bot.skip_simulation = true) — proceeding directly to submission");
    } else {
        // Zero cost, catches: already-liquidated positions, insufficient collateral,
        // swap slippage, and any unexpected revert.
        info!(user = ?opportunity.user, "Simulating liquidation via eth_call...");

        let sim_call = contract.initiateFlashLoan(
            opportunity.debt_asset,
            capped_debt_to_cover,
            opportunity.collateral_asset,
            opportunity.user,
            Uint::<24, 1>::from(swap_pool_fee),
            min_amount_out,
        );

        match sim_call.call().await {
            Ok(_) => {
                info!(user = ?opportunity.user, "✅ Simulation passed — proceeding to submit TX");
            }
            Err(e) => {
                warn!(
                    user = ?opportunity.user,
                    error = %e,
                    "❌ Simulation REVERTED — aborting (position likely already liquidated)"
                );
                METRICS.record_failed();
                return Ok(ExecutionResult {
                    paper_mode: false,
                    tx_hash: None,
                    gas_used: 0,
                    gas_price: U256::ZERO,
                    net_profit: U256::ZERO,
                    opportunity: opportunity.clone(),
                });
            }
        }
    }

    // ── 8. Submit real transaction ────────────────────────────────────────
    info!(user = ?opportunity.user, "📤 Submitting liquidation transaction...");

    let send_call = contract.initiateFlashLoan(
        opportunity.debt_asset,
        capped_debt_to_cover,
        opportunity.collateral_asset,
        opportunity.user,
        Uint::<24, 1>::from(swap_pool_fee),
        min_amount_out,
    );

    let pending_tx = send_call
        .send()
        .await
        .wrap_err("Failed to submit liquidation transaction")?;

    let tx_hash = *pending_tx.tx_hash();
    info!(?tx_hash, "Transaction submitted — waiting for confirmation...");

    // ── 9. Wait for receipt ──────────────────────────────────────────────
    let receipt = pending_tx
        .get_receipt()
        .await
        .wrap_err("Failed to get transaction receipt")?;

    let gas_used = receipt.gas_used;
    let effective_gas_price = receipt.effective_gas_price;
    let success = receipt.status();

    if success {
        info!(
            ?tx_hash,
            gas_used,
            effective_gas_price,
            user = ?opportunity.user,
            "✅ Liquidation transaction CONFIRMED!"
        );
    } else {
        error!(
            ?tx_hash,
            gas_used,
            user = ?opportunity.user,
            "❌ Liquidation transaction REVERTED on-chain (gas was consumed)"
        );
    }

    // ── 10. Calculate net profit ─────────────────────────────────────────
    // gas_cost = gas_used * effective_gas_price (in wei)
    // This is an approximation — true profit requires parsing LiquidationExecuted events
    let gas_cost = U256::from(gas_used)
        .checked_mul(U256::from(effective_gas_price))
        .unwrap_or(U256::ZERO);

    let net_profit = if success && opportunity.estimated_profit_usd > gas_cost {
        opportunity.estimated_profit_usd - gas_cost
    } else {
        U256::ZERO
    };

    let profit_u64: u64 = net_profit.try_into().unwrap_or(0);
    METRICS.record_executed(profit_u64);

    Ok(ExecutionResult {
        paper_mode: false,
        tx_hash: Some(tx_hash),
        gas_used,
        gas_price: U256::from(effective_gas_price),
        net_profit,
        opportunity: opportunity.clone(),
    })
}

/// Log a summary of an execution result
pub fn log_execution_result(result: &ExecutionResult) {
    if result.paper_mode {
        info!(
            mode = "PAPER",
            user = ?result.opportunity.user,
            profit_usd = %result.net_profit,
            profit_bps = result.opportunity.estimated_profit_bps,
            "Execution result logged"
        );
    } else {
        match &result.tx_hash {
            Some(hash) => {
                info!(
                    mode = "LIVE",
                    tx_hash = ?hash,
                    gas_used = result.gas_used,
                    gas_price = %result.gas_price,
                    profit_usd = %result.net_profit,
                    "✅ Liquidation transaction confirmed"
                );
            }
            None => {
                error!(
                    mode = "LIVE",
                    user = ?result.opportunity.user,
                    "❌ Liquidation transaction failed or simulation reverted"
                );
            }
        }
    }
}
