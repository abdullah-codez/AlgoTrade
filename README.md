# AlgoTrade — BTC Perpetual Trading Bot

Deterministic algorithmic trading engine engineered in Rust targeting the **Hyperliquid Testnet**.

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](https://www.rust-lang.org)
[![Network](https://img.shields.io/badge/Exchange-Hyperliquid%20Testnet-blue.svg)](https://app.hyperliquid-testnet.xyz)
[![License](https://img.shields.io/badge/License-Apache%202.0-green.svg)](LICENSE)

---

## Quick Start

### 1. Prerequisites
* **Rust toolchain** (1.75 or later): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
* Hyperliquid Testnet account funded with strictly **$10 testnet USDC**.
* Dedicated Agent Wallet authorized via the Hyperliquid Testnet UI (`More -> API / Agents`).

### 2. Configure Credentials
Copy `.env.example` to `.env` and fill in your testnet account address and dedicated agent signer private key:

```bash
cp .env.example .env
```

Edit `.env`:
```env
HYPERLIQUID_TESTNET_API_URL=https://api.hyperliquid-testnet.xyz
HYPERLIQUID_TESTNET_WS_URL=wss://api.hyperliquid-testnet.xyz/ws
HYPERLIQUID_TESTNET_ACCOUNT_ADDRESS=0xYourAccountAddressHere
HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY=0xYourAgentPrivateKeyHere
SERVER_ADDR=127.0.0.1:3000
RUST_LOG=info
```

> **Security Invariant**: Never commit `.env` or expose private keys. Frontend code and logs never receive private keys.

### 3. Build & Run
Run the engine and local web server:

```bash
cargo run --release
```

Open your browser to the local dashboard:
```
http://127.0.0.1:3000
```

> **Safety Notice**: The engine boots in `STOPPED` mode. Opening the dashboard does not execute trades. To begin trading, configure your desired profile and click **START ENGINE**.

---

## User Manual & Documentation

For the complete operator manual, detailed architectural overview, credential setup, 30-day agent lifecycle rules, panel walkthroughs, and troubleshooting guides, see:

📖 **[Operator Manual & Runbook](docs/USER_MANUAL.md)**

---

## Key Features

* **Deterministic Core**: Pure financial mathematical rules. No ML, LLMs, or speculative heuristics.
* **Perpetual Futures Only**: Focused strictly on Hyperliquid BTC Perpetual futures (`BTC-PERP`).
* **Continuous Protection**: Every open trade is backed by resting exchange-side reduce-only Take-Profit (TP) and Stop-Loss (SL) triggers.
* **Internal Capital Invariants**: $10 total strategy budget, $8 hard initial margin cap, $2 buffer reserve target.
* **Trading Profiles**:
  * `NORMAL`: 5x leverage, 1.0% TP, 0.5% SL, $0.70 net session target.
  * `GREEDY`: 10x leverage, 1.5% TP, 0.75% SL, $2.00 net session target, optional trailing stop.
* **6-Panel Web Dashboard**:
  1. **Account Overview**: Live Hyperliquid perpetual equity, withdrawable collateral, and margin used.
  2. **Bot Capital Ledger**: Internal budget allocation and live reallocation controls (`Transfer to Reserve` / `Transfer to Trading`).
  3. **Live BTC Perpetual Market**: TradingView Lightweight Charts™ candlestick & volume chart (1m, 5m, 15m, 1h).
  4. **Current Position and Protection**: Live MTM P&L, contract sizing, and active exchange TP/SL order IDs.
  5. **Strategy & Risk Settings**: Profile selector, independent margin modes (`ISOLATED`/`CROSS`), prospective trade preview card (`ESTIMATE / PENDING FILL`), and distinct lifecycle controls (`START`, `PAUSE`, `STOP`, `EMERGENCY FLATTEN`).
  6. **Activity & Trade History**: FIFO matched closed trades ledger and append-only real-time audit stream.

---

## Testing & Verification

Run the full deterministic unit and scenario integration test suite:

```bash
cargo test
```

Run static analysis and formatting checks:

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
```

---

## Third-Party Licenses & Attribution

* **TradingView Lightweight Charts™ v4.2.0**:
  Licensed under the **Apache License, Version 2.0**.  
  Copyright (c) 2024 TradingView, Inc. https://www.apache.org/licenses/LICENSE-2.0
