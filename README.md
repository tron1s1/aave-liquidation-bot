# Aave V3 Flash-Loan Liquidation Bot — Arbitrum

High-performance liquidation bot for Aave V3 on Arbitrum mainnet. Detects underwater positions (HF < 1.0), executes atomic flash-loan liquidations, and keeps the liquidation bonus as pure profit.

## Architecture

```
TheGraph (hourly) ──► Watchlist (DashMap, 15k+ users)
Sequencer Feed ──► Local Math (HF recalc) ──► Simulator (eth_call) ──► Executor
                                                                    └──► Paper Mode
```

## Quick Start

### 1. Prerequisites
- Rust 1.95+ (`rustup install stable`)
- Docker & Docker Compose (for Nitro relay)
- Alchemy API key (for Arbitrum RPC)
- TheGraph API key (for subgraph access)

### 2. Configure
```bash
cp config.toml config.local.toml
# Edit config.local.toml with your RPC URLs
```

### 3. Set Environment Variables
```bash
export BOT_PRIVATE_KEY="0x..."           # Your bot wallet private key
export THEGRAPH_API_KEY="your-api-key"   # TheGraph API key
export CONFIG_PATH="config.local.toml"
```

### 4. Run in Paper Mode (Recommended First)
```bash
cargo run --release
```
Paper mode is ON by default (`paper_mode = true` in config). The bot will:
- Fetch 15,000+ borrowers from TheGraph
- Monitor the sequencer feed for price updates
- Log "WOULD HAVE SUBMITTED LIQUIDATION" when opportunities are found
- **No real transactions are sent**

### 5. Deploy with Docker (Production)
```bash
cd docker
docker compose up -d
```

## Smart Contract Deployment

### Prerequisites
- Install [Foundry](https://getfoundry.sh/)

### Build & Test
```bash
cd contracts
forge build
forge test --fork-url $ARBITRUM_RPC_URL -vvv
```

### Deploy to Arbitrum
```bash
forge create src/LiquidationExecutor.sol:LiquidationExecutor \
  --rpc-url $ARBITRUM_RPC_URL \
  --private-key $DEPLOYER_KEY \
  --constructor-args \
    "0xa97684ead0e402dC232d5A977953DF7ECBaB3CDb" \
    "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45" \
    280
```

Then set `liquidation_executor` in `config.toml` to the deployed address.

## Gas Targets
| Path | Target | Notes |
|------|--------|-------|
| Revert (bad HF, no profit) | < 35k gas | Custom errors, early checks |
| Happy path (full liquidation) | < 450k gas | Flash loan + liquidation + swap |

## Security
- OWASP 15-Category audit performed after every build phase
- ReentrancyGuard on all entry points
- Owner-only execution (immutable)
- SafeERC20 for all token operations
- Min profit enforced on-chain (cannot be bypassed)
- Private key from env vars only (never in code or config)

## Profit Model
- **Source:** Aave V3 liquidation bonus (typically 5% for major assets)
- **Costs:** 0.05% flash loan fee + gas (~$0.10-0.50) + swap slippage (~0.3-0.5%)
- **Min threshold:** 2.8% net profit per trade
- **Zero capital required:** Flash loans provide all liquidity

## AWS us-east-1 Deployment
1. Launch EC2 instance (`t3.medium`) in `us-east-1`
2. Install Docker
3. Clone repo and configure
4. Run `docker compose up -d`
5. Monitor logs: `docker compose logs -f bot`
