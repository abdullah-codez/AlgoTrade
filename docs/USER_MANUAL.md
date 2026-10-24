# AlgoTrade — User Manual & Operator Runbook

**BTC Perpetual Futures Deterministic Trading Engine**  
*Hyperliquid Testnet Edition*

---

## 1. System Overview

AlgoTrade is an ultra-modular, deterministic algorithmic trading bot engineered in Rust for trading Bitcoin perpetual futures (`BTC-PERP`) on the Hyperliquid Testnet.

### Core Philosophy
* **Pure Determinism**: All trade execution, position sizing, risk thresholds, and exit triggers follow deterministic mathematical rules and exchange mark-price updates. There are **no** LLMs, machine learning models, external predictive APIs, or speculative heuristics involved in trading decisions.
* **BTC Perpetuals Only**: The bot strictly trades BTC perpetual contracts against USDC collateral on Hyperliquid Testnet (`https://api.hyperliquid-testnet.xyz`, `wss://api.hyperliquid-testnet.xyz/ws`). Spot trading and Spot balances are completely out of scope.
* **Continuous Protection**: Any open position on the exchange is covered by active, resting exchange-side reduce-only Take-Profit (TP) and Stop-Loss (SL) trigger orders.
* **Zero Float Financial Arithmetic**: All financial calculations (prices, quantities, margin, fees, P&L, funding) use exact 128-bit decimal arithmetic (`rust_decimal::Decimal`), avoiding IEEE-754 floating-point inaccuracies.

---

## 2. Safety Architecture & Governance Invariants

AlgoTrade is governed by hard security and risk rules:

1. **Loopback Binding Only (`127.0.0.1:3000`)**:
   The embedded Axum web server and WebSocket broadcaster bind strictly to loopback address `127.0.0.1:3000`. Requests with invalid or non-loopback `Host` headers are rejected with `403 Forbidden`.
2. **Engine Boots Stopped**:
   Launching the binary or opening the web dashboard **never** places orders or starts trading automatically. The engine initializes in `Stopped` mode. Trading requires explicit operator activation via the UI or API.
3. **Frontend Never Submits Exchange Orders**:
   The web dashboard is purely a telemetry presentation and command-staging interface. It communicates exclusively with local backend REST/WebSocket endpoints. Private keys never leave the server process; they are never exposed to the browser, logs, or API responses.
4. **Signer Status Gating**:
   All order-affecting controls (`Start Engine`, `Approve Proposal`, `Emergency Flatten`) are gated by active signer authorization. If the signer key is missing, invalid, or unauthorized, the dashboard displays a prominent warning banner and disables order execution.
5. **Continuous State Reconciliation**:
   The engine continuously queries exchange state (`clearinghouseState` and `openOrders`). Any discrepancy in position size, direction, or protective order coverage triggers an immediate state-reconciliation event.

---

## 3. Hyperliquid Testnet Setup & Credentials

AlgoTrade uses a two-wallet architecture separating the funded trading account from the signing agent.

### Environment Configuration (`.env`)
Create a `.env` file in the project root based on `.env.example`:

```env
# Hyperliquid Testnet API Endpoints
HYPERLIQUID_TESTNET_API_URL=https://api.hyperliquid-testnet.xyz
HYPERLIQUID_TESTNET_WS_URL=wss://api.hyperliquid-testnet.xyz/ws

# 1. Target Account Address (Funded with strictly $10 testnet USDC)
HYPERLIQUID_TESTNET_ACCOUNT_ADDRESS=0x0123456789abcdef0123456789abcdef01234567

# 2. Agent Signer Private Key (Dedicated agent wallet authorized to sign)
HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY=0x0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef

# Server Configuration
SERVER_ADDR=127.0.0.1:3000
RUST_LOG=info
```

### Credential Roles & Agent Lifecycle

| Variable | Role | Permissions |
| :--- | :--- | :--- |
| `HYPERLIQUID_TESTNET_ACCOUNT_ADDRESS` | Target Account | Read-only account queries: perpetual equity, available margin, open positions, order status. |
| `HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY` | Dedicated Agent Signer | EIP-712 transaction signing: order placement, cancels, leverage updates. |

