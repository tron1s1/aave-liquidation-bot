// =============================================================================
// src/sequencer.rs — Arbitrum Sequencer Feed client
// =============================================================================
// Connects to wss://arb1-feed.arbitrum.io/feed (or local Nitro relay) to receive
// real-time transactions as they enter the sequencer. We watch for:
// - Chainlink price oracle updates (triggers HF recalculation)
// - Aave Pool events (large borrows, withdrawals that shift utilization)
//
// IMPORTANT: In production, run a LOCAL Nitro relay for lower latency.
// The public endpoint may rate-limit or lag under heavy load.

use eyre::Result;
use futures_util::StreamExt;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::config::AppConfig;
use crate::metrics::METRICS;

/// Events detected from the sequencer feed
#[derive(Debug, Clone)]
pub enum SequencerEvent {
    /// A Chainlink price update was detected
    PriceUpdate {
        /// The asset whose price changed (if we can identify it)
        asset_hint: Option<alloy::primitives::Address>,
    },
    /// A new block/batch was received (trigger general refresh)
    NewBatch {
        sequence_number: u64,
    },
}

/// Spawn the sequencer feed listener.
/// Returns a channel receiver that emits SequencerEvents.
pub async fn spawn_sequencer_listener(
    config: Arc<AppConfig>,
) -> Result<mpsc::Receiver<SequencerEvent>> {
    let (tx, rx) = mpsc::channel::<SequencerEvent>(1000);
    let feed_url = config.network.sequencer_feed_url.clone();

    let handle = tokio::spawn(async move {
        loop {
            match connect_and_listen(&feed_url, &tx).await {
                Ok(()) => {
                    info!("Sequencer feed connection closed, reconnecting...");
                }
                Err(e) => {
                    error!("Sequencer feed error: {:#}, reconnecting in 5s...", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        }
    });

    // Don't drop the handle — let it run forever
    std::mem::forget(handle);

    Ok(rx)
}

/// Connect to the sequencer feed and process messages
async fn connect_and_listen(
    url: &str,
    tx: &mpsc::Sender<SequencerEvent>,
) -> Result<()> {
    info!(url, "Connecting to Arbitrum sequencer feed...");

    let (ws_stream, _response) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| eyre::eyre!("WebSocket connection failed: {}", e))?;

    info!("Connected to sequencer feed");

    let (_write, mut read) = ws_stream.split();
    let mut sequence = 0u64;

    while let Some(msg) = read.next().await {
        match msg {
            Ok(tungstenite_msg) => {
                if tungstenite_msg.is_text() || tungstenite_msg.is_binary() {
                    METRICS.sequencer_messages.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    sequence += 1;

                    // Parse the message to detect relevant events
                    // For now, treat every message as a potential price update trigger
                    if let Err(e) = tx.try_send(SequencerEvent::NewBatch {
                        sequence_number: sequence,
                    }) {
                        debug!("Sequencer channel full, dropping event: {}", e);
                    }
                }
            }
            Err(e) => {
                warn!("WebSocket message error: {}", e);
                return Err(eyre::eyre!("WebSocket error: {}", e));
            }
        }
    }

    Ok(())
}

/// Alternative: poll-based price monitoring using RPC (fallback if sequencer unavailable)
pub async fn spawn_rpc_price_poller(
    config: Arc<AppConfig>,
) -> Result<mpsc::Receiver<SequencerEvent>> {
    let (tx, rx) = mpsc::channel::<SequencerEvent>(100);
    let poll_interval = std::time::Duration::from_millis(config.network.block_poll_interval_ms);

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(poll_interval);
        let mut tick_count = 0u64;

        loop {
            interval.tick().await;
            tick_count += 1;

            // Emit a NewBatch event on every poll tick
            // The main loop will re-check prices and HFs
            if let Err(e) = tx.try_send(SequencerEvent::NewBatch {
                sequence_number: tick_count,
            }) {
                debug!("RPC poller channel full: {}", e);
            }
        }
    });

    Ok(rx)
}
