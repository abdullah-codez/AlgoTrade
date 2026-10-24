//! Thread-safe multi-timeframe candle cache supporting 1m, 5m, 15m, and 1h intervals.
//! Enforces chronological ordering, timestamp deduplication, in-progress bar merging,
//! aligned higher-timeframe aggregation, and bounded retention.

use crate::execution::client::CandleSnapshot;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio::sync::RwLock;

pub const DEFAULT_MAX_BARS_PER_TIMEFRAME: usize = 1000;

/// Multi-timeframe candlestick buffer.
#[derive(Clone)]
pub struct CandleCache {
    /// Maps interval string ("1m", "5m", "15m", "1h") -> (open_time_ms -> CandleSnapshot)
    buffers: Arc<RwLock<HashMap<String, BTreeMap<u64, CandleSnapshot>>>>,
    max_bars: usize,
}

impl CandleCache {
    pub fn new(max_bars: usize) -> Self {
        let mut buffers = HashMap::new();
        buffers.insert("1m".to_string(), BTreeMap::new());
        buffers.insert("5m".to_string(), BTreeMap::new());
        buffers.insert("15m".to_string(), BTreeMap::new());
        buffers.insert("1h".to_string(), BTreeMap::new());

        Self {
            buffers: Arc::new(RwLock::new(buffers)),
            max_bars: if max_bars == 0 {
                DEFAULT_MAX_BARS_PER_TIMEFRAME
            } else {
                max_bars
            },
        }
    }

    pub fn new_default() -> Self {
        Self::new(DEFAULT_MAX_BARS_PER_TIMEFRAME)
    }
}

impl Default for CandleCache {
    fn default() -> Self {
        Self::new_default()
    }
}

impl CandleCache {
    /// Deduplicates by open_time_ms and enforces maximum retention.
    pub async fn merge_snapshot(&self, interval: &str, candles: Vec<CandleSnapshot>) {
        let mut buffers = self.buffers.write().await;
        let buffer = buffers
            .entry(interval.to_string())
            .or_insert_with(BTreeMap::new);

        for candle in candles {
            buffer.insert(candle.open_time_ms, candle);
        }

        // Evict oldest bars if exceeding capacity
        while buffer.len() > self.max_bars {
            if let Some(oldest_key) = buffer.keys().next().copied() {
                buffer.remove(&oldest_key);
            } else {
                break;
            }
        }
    }

    /// Insert or update a candle snapshot. Alias for update_or_append.
    pub async fn insert_candle(&self, candle: CandleSnapshot) {
        self.update_or_append(candle).await;
    }

    /// Update an existing in-progress candle or append a new candle.
    /// Also aggregates 1m bars into aligned 5m, 15m, and 1h intervals.
    pub async fn update_or_append(&self, candle: CandleSnapshot) {
        let interval = candle.interval.clone();
        let mut buffers = self.buffers.write().await;

        Self::insert_or_update_in_buffer(
            buffers
                .entry(interval.clone())
                .or_insert_with(BTreeMap::new),
            &candle,
            self.max_bars,
        );

        // If this is a 1m candle, update/aggregate into aligned higher timeframes
        if interval == "1m" {
            Self::aggregate_1m_to_higher(
                buffers
                    .entry("5m".to_string())
                    .or_insert_with(BTreeMap::new),
                &candle,
                300_000,
                "5m",
                self.max_bars,
            );
            Self::aggregate_1m_to_higher(
                buffers
                    .entry("15m".to_string())
                    .or_insert_with(BTreeMap::new),
                &candle,
                900_000,
                "15m",
                self.max_bars,
            );
            Self::aggregate_1m_to_higher(
                buffers
                    .entry("1h".to_string())
                    .or_insert_with(BTreeMap::new),
                &candle,
                3_600_000,
                "1h",
                self.max_bars,
            );
        }
    }

