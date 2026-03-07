# PolyHFT — Polymarket Crypto Trading Bot

![Rust](https://img.shields.io/badge/Built_With-Rust-orange?style=flat-square&logo=rust)
![License](https://img.shields.io/badge/License-MIT-blue?style=flat-square)

A high-frequency trading bot for Polymarket's 5-minute crypto markets, built in Rust. Connects to Binance, Coinbase, and Polymarket WebSocket feeds for real-time price data and executes trades via the Polymarket CLOB SDK.

## Features

- **Multi-asset support** — Separate bots for BTC and ETH 5-minute markets
- **Real-time feeds** — Concurrent Binance, Coinbase, Deribit DVOL, and Polymarket WebSocket streams
- **Risk management** — De-peg killswitch, velocity lockout, volatility desert detection, and drift reconciliation
- **Telegram alerts** — Async execution receipts and PnL notifications
- **Docker ready** — One command to build and deploy
- **Simulation mode** — Paper trade with live data before going live

## Project Structure

```
├── btc-5min-bot/       # BTC 5-minute market bot
│   └── src/
│       ├── main.rs         # WebSocket feeds, market discovery, main loop
│       ├── strategy.rs     # Entry/exit logic, order execution, SDK auth
│       ├── state.rs        # Persistent state & PnL tracking
│       ├── atr.rs          # ATR (Average True Range) monitor
│       ├── velocity.rs     # Velocity lockout logic
│       └── config.rs       # Market configuration
├── eth-5min-bot/       # ETH 5-minute market bot (same structure)
├── core-shared/        # Shared library (types, WebSocket parsing)
├── docker-compose.yml
├── Dockerfile
└── .env.example
```

## Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) (for local builds) or Docker
- Polygon RPC URL (Alchemy recommended)
- Polymarket wallet with USDC.e on Polygon

### Setup

1. **Clone and configure**

   ```bash
   git clone https://github.com/TheOverLordEA/poly-hft-engine.git
   cd poly-hft-engine
   cp .env.example .env.btc   # and/or .env.eth
   ```

2. **Edit `.env.btc`** with your credentials:

   ```env
   PRIVATE_KEY=0xYourPrivateKey
   POLYGON_RPC_URL=https://polygon-mainnet.g.alchemy.com/v2/YOUR_KEY
   POLYMARKET_WS_URL=wss://ws-subscriptions-clob.polymarket.com/ws/market
   TRADING_ENABLED=true
   ```

   See [.env.example](.env.example) for all options (Telegram alerts, funder address, signature type, etc).

3. **Run with Docker** (recommended)

   ```bash
   docker compose up btc-5min-bot -d --build
   docker compose logs btc-5min-bot -f          # watch logs
   ```

   Or run locally:

   ```bash
   cargo build --release
   ./target/release/btc-5min-bot
   ```

### Testing Safely

- **Simulation mode** — Set `TRADING_ENABLED=false`. The bot tracks live data and simulates fills without signing transactions.
- **Ghost test** — Set `RUN_GHOST_TEST=true` to verify API keys and proxy wallet by placing and cancelling a $0.10 order.
- **Permission setup** — Set `SETUP_PERMISSIONS=true` to grant the required `setApprovalForAll` for selling shares.

## Strategy

The entry/exit logic lives in `strategy.rs` → `execute_tick()`. The engine provides:

- `gap` — price divergence from strike
- `time_to_expiry` — seconds remaining
- `active_dvol` — current implied volatility
- Binance/Coinbase prices, Polymarket orderbook spread

## Environment Variables

| Variable | Required | Description |
|---|---|---|
| `PRIVATE_KEY` | Yes | Polygon wallet private key |
| `POLYGON_RPC_URL` | Yes | Polygon RPC endpoint |
| `POLYMARKET_WS_URL` | Yes | Polymarket WebSocket URL |
| `TRADING_ENABLED` | No | `true` for live trading (default: `false`) |
| `MARKET_ASSET` | No | `btc` or `eth` (default: `btc`) |
| `SIGNATURE_TYPE` | No | `0` (EOA) or `2` (Gnosis Safe) |
| `FUNDER_ADDRESS` | No | Proxy/Safe address if using `SIGNATURE_TYPE=2` |
| `TELEGRAM_BOT_TOKEN` | No | Telegram bot token for alerts |
| `TELEGRAM_CHAT_ID` | No | Telegram chat ID for alerts |
