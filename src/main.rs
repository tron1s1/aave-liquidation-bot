// =============================================================================
// src/main.rs — Entry point and orchestration
// =============================================================================
// Wires together: TheGraph → Watchlist → Sequencer → Detection → Executor
// Includes PendingCache to prevent duplicate liquidation attempts.

use rustls;

mod config;
mod contracts;
mod executor;
mod graph_client;
mod health_calculator;
mod metrics;
mod oracle_cache;
mod pending_cache;
mod profit_calculator;
mod sequencer;
mod simulator;
mod types;
mod watchlist;

use crate::config::AppConfig;
use crate::metrics::METRICS;
use crate::pending_cache::PendingCache;
use crate::types::{LiquidationOpportunity, SimulationResult, WAD};
use alloy::primitives::U256;
use eyre::Result;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// Pending cache TTL: how long a user is "locked" after a liquidation attempt.
/// - Paper mode: prevents log spam (same user won't be logged again for this duration)
/// - Live mode: prevents duplicate TXs while previous one is in the mempool
const PENDING_CACHE_TTL_SECS: u64 = 60;

#[tokio::main]
async fn main() -> Result<()> {
    // ── Initialize error handling ────────────────────────────────────────
    color_eyre::install()?;

    // Install TLS crypto provider (required by rustls when multiple features are enabled)
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // ── Load configuration ──────────────────────────────────────────────
    let config_path = std::env::var("CONFIG_PATH")
        .unwrap_or_else(|_| "config.toml".to_string());
    let config = AppConfig::load(Path::new(&config_path))?;

    // ── Initialize logging (AFTER config so we know log_level) ──────────
    metrics::init_logging(&config.bot.log_level)?;

    // ── Startup banner ──────────────────────────────────────────────────
    info!("╔══════════════════════════════════════════════════════════════╗");
    info!("║   Aave V3 Liquidation Bot — Arbitrum Mainnet               ║");
    info!("║   Strategy 1: Flash-Loan Liquidations                      ║");
    info!("╚══════════════════════════════════════════════════════════════╝");
    info!(
        paper_mode = config.bot.paper_mode,
        chain_id = config.network.chain_id,
        min_profit_bps = config.strategy.min_profit_bps,
        max_flash_loan_usd = config.strategy.max_flash_loan_usd,
        "Bot starting up"
    );

    if config.bot.paper_mode {
        warn!("🔸 PAPER MODE ACTIVE — No real transactions will be sent");
    } else {
        info!("🟢 LIVE MODE — Transactions WILL be submitted via private relay");
    }

    // ── Validate executor contract is deployed (not needed in paper mode) ──
    match config.get_executor_address() {
        Some(addr) => info!(?addr, "LiquidationExecutor contract configured"),
        None => {
            if config.bot.paper_mode {
                warn!("No LiquidationExecutor deployed yet — OK for paper mode");
            } else {
                error!("LiquidationExecutor address not set! Cannot run in live mode.");
                eyre::bail!("Set contracts.liquidation_executor in config.toml before live mode");
            }
        }
    }

    // ── Validate private key availability (not needed in paper mode) ─────
    if !config.bot.paper_mode {
        let _key = config.get_private_key()?;
        info!("Private key loaded from environment (not logged)");
    }

    let config_arc = Arc::new(config);

    // ── Create pending liquidation cache ─────────────────────────────────
    // Prevents duplicate TXs: once a user is claimed, they're locked for TTL.
    let pending_cache = Arc::new(PendingCache::new(
        Duration::from_secs(PENDING_CACHE_TTL_SECS),
    ));
    info!(
        ttl_secs = PENDING_CACHE_TTL_SECS,
        "Pending liquidation cache initialized"
    );

    // ── Start TheGraph cold-start poller ─────────────────────────────────
    let watchlist = crate::watchlist::create_watchlist();
    let _graph_poller = crate::graph_client::spawn_graph_poller(
        Arc::clone(&config_arc),
        Arc::clone(&watchlist)
    ).await?;

    // ── Create Oracle Cache ──────────────────────────────────────────────
    // Cache oracle prices for 60 seconds to avoid blocking the hot path
    // with RPC calls for every single liquidation simulation.
    let oracle_cache = Arc::new(crate::oracle_cache::OracleCache::new(60));

    // ── Start Sequencer Feed listener ────────────────────────────────────
    let mut sequencer_events = crate::sequencer::spawn_sequencer_listener(Arc::clone(&config_arc)).await?;

    // ── Metrics + cache cleanup loop ────────────────────────────────────
    let cache_for_cleanup = Arc::clone(&pending_cache);
    let _metrics_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            METRICS.log_summary();
            // Periodic cleanup of expired pending cache entries
            cache_for_cleanup.cleanup_expired();
        }
    });

    info!("Bot initialization complete. Listening for sequencer events...");

    // ── Scan interval: check for underwater users every N batches ────────
    // Don't scan on every single batch (would be wasteful with 4 batches/sec).
    // Instead scan every 20 batches (~5 seconds) or on price updates.
    let scan_interval = 20u64;
    let mut batch_counter = 0u64;
    let min_profit_bps = config_arc.strategy.min_profit_bps;

    // HF < 1.0 in WAD (the graph stores decimal, we converted to scaled WAD in parse_decimal)
    // parse_decimal("0.99") → 0.99 * 1e18 = 990000000000000000
    // So WAD (1e18) = HF 1.0 threshold
    let underwater_threshold = WAD;

    // Keep the bot running until interrupted
    let mut shutdown_signal = std::pin::pin!(tokio::signal::ctrl_c());

    loop {
        tokio::select! {
            _ = &mut shutdown_signal => {
                info!("Received Ctrl+C, shutting down gracefully...");
                break;
            }
            Some(event) = sequencer_events.recv() => {
                match event {
                    crate::sequencer::SequencerEvent::PriceUpdate { asset_hint } => {
                        debug!(?asset_hint, "Received price update from sequencer");
                        // Price update — scan immediately
                        scan_for_liquidations(
                            &watchlist,
                            &config_arc,
                            &pending_cache,
                            &oracle_cache,
                            underwater_threshold,
                            min_profit_bps,
                        ).await;
                    }
                    crate::sequencer::SequencerEvent::NewBatch { sequence_number } => {
                        batch_counter += 1;
                        METRICS.sequencer_messages.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                        if batch_counter % scan_interval == 0 {
                            debug!(sequence_number, batch_counter, "Periodic scan triggered");
                            scan_for_liquidations(
                                &watchlist,
                                &config_arc,
                                &pending_cache,
                                &oracle_cache,
                                underwater_threshold,
                                min_profit_bps,
                            ).await;
                        }
                    }
                }
            }
        }
    }

    METRICS.log_summary();
    info!(
        pending_cache_size = pending_cache.len(),
        "Bot shutdown complete."
    );
    Ok(())
}