    fn insert_or_update_in_buffer(
        buffer: &mut BTreeMap<u64, CandleSnapshot>,
        incoming: &CandleSnapshot,
        max_bars: usize,
    ) {
        if let Some(existing) = buffer.get_mut(&incoming.open_time_ms) {
            // Update in-progress bar
            let new_high = if incoming.high > existing.high {
                incoming.high
            } else {
                existing.high
            };
            let new_low = if incoming.low < existing.low {
                incoming.low
            } else {
                existing.low
            };
            existing.high = new_high;
            existing.low = new_low;
            existing.close = incoming.close;
            existing.close_time_ms = incoming.close_time_ms;
            existing.volume = incoming.volume;
            existing.trade_count = incoming.trade_count;
        } else {
            buffer.insert(incoming.open_time_ms, incoming.clone());
            while buffer.len() > max_bars {
                if let Some(oldest) = buffer.keys().next().copied() {
                    buffer.remove(&oldest);
                } else {
                    break;
                }
            }
        }
    }

    fn aggregate_1m_to_higher(
        buffer: &mut BTreeMap<u64, CandleSnapshot>,
        candle_1m: &CandleSnapshot,
        bucket_duration_ms: u64,
        target_interval: &str,
        max_bars: usize,
    ) {
        let bucket_open = (candle_1m.open_time_ms / bucket_duration_ms) * bucket_duration_ms;
        let bucket_close = bucket_open + bucket_duration_ms - 1;

        if let Some(existing) = buffer.get_mut(&bucket_open) {
            if candle_1m.high > existing.high {
                existing.high = candle_1m.high;
            }
            if candle_1m.low < existing.low {
                existing.low = candle_1m.low;
            }
            existing.close = candle_1m.close;
            existing.close_time_ms = bucket_close;
            existing.volume += candle_1m.volume;
            existing.trade_count += candle_1m.trade_count;
        } else {
            buffer.insert(
                bucket_open,
                CandleSnapshot {
                    open_time_ms: bucket_open,
                    close_time_ms: bucket_close,
                    coin: candle_1m.coin.clone(),
                    interval: target_interval.to_string(),
                    open: candle_1m.open,
                    high: candle_1m.high,
                    low: candle_1m.low,
                    close: candle_1m.close,
                    volume: candle_1m.volume,
                    trade_count: candle_1m.trade_count,
                },
            );
            while buffer.len() > max_bars {
                if let Some(oldest) = buffer.keys().next().copied() {
                    buffer.remove(&oldest);
                } else {
                    break;
                }
            }
        }
    }

    /// Retrieve up to `limit` candles for the specified interval in ascending chronological order.
    pub async fn get_candles(&self, interval: &str, limit: usize) -> Vec<CandleSnapshot> {
        let buffers = self.buffers.read().await;
        if let Some(buffer) = buffers.get(interval) {
            let total = buffer.len();
            let skip_count = total.saturating_sub(limit);
            buffer.values().skip(skip_count).cloned().collect()
        } else {
            Vec::new()
        }
    }

    /// Return the timestamp of the latest candle in the buffer for the specified interval.
    pub async fn last_open_time(&self, interval: &str) -> Option<u64> {
        let buffers = self.buffers.read().await;
        buffers
            .get(interval)
            .and_then(|b| b.keys().next_back().copied())
    }

