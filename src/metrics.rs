// =============================================================================
// src/metrics.rs — Structured logging, tracing, and operational metrics
// =============================================================================
// Uses tracing-subscriber with JSON output for production and pretty output for dev.
// All sensitive values (private keys, raw TX data) are EXCLUDED from logs.

use eyre::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing_subscriber::{fmt, EnvFilter, prelude::*};

/// Global counters for operational metrics (lock-free atomics)
pub struct BotMetrics {
    /// Total opportunities detected
    pub opportunities_detected: AtomicU64,
    /// Total opportunities that passed simulation
    pub opportunities_simulated: AtomicU64,
    /// Total opportunities executed (paper or live)
    pub opportunities_executed: AtomicU64,
    /// Total opportunities skipped (below profit threshold)
    pub opportunities_skipped: AtomicU64,
    /// Total opportunities that failed simulation
    pub opportunities_failed: AtomicU64,
    /// Cumulative profit in USD (8 decimals, integer)
    pub cumulative_profit_usd: AtomicU64,
    /// Watchlist size (number of tracked users)
    pub watchlist_size: AtomicU64,
    /// Last TheGraph sync timestamp
    pub last_graph_sync: AtomicU64,
    /// Total sequencer messages processed
    pub sequencer_messages: AtomicU64,
}

impl BotMetrics {
    pub const fn new() -> Self {
        Self {
            opportunities_detected: AtomicU64::new(0),
            opportunities_simulated: AtomicU64::new(0),
            opportunities_executed: AtomicU64::new(0),
            opportunities_skipped: AtomicU64::new(0),
            opportunities_failed: AtomicU64::new(0),
            cumulative_profit_usd: AtomicU64::new(0),
            watchlist_size: AtomicU64::new(0),
            last_graph_sync: AtomicU64::new(0),
            sequencer_messages: AtomicU64::new(0),
        }
    }

    pub fn record_detected(&self) {
        self.opportunities_detected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_simulated(&self) {
        self.opportunities_simulated.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_executed(&self, profit_usd: u64) {
        self.opportunities_executed.fetch_add(1, Ordering::Relaxed);
        self.cumulative_profit_usd.fetch_add(profit_usd, Ordering::Relaxed);
    }

    pub fn record_skipped(&self) {
        self.opportunities_skipped.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_failed(&self) {
        self.opportunities_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_watchlist_size(&self, size: u64) {
        self.watchlist_size.store(size, Ordering::Relaxed);
    }

    /// Log a summary of current metrics
    pub fn log_summary(&self) {
        tracing::info!(
            detected = self.opportunities_detected.load(Ordering::Relaxed),
            simulated = self.opportunities_simulated.load(Ordering::Relaxed),
            executed = self.opportunities_executed.load(Ordering::Relaxed),
            skipped = self.opportunities_skipped.load(Ordering::Relaxed),
            failed = self.opportunities_failed.load(Ordering::Relaxed),
            cumulative_profit_usd = self.cumulative_profit_usd.load(Ordering::Relaxed),
            watchlist = self.watchlist_size.load(Ordering::Relaxed),
            sequencer_msgs = self.sequencer_messages.load(Ordering::Relaxed),
            "📊 Bot metrics summary"
        );
    }
}

/// Global static metrics instance (lock-free, zero allocation after init)
pub static METRICS: BotMetrics = BotMetrics::new();

/// Initialize the tracing/logging subsystem.
/// - Production (JSON): `LOG_FORMAT=json`
/// - Development (pretty): default
pub fn init_logging(log_level: &str) -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(log_level));

    let use_json = std::env::var("LOG_FORMAT")
        .map(|v| v.to_lowercase() == "json")
        .unwrap_or(false);

    if use_json {
        // Production: structured JSON logs
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt::layer().json().with_target(true).with_thread_ids(true))
            .init();
    } else {
        // Development: pretty colored logs
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt::layer().pretty().with_target(true))
            .init();
    }

    tracing::info!("Logging initialized (level: {})", log_level);
    Ok(())
}