/// Scan the watchlist for underwater users and process any liquidation opportunities.
/// Uses PendingCache to deduplicate: each user is processed at most once per TTL window.
async fn scan_for_liquidations(
    watchlist: &Arc<crate::watchlist::Watchlist>,
    config: &Arc<AppConfig>,
    pending_cache: &Arc<PendingCache>,
    oracle_cache: &Arc<crate::oracle_cache::OracleCache>,
    hf_threshold: U256,
    min_profit_bps: u64,
) {
    let underwater = watchlist.get_underwater_users(hf_threshold);

    if underwater.is_empty() {
        return;
    }

    // Count how many are new (not already in pending cache)
    let mut new_count = 0u32;

    for user_pos in &underwater {
        // ── Pending cache check — skip if already in-flight ──────────────
        if !pending_cache.try_claim(&user_pos.user) {
            // Already pending — skip silently (don't even count as detected)
            continue;
        }

        // This is a genuinely new opportunity for this scan cycle
        new_count += 1;
        METRICS.record_detected();

        // Build a LiquidationOpportunity from the position data.
        // In the current subgraph model, each position = 1 reserve with collateral + debt.
        // The health factor comes from the subgraph directly.
        let reserve = match user_pos.reserves.first() {
            Some(r) => r,
            None => {
                pending_cache.release(&user_pos.user);
                continue;
            }
        };

        // Only process if there's actual debt
        if reserve.variable_debt.is_zero() && reserve.stable_debt.is_zero() {
            METRICS.record_skipped();
            pending_cache.release(&user_pos.user);
            continue;
        }

        // Estimate profit from liquidation bonus (Aave V3 typically 5-10% bonus)
        // Since we don't have full reserve info from the subgraph, use conservative estimate
        let estimated_bonus_bps: u64 = 500; // 5% assumed liquidation bonus
        let debt_value = reserve.variable_debt
            .checked_add(reserve.stable_debt)
            .unwrap_or(U256::ZERO);

        let estimated_profit = debt_value
            .checked_mul(U256::from(estimated_bonus_bps))
            .unwrap_or(U256::ZERO)
            / U256::from(10_000u64);

        // Check minimum profit threshold
        if estimated_bonus_bps < min_profit_bps {
            debug!(
                user = ?user_pos.user,
                bonus_bps = estimated_bonus_bps,
                min_bps = min_profit_bps,
                "Skipping — below minimum profit threshold"
            );
            METRICS.record_skipped();
            pending_cache.release(&user_pos.user);
            continue;
        }

        METRICS.record_simulated();

        let opportunity = LiquidationOpportunity {
            user: user_pos.user,
            collateral_asset: reserve.asset,
            debt_asset: reserve.asset, // same reserve in this subgraph model
            debt_to_cover: reserve.variable_debt,
            expected_collateral_amount: reserve.collateral_balance,
            liquidation_bonus_bps: estimated_bonus_bps as u16,
            estimated_profit_usd: estimated_profit,
            estimated_profit_bps: estimated_bonus_bps,
            user_health_factor: user_pos.health_factor,
            full_liquidation: user_pos.health_factor < U256::from(950_000_000_000_000_000u64),
            detected_at_ms: chrono::Utc::now().timestamp_millis() as u64,
        };

        // Create a synthetic simulation result (paper mode doesn't need real simulation)
        let simulation = SimulationResult {
            success: true,
            gas_used: 500_000, // estimated
            revert_reason: None,
            actual_profit: Some(estimated_profit),
        };

        // Execute (paper mode logs, live mode submits TX)
        match crate::executor::execute_liquidation(config, &opportunity, &simulation, oracle_cache).await {
            Ok(result) => {
                crate::executor::log_execution_result(&result);
                // In live mode, if TX failed (no hash), release from cache immediately
                // so the user can be retried on next scan
                if !result.paper_mode && result.tx_hash.is_none() {
                    pending_cache.release(&user_pos.user);
                }
                // Otherwise, keep in cache for TTL duration:
                // - Paper mode: prevents re-logging the same user
                // - Live mode with hash: prevents double-submission while TX confirms
            }
            Err(e) => {
                error!(user = ?user_pos.user, error = %e, "Execution failed");
                METRICS.record_failed();
                // Release on error so user can be retried
                pending_cache.release(&user_pos.user);
            }
        }
    }

    if new_count > 0 {
        info!(
            total_underwater = underwater.len(),
            new_opportunities = new_count,
            pending_cache_size = pending_cache.len(),
            "🔍 Scan complete"
        );
    }
}
