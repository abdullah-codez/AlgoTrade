//! AlgoTrade: Deterministic BTC Perpetual Trading Engine for Hyperliquid Testnet.
//!
//! Milestone 1 Core Deterministic Modules:
//! - [`types`]: Strongly typed domain primitives (Price, Quantity, SignedUsd, Direction, Side)
//! - [`config`]: Validated NORMAL and GREEDY profiles and dynamic market metadata
//! - [`ledger`]: Direction-aware FillLedger and FundingLedger with real-time fee and cost accounting
//! - [`strategy`]: Pure TP/SL trigger math, trailing stop logic, and direction reversal rules
//! - [`risk`]: Position sizer (respecting initial margin cap and $10 min notional), MTM equity risk, loss streaks
//! - [`state`]: Decoupled EngineMode and PositionState machines, HaltReason, and seeded RNG
//!
//! Milestone 2 Testnet Execution & Market Data:
//! - [`execution`]: Hyperliquid Testnet client, EIP-712 signing, protection manager, reconciler, and mock engine
//! - [`market_data`]: Mark price cache, WebSocket feed, and 5000ms stale data detector with recovery guard

pub mod audit;
pub mod config;
pub mod engine;
pub mod execution;
pub mod ledger;
pub mod market_data;
pub mod risk;
pub mod server;
pub mod state;
pub mod strategy;
pub mod types;
