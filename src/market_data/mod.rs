//! Market data subsystem for streaming real-time mark prices and detecting stale feeds.
//!
//! - [`cache`]: High-performance thread-safe MarkPriceCache with timestamp tracking.
//! - [`detector`]: StaleDataDetector enforcing 5,000ms threshold and 3-step StaleRecoveryGuard.
//! - [`feed`]: Resilient Hyperliquid WebSocket subscriber streaming activeAssetCtx and userFundings.

pub mod cache;
pub mod candle_cache;
pub mod detector;
pub mod feed;

pub use cache::MarkPriceCache;
pub use candle_cache::CandleCache;
pub use detector::{
    RecoveryBlocker, StaleDataDetector, StaleRecoveryGuard, StalenessStatus,
    DEFAULT_STALENESS_THRESHOLD_MS,
};
pub use feed::{HyperliquidWsFeed, MarketFeedEvent, WsFeedError};
