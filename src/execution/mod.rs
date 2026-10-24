//! Execution modules for Hyperliquid Testnet integration.
//!
//! - [`guard`]: Strict host allowlist ensuring zero traffic targets mainnet.
//! - [`signing`]: EIP-712 Agent payload signing with LocalWallet.
//! - [`client`]: ExchangeClient trait and HyperliquidClient REST implementation.
//! - [`protection`]: Exchange-side continuous TP/SL reduce-only trigger coverage.
//! - [`reconciliation`]: Idempotent state reconciliation using cloid.
//! - [`mock`]: In-memory mock exchange for deterministic failure and execution testing.

pub mod client;
pub mod guard;
pub mod mock;
pub mod protection;
pub mod reconciliation;
pub mod signing;

pub use client::{
    shorten_address, validate_candle_interval, CandleSnapshot, ExchangeClient, ExecutionError,
    HyperliquidClient, OrderPlacementResult, OrderRequest, OrderTypeSpec, PerpAccountSummary,
    SignerStatus, UnifiedAccountSummary,
};
pub use mock::MockExchangeClient;