    /// Return total cached bars across all timeframes.
    pub async fn total_bar_count(&self) -> usize {
        let buffers = self.buffers.read().await;
        buffers.values().map(|b| b.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Price;
    use rust_decimal_macros::dec;

    #[tokio::test]
    async fn test_candle_cache_merge_and_deduplication() {
        let cache = CandleCache::new(5);

        let c1 = CandleSnapshot {
            open_time_ms: 1000,
            close_time_ms: 1059,
            coin: "BTC".into(),
            interval: "1m".into(),
            open: Price::new(dec!(80000.0)).unwrap(),
            high: Price::new(dec!(80100.0)).unwrap(),
            low: Price::new(dec!(79900.0)).unwrap(),
            close: Price::new(dec!(80050.0)).unwrap(),
            volume: dec!(1.0),
            trade_count: 10,
        };
        let c2 = CandleSnapshot {
            open_time_ms: 2000,
            close_time_ms: 2059,
            coin: "BTC".into(),
            interval: "1m".into(),
            open: Price::new(dec!(80050.0)).unwrap(),
            high: Price::new(dec!(80200.0)).unwrap(),
            low: Price::new(dec!(80000.0)).unwrap(),
            close: Price::new(dec!(80150.0)).unwrap(),
            volume: dec!(2.0),
            trade_count: 20,
        };

        // Merge snapshot
        cache
            .merge_snapshot("1m", vec![c1.clone(), c2.clone()])
            .await;
        assert_eq!(cache.get_candles("1m", 10).await.len(), 2);

        // Merge duplicate snapshot
        cache.merge_snapshot("1m", vec![c1, c2]).await;
        assert_eq!(cache.get_candles("1m", 10).await.len(), 2);
    }

    #[tokio::test]
    async fn test_in_progress_bar_update() {
        let cache = CandleCache::new(5);

        let initial = CandleSnapshot {
            open_time_ms: 5000,
            close_time_ms: 5059,
            coin: "BTC".into(),
            interval: "1m".into(),
            open: Price::new(dec!(80000.0)).unwrap(),
            high: Price::new(dec!(80050.0)).unwrap(),
            low: Price::new(dec!(79950.0)).unwrap(),
            close: Price::new(dec!(80020.0)).unwrap(),
            volume: dec!(1.0),
            trade_count: 5,
        };
        cache.update_or_append(initial).await;

        // In-progress tick: higher high and new close
        let updated = CandleSnapshot {
            open_time_ms: 5000,
            close_time_ms: 5059,
            coin: "BTC".into(),
            interval: "1m".into(),
            open: Price::new(dec!(80000.0)).unwrap(),
            high: Price::new(dec!(80120.0)).unwrap(), // higher high
            low: Price::new(dec!(79950.0)).unwrap(),
            close: Price::new(dec!(80110.0)).unwrap(),
            volume: dec!(2.5),
            trade_count: 12,
        };
        cache.update_or_append(updated).await;

        let candles = cache.get_candles("1m", 10).await;
        assert_eq!(candles.len(), 1);
        assert_eq!(candles[0].high.value(), dec!(80120.0));
        assert_eq!(candles[0].close.value(), dec!(80110.0));
        assert_eq!(candles[0].volume, dec!(2.5));
    }

    #[tokio::test]
    async fn test_aggregation_from_1m_to_5m() {
        let cache = CandleCache::new(10);

        // Bar at t = 0
        let b1 = CandleSnapshot {
            open_time_ms: 0,
            close_time_ms: 59_999,
            coin: "BTC".into(),
            interval: "1m".into(),
            open: Price::new(dec!(80000.0)).unwrap(),
            high: Price::new(dec!(80100.0)).unwrap(),
            low: Price::new(dec!(79900.0)).unwrap(),
            close: Price::new(dec!(80050.0)).unwrap(),
            volume: dec!(1.0),
            trade_count: 10,
        };
        cache.update_or_append(b1).await;

        // Bar at t = 60,000 (still within 0..300,000 bucket)
        let b2 = CandleSnapshot {
            open_time_ms: 60_000,
            close_time_ms: 119_999,
            coin: "BTC".into(),
            interval: "1m".into(),
            open: Price::new(dec!(80050.0)).unwrap(),
            high: Price::new(dec!(80300.0)).unwrap(), // higher high
            low: Price::new(dec!(80020.0)).unwrap(),
            close: Price::new(dec!(80250.0)).unwrap(),
            volume: dec!(2.0),
            trade_count: 15,
        };
        cache.update_or_append(b2).await;

        // Check 5m bucket
        let candles_5m = cache.get_candles("5m", 10).await;
        assert_eq!(candles_5m.len(), 1);
        assert_eq!(candles_5m[0].open_time_ms, 0);
        assert_eq!(candles_5m[0].open.value(), dec!(80000.0));
        assert_eq!(candles_5m[0].high.value(), dec!(80300.0));
        assert_eq!(candles_5m[0].low.value(), dec!(79900.0));
        assert_eq!(candles_5m[0].close.value(), dec!(80250.0));
        assert_eq!(candles_5m[0].volume, dec!(3.0));
        assert_eq!(candles_5m[0].trade_count, 25);
    }
}