#### 30-Day Agent Key Lifecycle
Hyperliquid agent wallet authorizations have an intended validity period of **30 days**:
1. **Creation & Authorization**:
   - In the Hyperliquid Testnet web app (`https://app.hyperliquid-testnet.xyz`), connect your master wallet.
   - Navigate to **More** → **API / Agents** and authorize your agent wallet address.
   - Set the agent wallet's private key as `HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY`.
2. **Revocation & Expiration**:
   - The master account can revoke the agent wallet at any time on the Hyperliquid website.
   - When the 30-day window expires or the key is revoked, the exchange rejects signing attempts.
   - AlgoTrade detects this status (`SignerStatus::UnauthorizedOrExpired`), halts trading, displays a warning banner, and falls back to safe read-only monitoring until a renewed key is configured.

---

## 4. Bot Capital Accounting ($10 Fixed Strategy Budget)

The internal capital ledger (`src/ledger/capital.rs`) enforces strict bounds:

```
┌────────────────────────────────────────────────────────┐
│            Total Strategy Budget: $10.00 USDC          │
├───────────────────────────────┬────────────────────────┤
│ Allocated Trading: $8.00 USDC │ Buffer Reserve: $2.00  │
│ (Max Margin Cap per trade)    │ (Fee & Funding Buffer) │
├───────────────┬───────────────┴────────────────────────┤
│ Committed:    │ Uncommitted Allocation:                │
│ $0.00 (Flat)  │ $8.00 (Available for new entries)      │
└───────────────┴────────────────────────────────────────┘
```

* **Total Budget ($10.00)**: The total capital allocated to the bot session.
* **Max Trading Margin Cap ($8.00)**: The maximum initial margin the bot is permitted to allocate to any single trade.
* **Buffer Reserve ($2.00)**: Liquid buffer reserved to absorb negative funding payments, taker fees, and margin slippage without triggering liquidation.
* **Committed Margin**: Capital actively tied up as margin in an open perpetual position. When the position closes, margin is released back to uncommitted allocation.
* **Manual Reallocation**:
  Operators can adjust the division between Trading and Reserve using the UI buttons:
  - **Transfer to Reserve**: Moves uncommitted trading allocation to the reserve (e.g. $8.00/$2.00 → $7.00/$3.00).
  - **Transfer to Trading**: Moves reserve back to trading up to the hard $8.00 cap.
  - Reallocations that would exceed the $8.00 trading cap or reduce reserve below zero are strictly rejected.

---

## 5. Strategy Profiles & Risk Controls

AlgoTrade offers two deterministic trading profiles, with independent margin and execution modes:

### Profile Comparison

| Parameter | `NORMAL` Profile | `GREEDY` Profile |
| :--- | :--- | :--- |
| **Target Leverage** | **5x** | **10x** |
| **Initial Margin Cap** | **$8.00 USDC** | **$8.00 USDC** |
| **Take-Profit (TP)** | **+1.00%** (Exchange Reduce-Only) | **+1.50%** (Exchange Reduce-Only) |
| **Stop-Loss (SL)** | **-0.50%** (Exchange Reduce-Only) | **-0.75%** (Exchange Reduce-Only) |
| **Net Session Target** | **+$0.70 USDC** | **+$2.00 USDC** |
| **Hard Session Loss Trigger** | **-$2.00 USDC** | **-$2.00 USDC** |
| **Consecutive Loss Limit**| **3 Consecutive Losses** | **3 Consecutive Losses** |
| **Trailing Stop** | Disabled | Optional (1.0% activation, 0.4% trail) |

### Independent Margin Modes
* **ISOLATED** (Default): Position risk is strictly confined to the allocated trade margin ($8 cap).
* **CROSS**: Position uses shared account collateral to support maintenance margin.
* *Note*: Margin mode is selected independently of strategy profile.

