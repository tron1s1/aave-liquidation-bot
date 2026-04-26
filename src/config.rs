// =============================================================================
// src/config.rs — Configuration loading from TOML file + environment variables
// =============================================================================
// SECURITY: Private keys and API keys are NEVER stored in config files.
// They are loaded from environment variables at runtime only.
// The config file only contains non-sensitive operational parameters.

use alloy::primitives::Address;
use eyre::{Result, WrapErr};
use serde::Deserialize;
use std::path::Path;
use tracing::info;

/// Top-level configuration structure, deserialized from config.toml
#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub network: NetworkConfig,
    pub contracts: ContractsConfig,
    pub thegraph: TheGraphConfig,
    pub strategy: StrategyConfig,
    pub bot: BotConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkConfig {
    pub chain_id: u64,
    pub rpc_url: String,
    pub private_rpc_url: String,
    pub ws_rpc_url: String,
    pub sequencer_feed_url: String,
    pub sequencer_connections: u32,
    pub block_poll_interval_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContractsConfig {
    /// PoolAddressesProvider — root registry for Aave V3
    #[serde(deserialize_with = "deserialize_address")]
    pub pool_addresses_provider: Address,
    /// Uniswap V3 SwapRouter02
    #[serde(deserialize_with = "deserialize_address")]
    pub uniswap_v3_router: Address,
    /// Our deployed LiquidationExecutor contract
    pub liquidation_executor: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TheGraphConfig {
    pub subgraph_url: String,
    pub poll_interval_secs: u64,
    pub max_health_factor: f64,
    pub page_size: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StrategyConfig {
    pub min_profit_bps: u64,
    pub flash_loan_premium_bps: u64,
    pub max_slippage_bps: u64,
    pub max_gas_price_gwei: f64,
    pub min_debt_usd: u64,
    /// Maximum flash loan size in USD. Caps exposure per liquidation.
    /// Start low (1000 = $1k) for testing, ramp up (10000 = $10k) once validated.
    pub max_flash_loan_usd: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BotConfig {
    pub paper_mode: bool,
    /// Whether to skip the eth_call simulation and send transactions directly.
    /// Saves 100-200ms latency but wastes gas on reverted liquidations.
    #[serde(default)]
    pub skip_simulation: bool,
    /// Name of the env var holding the bot private key (NOT the key itself)
    pub private_key_env: String,
    /// Name of the env var holding TheGraph API key (NOT the key itself)
    pub thegraph_api_key_env: String,
    pub log_level: String,
}

/// Custom deserializer for Address from hex string in TOML
fn deserialize_address<'de, D>(deserializer: D) -> Result<Address, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: String = String::deserialize(deserializer)?;
    s.parse::<Address>().map_err(serde::de::Error::custom)
}

impl AppConfig {
    /// Load configuration from a TOML file path.
    /// Sensitive values are NOT in this file — they come from env vars.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("Failed to read config file: {}", path.display()))?;
        
        let config: AppConfig = toml::from_str(&content)
            .wrap_err("Failed to parse config.toml")?;

        // Validate critical fields
        config.validate()?;

        info!(
            chain_id = config.network.chain_id,
            paper_mode = config.bot.paper_mode,
            min_profit_bps = config.strategy.min_profit_bps,
            "Configuration loaded successfully"
        );

        Ok(config)
    }

    /// Validate configuration values are sane
    fn validate(&self) -> Result<()> {
        eyre::ensure!(
            self.network.chain_id == 42161,
            "This bot only supports Arbitrum One (chain_id 42161), got {}",
            self.network.chain_id
        );
        eyre::ensure!(
            self.strategy.min_profit_bps >= 100,
            "min_profit_bps must be >= 100 (1%), got {}",
            self.strategy.min_profit_bps
        );
        eyre::ensure!(
            self.strategy.flash_loan_premium_bps <= 100,
            "flash_loan_premium_bps seems too high: {}",
            self.strategy.flash_loan_premium_bps
        );
        eyre::ensure!(
            self.strategy.max_slippage_bps <= 500,
            "max_slippage_bps should be <= 500 (5%), got {}",
            self.strategy.max_slippage_bps
        );
        eyre::ensure!(
            self.strategy.max_flash_loan_usd >= 100,
            "max_flash_loan_usd must be >= 100 ($100 minimum), got {}",
            self.strategy.max_flash_loan_usd
        );
        eyre::ensure!(
            !self.bot.private_key_env.is_empty(),
            "private_key_env must be set (name of env var, not the key!)"
        );
        Ok(())
    }

    /// Retrieve the bot's private key from the environment variable.
    /// SECURITY: This value is never logged, stored in config files, or printed.
    pub fn get_private_key(&self) -> Result<String> {
        std::env::var(&self.bot.private_key_env).wrap_err_with(|| {
            format!(
                "Environment variable '{}' not set. Set it with your bot wallet private key.",
                self.bot.private_key_env
            )
        })
    }

    /// Retrieve TheGraph API key from the environment variable.
    pub fn get_thegraph_api_key(&self) -> Result<String> {
        std::env::var(&self.bot.thegraph_api_key_env).wrap_err_with(|| {
            format!(
                "Environment variable '{}' not set. Set it with your TheGraph API key.",
                self.bot.thegraph_api_key_env
            )
        })
    }

    /// Get the LiquidationExecutor contract address (returns None if not deployed yet)
    pub fn get_executor_address(&self) -> Option<Address> {
        if self.contracts.liquidation_executor.is_empty() {
            None
        } else {
            self.contracts.liquidation_executor.parse::<Address>().ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_test_config() -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        write!(
            file,
            r#"
[network]
chain_id = 42161
rpc_url = "https://arb-mainnet.g.alchemy.com/v2/test"
private_rpc_url = ""
ws_rpc_url = "wss://arb-mainnet.g.alchemy.com/v2/test"
sequencer_feed_url = "wss://arb1-feed.arbitrum.io/feed"
sequencer_connections = 5
block_poll_interval_ms = 250

[contracts]
pool_addresses_provider = "0xa97684ead0e402dC232d5A977953DF7ECBaB3CDb"
uniswap_v3_router = "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45"
liquidation_executor = ""

[thegraph]
subgraph_url = "https://gateway.thegraph.com/api/test/subgraphs/id/test"
poll_interval_secs = 3600
max_health_factor = 1.1
page_size = 1000

[strategy]
min_profit_bps = 280
flash_loan_premium_bps = 5
max_slippage_bps = 50
max_gas_price_gwei = 5.0
min_debt_usd = 100
max_flash_loan_usd = 1000

[bot]
paper_mode = true
skip_simulation = false
private_key_env = "BOT_PRIVATE_KEY"
thegraph_api_key_env = "THEGRAPH_API_KEY"
log_level = "info"
"#
        )
        .unwrap();
        file
    }

    #[test]
    fn test_load_valid_config() {
        let file = write_test_config();
        let config = AppConfig::load(file.path()).unwrap();
        assert_eq!(config.network.chain_id, 42161);
        assert!(config.bot.paper_mode);
        assert_eq!(config.strategy.min_profit_bps, 280);
    }
}
