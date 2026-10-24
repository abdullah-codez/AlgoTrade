//! Thread-safe cache storing the latest mark price and timestamp for continuous pricing triggers.

use crate::types::Price;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

/// Thread-safe mark price cache storing latest observed mark price and update timestamp.
#[derive(Debug, Default)]
pub struct MarkPriceCache {
    latest_price: RwLock<Option<Price>>,
    last_update_ms: AtomicU64,
}

impl MarkPriceCache {
    pub fn new() -> Self {
        Self {
            latest_price: RwLock::new(None),
            last_update_ms: AtomicU64::new(0),
        }
    }

    /// Update cache with a fresh mark price and timestamp in milliseconds.
    pub fn update(&self, price: Price, timestamp_ms: u64) {
        if let Ok(mut lock) = self.latest_price.write() {
            *lock = Some(price);
        }
        self.last_update_ms.store(timestamp_ms, Ordering::Release);
    }

    /// Retrieve the latest mark price if present.
    pub fn latest_price(&self) -> Option<Price> {
        self.latest_price.read().ok().and_then(|guard| *guard)
    }

    /// Retrieve the timestamp in milliseconds of the last recorded update.
    pub fn last_update_ms(&self) -> u64 {
        self.last_update_ms.load(Ordering::Acquire)
    }

    /// Retrieve both price and timestamp if populated.
    pub fn latest(&self) -> Option<(Price, u64)> {
        let price = self.latest_price()?;
        let ts = self.last_update_ms();
        if ts > 0 {
            Some((price, ts))
        } else {
            None
        }
    }

    /// Age of the cached data in milliseconds relative to a given timestamp.
    pub fn age_ms(&self, current_time_ms: u64) -> Option<u64> {
        let last = self.last_update_ms();
        if last == 0 {
            None
        } else {
            Some(current_time_ms.saturating_sub(last))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_mark_price_cache_lifecycle() {
        let cache = MarkPriceCache::new();
        assert!(cache.latest_price().is_none());
        assert_eq!(cache.last_update_ms(), 0);

        let p1 = Price::new(dec!(67000.5)).unwrap();
        cache.update(p1, 1000);

        assert_eq!(cache.latest_price(), Some(p1));
        assert_eq!(cache.last_update_ms(), 1000);
        assert_eq!(cache.age_ms(1500), Some(500));

        let p2 = Price::new(dec!(67100.0)).unwrap();
        cache.update(p2, 2000);
        assert_eq!(cache.latest_price(), Some(p2));
        assert_eq!(cache.last_update_ms(), 2000);
    }
}
