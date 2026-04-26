// =============================================================================
// src/graph_client.rs — TheGraph cold-start client for Aave V3 Arbitrum
// =============================================================================
// Uses the aave-liquidation-risk-arbitrum subgraph (schema: Position type).
// Queries positions with HF < 1.1 and populates the watchlist.
//
// Verified schema (introspected 2026-04-26):
//   Position { id, user { id }, reserve { underlyingAsset, symbol, decimals },
//              collateral, debt, healthFactor, liquidationThreshold }

use alloy::primitives::{Address, U256};
use eyre::{Result, WrapErr};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::config::AppConfig;
use crate::metrics::METRICS;
use crate::types::{UserPosition, UserReservePosition};
use crate::watchlist::Watchlist;

// ── GraphQL response structures ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GraphResponse {
    data: Option<GraphData>,
    errors: Option<Vec<GraphError>>,
}

#[derive(Debug, Deserialize)]
struct GraphData {
    positions: Vec<GraphPosition>,
}

#[derive(Debug, Deserialize)]
struct GraphError {
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphPosition {
    id: String,
    user: String,       // plain address string e.g. "0xabc..."
    reserve: String,    // plain address string e.g. "0xaf88..."
    collateral: String,
    debt: String,
    health_factor: String,
    liquidation_threshold: String,
}

// ── GraphClient ─────────────────────────────────────────────────────────────

pub struct GraphClient {
    http_client: reqwest::Client,
    subgraph_url: String,
    page_size: u32,
}

impl GraphClient {
    pub fn new(config: &AppConfig) -> Result<Self> {
        let api_key = std::env::var(&config.bot.thegraph_api_key_env)
            .unwrap_or_default();
        let url = config.thegraph.subgraph_url.replace("{api_key}", &api_key);

        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .wrap_err("Failed to build HTTP client")?;

        info!(url = %url.replace(&api_key, "***"), "TheGraph client configured");

        Ok(Self {
            http_client,
            subgraph_url: url,
            page_size: config.thegraph.page_size,
        })
    }

    /// Fetch all at-risk positions (HF < 1.1 WAD) from TheGraph, paginated.
    pub async fn fetch_at_risk_borrowers(&self) -> Result<Vec<UserPosition>> {
        let mut all_positions: Vec<UserPosition> = Vec::with_capacity(5_000);
        let mut last_id = String::new();
        let mut page = 0u32;

        // HF is stored as a plain decimal in this subgraph (e.g. "1.05", not WAD)
        let hf_threshold = "1.1";

        loop {
            let query = self.build_query(hf_threshold, &last_id);
            let response = self.execute_query(&query).await?;

            match response.data {
                Some(data) => {
                    let count = data.positions.len();
                    if count == 0 {
                        break;
                    }

                    if let Some(last) = data.positions.last() {
                        last_id = last.id.clone();
                    }

                    for pos in data.positions {
                        match Self::convert_position(pos) {
                            Ok(up) => all_positions.push(up),
                            Err(e) => debug!("Skipping position: {}", e),
                        }
                    }

                    page += 1;
                    debug!(page, fetched = count, total = all_positions.len(), "Graph page");

                    if count < self.page_size as usize {
                        break;
                    }
                }
                None => {
                    if let Some(errors) = response.errors {
                        let msgs: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
                        error!(?msgs, "TheGraph query errors");
                    }
                    break;
                }
            }
        }

        info!(total = all_positions.len(), pages = page, "TheGraph fetch complete");
        Ok(all_positions)
    }

    fn build_query(&self, hf_threshold: &str, last_id: &str) -> String {
        let id_filter = if last_id.is_empty() {
            String::new()
        } else {
            format!(r#", id_gt: "{}""#, last_id)
        };

        format!(
            r#"{{
  positions(
    first: {}
    orderBy: id
    orderDirection: asc
    where: {{ healthFactor_gt: "0", healthFactor_lt: "{}" {} }}
  ) {{
    id
    user {{ id }}
    reserve {{ underlyingAsset symbol decimals }}
    collateral
    debt
    healthFactor
    liquidationThreshold
  }}
}}"#,
            self.page_size, hf_threshold, id_filter
        )
    }

