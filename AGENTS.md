# AGENTS.md — BTC Perpetual Trading Bot Architecture & Governance

## 1. Project Overview & Role
You are operating within the **BTC Perpetual Trading Bot** codebase, an ultra-modular, deterministic trading engine in Rust targeting the **Hyperliquid Testnet**.
* **Engine Focus**: BTC perpetual futures only.
* **Paradigm**: Pure deterministic financial rules and exchange mark-price triggers. No LLMs, ML models, external inference APIs, or speculative heuristics.
* **Profiles**: Supports `NORMAL` (5x leverage, $8 initial margin cap, 1.0% TP, 0.5% SL, $0.70 net target) and `GREEDY` (10x leverage, $8 initial margin cap, 1.5% TP, 0.75% SL, $2.00 net target, optional trailing stop).

---

## 2. Mandatory Workflow & Approval Gates
The project is organized into **EXACTLY FOUR MILESTONES**:
* **Milestone 1**: Project Foundation, Governance and Deterministic Core (Domain primitives, ledgers, sizing, state machines, hand-calculated unit tests).
* **Milestone 2**: Hyperliquid TESTNET Integration and Safe Execution (`hyperliquid_rust_sdk`, mark-price WebSocket, EIP-712 signing, partial fill protection, reconciliation).
* **Milestone 3**: Strategy Orchestration, Risk Controls and End-to-End Testnet Trials (Async engine loop, persistent state, safe shutdown, scenario simulations, live testnet trial).
* **Milestone 4**: Backtesting, Comparison and Final Validation (Binance USDⓈ-M 1m replay, gap handling, timestamped funding, locked out-of-sample benchmarks, final audit).

### Inviolable Milestone Gate Rules:
1. **Never begin a milestone** without explicit written authorization from the user.
2. At the conclusion of each milestone:
   - Run formatting (`cargo fmt -- --check`), static analysis (`cargo clippy --all-targets -- -D warnings`), and test suites (`cargo test`).
   - Audit the implementation for race conditions, financial math errors, and failure modes.
   - Update `IMPLEMENTATION.md` detailing every file created/modified, architectural rationale, commands run, test results, and exact manual testing steps.
   - **STOP**. Deliver the milestone report and await user manual testing and explicit approval before touching the next milestone.

---

## 3. Architecture & Module Responsibilities

```
src/
├── lib.rs              # Root library export
├── main.rs             # Application entry point: starts Axum web server & engine
├── types.rs            # Strongly typed domain newtypes (Price, Quantity, SignedUsd, MarginAllocation)
├── ledger/             # FillLedger & FundingLedger (Direction-aware P&L, real-time funding, fee tracking)
├── config/             # NORMAL & GREEDY profiles, MarketMetadata, invariant validation
├── strategy/           # Pure TP/SL trigger calculations, re-entry, reversal, trailing stop rules
├── risk/               # Position sizing, mark-to-market exposure (incl. exit costs), loss streak tracking
├── state/              # Decoupled EngineMode & PositionState machines, deterministic seeded RNG
├── execution/          # Hyperliquid Testnet adapter, EIP-712 signing, protection manager, reconciler (M2)
├── market_data/        # activeAssetCtx WebSocket feed, mark price cache, stale-data detector (M2)
├── engine/             # Async orchestrator, atomic state persistence, graceful shutdown (M3)
├── audit/              # Structured append-only JSONL event logger (M3)
├── server/             # Embedded Axum web server, REST API, WebSocket broadcaster (M3)
└── backtest/           # Historical candle replay, intra-bar execution, metric calculator (M4)

web/                    # Local Web Dashboard (Served at 127.0.0.1:3000)
├── index.html          # Single-page UI with Trading Control, Live Performance & Backtesting
├── css/styles.css      # Dark-mode styling, glassmorphism, responsive cards
└── js/                 # Modular ES6 controllers (control, performance, backtest, ws)
```

---

## 4. Web Dashboard Governance & Security Invariants
1. **Loopback Binding Only**:
   - The web server must bind strictly to `127.0.0.1:3000` (loopback only, never `0.0.0.0`).
2. **Frontend Never Submits Exchange Orders**:
   - The web UI is strictly a presentation and command-staging layer. It communicates exclusively with local Rust backend endpoints.
   - Private keys never leave the server process; they are never exposed to the frontend or logs.
3. **Engine Starts STOPPED**:
   - Booting the application or launching the web dashboard must **never** automatically start trading. Resuming or starting trading requires explicit user action.
4. **Independent Margin Mode**:
   - `ISOLATED` and `CROSS` margin are selectable independently of strategy profiles (`NORMAL` vs `GREEDY`).
5. **Execution Mode Divergence**:
   - `Automatic`: Trades execute autonomously within risk boundaries upon explicit start.
   - `Confirmation Required`: Holds entry signals in `AwaitingConfirmation` pending explicit user approval in the UI. Open position protective exits execute automatically without confirmation.
6. **Isolated Backtesting**:
   - Backtesting operates 100% offline on candle data without network access and cannot submit exchange orders or alter live testnet positions. Simulated results are strictly segregated from live records.

---

## 5. Coding Conventions & Standards
1. **Financial Arithmetic**:
   - Never use IEEE-754 floating point (`f32`, `f64`) for prices, quantities, margin, fees, or P&L.
   - Always use `rust_decimal::Decimal` and its domain newtypes (`Price`, `Quantity`, `SignedUsd`, `MarginAllocation`).
2. **Direction-Aware Math**:
   - Gross fill P&L must always use: $\text{Gross P\&L} = (P_{\text{exit}} - P_{\text{entry}}) \times Q \times D$ where $D = +1$ for Long and $D = -1$ for Short.
3. **Continuous Protection**:
   - An open position on the exchange must always be covered by active exchange-side reduce-only trigger orders.
   - Partial fills must trigger incremental resizing of protective orders.
4. **Idempotency & Reconciler First**:
   - Never blindly retry timed-out submissions. Query exchange state using `cloid` before resubmitting.
5. **Zero Clippy Warnings**:
   - All code must pass `cargo clippy --all-targets -- -D warnings`.
6. **Credential Roles & Agent Signing**:
   - `HYPERLIQUID_TESTNET_ACCOUNT_ADDRESS`: Target account (funded with strictly $10 testnet USDC) used exclusively for account-specific queries (positions, open orders, account state).
   - `HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY`: Private key of the dedicated testnet API/agent wallet used exclusively to derive the signer address and sign authenticated actions (orders, cancels, leverage updates).
   - The agent wallet must be authorized by the target account on Hyperliquid before it can sign trading actions. Never use an account's master private key as a fallback.

