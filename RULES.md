# RULES.md — Inviolable Engineering, Verification & Safety Rules

Every agent, developer, and automated process operating in this repository is strictly bound by these mandatory rules:

---

## 1. Safety & Mainnet Prohibition
1. **NEVER TARGET MAINNET**:
   - The production base URL (`api.hyperliquid.xyz`) is strictly banned across all configurations, code, scripts, and tests.
   - Any URL or host not resolving to `api.hyperliquid-testnet.xyz` must fail initialization immediately with a configuration error.
2. **NO SECRET LEAKS & ENFORCED CREDENTIAL ROLES**:
   - Never commit private keys, wallet mnemonics, API secrets, or `.env` files into git.
   - All credentials must be loaded from environment variables:
     * `HYPERLIQUID_TESTNET_ACCOUNT_ADDRESS`: Identifies the trading account (funded with strictly $10 testnet USDC) and is used exclusively for account-specific queries (positions, open orders, account state).
     * `HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY`: Private key of the dedicated testnet API/agent wallet used exclusively to derive the signer address and sign authenticated actions (orders, cancels, leverage updates).
   - The agent wallet must be authorized by the target account on Hyperliquid before it can sign trading actions. Never use an account's master private key as a fallback.
   - Only dummy/placeholder examples may exist in `.env.example`.
   - Never log private keys, secret values, or raw signatures in audit logs or console output.
3. **DEDICATED $10 TESTNET ACCOUNT**:
   - Trading must occur exclusively on a dedicated testnet account funded with strictly $10.00 (e.g. 10 testnet USDC).
   - Never run this bot on an account with more than the $10 intended testnet capital.
4. **LOCAL WEB DASHBOARD SECURITY & INVARIANTS**:
   - The web server must bind exclusively to loopback (`127.0.0.1:3000`). Never bind to `0.0.0.0` or expose the dashboard to the public internet.
   - The frontend is strictly a command-staging and telemetry-display layer: **the frontend must never submit exchange orders directly**.
   - Private keys and sensitive credentials must remain server-side and never be returned in API payloads or rendered in the DOM.
   - **The engine must start STOPPED**: Launching the application or opening the dashboard must never automatically start trading. Starting or resuming requires explicit user action.
   - **Backtesting Isolation**: The backtester has zero exchange access and can never submit live orders or touch testnet state. Historical simulation results must never be mixed with live execution records.


---

## 2. Financial Rigor & Risk Boundaries
1. **NEVER BYPASS RISK LIMITS**:
   - The strategy budget is hard-capped at $10.00.
   - Initial margin per position is hard-capped at $8.00.
   - The remaining $2.00 is an operational reserve for taker fees, funding, and adverse slippage.
   - If a calculated position notional is below the exchange minimum ($10.00), the order must be safely aborted, never forced.
2. **NO MARTINGALE OR LOSS EXPANSION**:
   - Never increase position sizing after a loss to recover capital. Sizing must always be computed strictly from initial margin and configured leverage.
3. **EXACT DECIMAL ARITHMETIC**:
   - All prices, sizes, fees, cash flows, and P&L must be computed using `rust_decimal::Decimal`.
   - Floating-point arithmetic (`f32`, `f64`) is prohibited for financial values.
4. **SESSION RISK DEFINITIONS**:
   - Session limits are evaluated on Total Equity Change: $\text{Realized Net P\&L} + \text{Unrealized P\&L (MTM)} - \text{Estimated Exit Fees}$.
   - Stop-loss triggers and session loss limits are **triggers to attempt an emergency exit**, not guaranteed caps on loss (due to Hyperliquid's 10% market slippage tolerance).

---

## 3. Engineering & Verification Integrity
1. **NO CLAIMS WITHOUT VERIFICATION**:
   - Never claim a test passed, lint passed, or build succeeded without actually executing the command and observing the output.
   - Rerun tests after any code modification.
2. **ZERO COMPILATION & CLIPPY WARNINGS**:
   - All code must compile cleanly and pass `cargo clippy --all-targets -- -D warnings`.
   - Formatting must adhere to `cargo fmt -- --check`.
3. **MANDATORY IMPLEMENTATION LOG**:
   - Every milestone must produce/update `IMPLEMENTATION.md` before completion.
4. **MILESTONE BOUNDARY HALT**:
   - Upon completing a milestone's acceptance criteria, execution must **STOP**.
   - Do not begin the subsequent milestone until the user has performed manual testing and provided explicit written approval.