    async fn execute_query(&self, query: &str) -> Result<GraphResponse> {
        let body = serde_json::json!({ "query": query });

        let resp = self
            .http_client
            .post(&self.subgraph_url)
            .json(&body)
            .send()
            .await
            .wrap_err("Failed to send TheGraph request")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            eyre::bail!("TheGraph HTTP {}: {}", status, text);
        }

        resp.json::<GraphResponse>()
            .await
            .wrap_err("Failed to parse TheGraph response")
    }

    /// Convert a GraphPosition into a UserPosition for the watchlist.
    fn convert_position(pos: GraphPosition) -> Result<UserPosition> {
        let user: Address = pos.user.parse()
            .wrap_err("Invalid user address")?;
        let asset: Address = pos.reserve.parse()
            .wrap_err("Invalid reserve address")?;

        let collateral = parse_decimal(&pos.collateral)?;
        let debt = parse_decimal(&pos.debt)?;
        let health_factor = parse_decimal(&pos.health_factor)?;

        if debt.is_zero() {
            eyre::bail!("Position has zero debt — skip");
        }

        let reserve_position = UserReservePosition {
            asset,
            reserve_index: 0,
            collateral_balance: collateral,
            variable_debt: debt,
            stable_debt: U256::ZERO,
            is_collateral: !collateral.is_zero(),
            is_borrowing: !debt.is_zero(),
        };

        Ok(UserPosition {
            user,
            reserves: vec![reserve_position],
            total_collateral_base: U256::ZERO,
            total_debt_base: U256::ZERO,
            health_factor,
            config_bitmask: U256::ZERO,
            last_updated: chrono::Utc::now().timestamp() as u64,
        })
    }
}

fn parse_u256(s: &str) -> Result<U256> {
    if s.is_empty() || s == "0" {
        return Ok(U256::ZERO);
    }
    s.parse::<U256>()
        .wrap_err_with(|| format!("Failed to parse U256: {}", s))
}

/// Parse a decimal string (e.g. "44.83" or "1234567.89") into U256 scaled by 1e18
fn parse_decimal(s: &str) -> Result<U256> {
    if s.is_empty() || s == "0" {
        return Ok(U256::ZERO);
    }
    // Try integer first (fast path)
    if !s.contains('.') {
        return s.parse::<U256>()
            .wrap_err_with(|| format!("Failed to parse amount: {}", s));
    }
    // Parse as f64 and scale by 1e18 to get WAD
    let f: f64 = s.parse().wrap_err_with(|| format!("Failed to parse decimal: {}", s))?;
    if f < 0.0 {
        eyre::bail!("Negative amount: {}", s);
    }
    let scaled = (f * 1e18) as u128;
    Ok(U256::from(scaled))
}

// ── Background poller ───────────────────────────────────────────────────────

pub async fn spawn_graph_poller(
    config: Arc<AppConfig>,
    watchlist: Arc<Watchlist>,
) -> Result<tokio::task::JoinHandle<()>> {
    let client = GraphClient::new(&config)?;
    let poll_interval = std::time::Duration::from_secs(config.thegraph.poll_interval_secs);

    let handle = tokio::spawn(async move {
        // Initial fetch
        match client.fetch_at_risk_borrowers().await {
            Ok(positions) => {
                let count = positions.len();
                info!(count, "Initial TheGraph fetch complete");
                watchlist.bulk_update(positions);
                METRICS.last_graph_sync.store(
                    chrono::Utc::now().timestamp() as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
                METRICS.set_watchlist_size(count as u64);
            }
            Err(e) => error!("Initial TheGraph fetch failed: {:#}", e),
        }

        // Hourly refresh
        let mut interval = tokio::time::interval(poll_interval);
        interval.tick().await; // skip immediate tick

        loop {
            interval.tick().await;
            info!("Starting hourly TheGraph refresh...");
            match client.fetch_at_risk_borrowers().await {
                Ok(positions) => {
                    let count = positions.len();
                    watchlist.bulk_update(positions);
                    METRICS.last_graph_sync.store(
                        chrono::Utc::now().timestamp() as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    METRICS.set_watchlist_size(count as u64);
                    info!(count, "Hourly TheGraph refresh complete");
                }
                Err(e) => warn!("TheGraph refresh failed (will retry): {:#}", e),
            }
        }
    });

    Ok(handle)
}