### Execution Modes
* **AUTOMATIC**: The bot autonomously executes entry orders and manages stops within risk rules upon explicit engine start.
* **CONFIRMATION REQUIRED**: The bot identifies valid entry conditions and presents a staged proposal with a 60-second countdown in the UI. The operator must click **Approve & Execute** to dispatch the order. Protective TP/SL exits execute automatically without confirmation.

---

## 6. Web Dashboard: 6-Panel Guide

The dashboard at `http://127.0.0.1:3000` is divided into 6 distinct panels:

```
┌────────────────────────────────────────────────────────────────────────┐
│ Header: Account (0x12..34) | Signer (Active) | Mode (STOPPED) | Profile│
├───────────────────────────────────┬────────────────────────────────────┤
│ 1. Account Overview               │ 2. Bot Capital Ledger              │
│ - Perpetual Equity ($ USDC)       │ - Allocated Trading ($8.00 cap)    │
│ - Withdrawable Collateral         │ - Buffer Reserve ($2.00 min)       │
│ - Exchange Margin Used            │ - Committed / Uncommitted Margin   │
│ - Feed Freshness & Latency        │ - Reallocation Controls            │
├───────────────────────────────────┼────────────────────────────────────┤
│ 3. Live BTC Perpetual Market      │ 4. Current Position & Protection   │
│ - Candlestick Chart (1m,5m,15m,1h)│ - State: FLAT / LONG / SHORT       │
│ - Volume Histogram (overlay)      │ - Unrealized & Realized Net P&L    │
│ - Real-time Mark Price & Pulse    │ - Active TP & SL Order IDs         │
├───────────────────────────────────┴────────────────────────────────────┤
│ 5. Strategy & Risk Settings (with Trade Preview Card & Controls)       │
│ - Profile (NORMAL/GREEDY) | Margin Mode (ISOLATED/CROSS)               │
│ - Prospective Trade Preview (Entry, TP/SL, Fees, Net P&L estimate)     │
│ - Lifecycle Buttons: START ENGINE | PAUSE | STOP | EMERGENCY FLATTEN   │
├────────────────────────────────────────────────────────────────────────┤
│ 6. Activity & Trade History (Tab 2)                                    │
│ - Closed Trades Table (FIFO Matched Ledger)                            │
│ - Real-time Append-Only JSONL Audit Stream                             │
└────────────────────────────────────────────────────────────────────────┘
```

### Detailed Panel Explanations

#### Panel 1: Account Overview
* **Perpetual Equity**: Total net liquidation value of the Hyperliquid perpetual account (`clearinghouseState.marginSummary.accountValue`).
* **Withdrawable Collateral**: Free collateral available to withdraw or use as initial margin (`clearinghouseState.withdrawable`).
* **Exchange Margin Used**: Total margin currently locked in positions on the exchange.
* **Signer Status Badge**: Displays `ACTIVE` (green), `READ ONLY`, `UNAUTHORIZED`, or `EXPIRED`.
* **Feed Latency**: Real-time WebSocket latency in milliseconds with freshness dot (green < 3000ms, red if stale).

#### Panel 2: Bot Capital Ledger
* **Allocated Trading & Reserve**: Current breakdown of internal capital.
* **Committed Margin**: Capital tied up in the open position.
* **Uncommitted Allocation**: Free bot trading margin available for new trade entries.
* **Realized Performance Strip**: Bot realized net P&L, cumulative taker fees paid, and cumulative funding received/paid.
* **Reallocation Controls**: Enter an amount and click `Transfer to Reserve` or `Transfer to Trading`.

#### Panel 3: Live BTC Perpetual Market
* **Candlestick Chart**: Embedded TradingView Lightweight Charts™ candlestick visualization.
* **Timeframe Selector**: Toggle between `1m`, `5m`, `15m`, and `1h` candle aggregations.
* **Volume Histogram**: Integrated sub-panel displaying buy/sell volume per bar.
* **Live Mark Price**: Dynamic exchange mark price updated via WebSocket with pulse animation.

#### Panel 4: Current Position and Protection
* **Position State**: Visual indicator showing `FLAT`, `LONG` (green), or `SHORT` (red).
* **Position Details**: Active contract size in BTC, entry price, and current mark price.
* **Unrealized Gross P&L**: Continuous direction-aware mark-to-market P&L.
* **Active Protection Orders**: Displays active Take-Profit and Stop-Loss exchange order IDs (`#oid`). Shows `Protected by Exchange Reduce-Only` when orders are resting on the order book.

#### Panel 5: Strategy & Risk Settings + Trade Preview
* **Configuration Form**: Toggle profile (`NORMAL` / `GREEDY`), margin mode (`ISOLATED` / `CROSS`), execution mode (`AUTOMATIC` / `CONFIRMATION`), trailing stop, and deterministic RNG seed.
* **Prospective Trade Preview Card**:
  Calculates prospective order specifications before execution based on live mark price and 0.045% benchmark taker fee rates:
  - Est. Entry Price, Size (BTC), Notional ($), Margin Required ($)
  - Est. TP Price (+1.0% or +1.5%) and Gross Profit
  - Est. SL Price (-0.5% or -0.75%) and Gross Loss
  - Est. Round-Trip Taker Fees
  - Est. Net TP Profit and Net SL Loss
  - Clearly labeled `ESTIMATE / PENDING FILL`.
* **Lifecycle Control Buttons**:
  - `START ENGINE`: Activates trading loop (gated by pre-flight checks and active signer).
  - `PAUSE NEW ENTRIES`: Halts new trade entries while keeping open positions and protective stops fully active.
  - `STOP STRATEGY`: Graceful shutdown after the current trade completes.
  - `EMERGENCY FLATTEN`: Opens a confirmation modal. When confirmed, immediately dispatches a reduce-only market order to close open positions and halt the engine.

#### Panel 6: Activity & Trade History
* **Closed Trades Ledger**: FIFO matched fills with direction, entry price, exit price, size, gross P&L, fees, and realized net P&L.
* **Real-time Audit Stream**: Structured append-only security log streaming every risk check, order dispatch, fill, and reconciliation event.

---

## 7. Troubleshooting & Operational Runbook

### Issue: "Signer Status: Unauthorized or Read-Only"
* **Symptom**: Dashboard displays warning banner; Start Engine and Approve buttons are disabled.
* **Cause**: `HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY` is not authorized on Hyperliquid for the configured account, or the 30-day authorization window has elapsed.
* **Remedy**:
  1. Open `https://app.hyperliquid-testnet.xyz` with the account wallet.
  2. Navigate to **More** → **API / Agents**.
  3. Authorize the agent wallet address derived from your agent private key.
  4. Restart the bot or wait for the next account poll (5 seconds).

### Issue: "Exchange State Discrepancy Detected"
* **Symptom**: Red discrepancy banner appears at the top of the dashboard.
* **Cause**: Internal position state diverges from the exchange clearinghouse state (e.g. manual order placed on the exchange, partial liquidation, or network desync).
* **Remedy**:
  The engine's built-in reconciler (`src/execution/protection.rs`) runs every cycle:
  - Resizes or cancels protective orders to match the actual exchange position.
  - If unexpected exposure exists, the engine halts new entries and requests operator review.
  - Click **Emergency Flatten** if you wish to exit immediately to cash.

### Issue: "Market Feed Stale (>3000ms)"
* **Symptom**: Freshness indicator turns red.
* **Cause**: WebSocket connection to Hyperliquid testnet interrupted or experiencing high latency.
* **Remedy**:
  - The bot automatically reconnects with exponential backoff (1s to 10s).
  - While the feed is stale, new entry orders are strictly blocked.
  - Open positions remain safely covered by exchange-side resting reduce-only triggers.

---

## 8. Third-Party Licenses & Attribution

* **TradingView Lightweight Charts™**:
  The dashboard includes TradingView Lightweight Charts™ v4.2.0 (`web/js/lightweight-charts.standalone.production.js`), licensed under the **Apache License, Version 2.0**.
  Copyright (c) 2024 TradingView, Inc.
  https://www.apache.org/licenses/LICENSE-2.0
