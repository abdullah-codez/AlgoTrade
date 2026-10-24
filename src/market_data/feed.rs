//! Hyperliquid Testnet WebSocket client streaming real-time mark prices and funding rates.
//! Strictly guarded against non-testnet endpoints via `validate_testnet_ws_url`.

use crate::execution::guard::{validate_testnet_ws_url, GuardError};
use crate::execution::CandleSnapshot;
use crate::ledger::FundingRecord;
use crate::types::{Price, Quantity, SignedUsd};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde_json::json;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

#[derive(Error, Debug)]
pub enum WsFeedError {
    #[error("Guard error: {0}")]
    Guard(#[from] GuardError),
    #[error("WebSocket connection error: {0}")]
    Connection(String),
    #[error("Failed to serialize subscription payload: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Channel closed")]
    ChannelClosed,
}

/// Normalized event emitted by the market data WebSocket feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarketFeedEvent {
    Connected,
    Disconnected(String),
    MarkPriceUpdate {
        coin: String,
        mark_price: Price,
        timestamp_ms: u64,
    },
    CandleUpdate(CandleSnapshot),
    FundingUpdate(FundingRecord),
}

/// WebSocket subscriber streaming mark prices and user fundings from Hyperliquid Testnet.
pub struct HyperliquidWsFeed {
    ws_url: String,
    coin: String,
    user_address: Option<String>,
}

impl HyperliquidWsFeed {
    pub fn new(
        ws_url: &str,
        coin: impl Into<String>,
        user_address: Option<String>,
    ) -> Result<Self, WsFeedError> {
        let valid_url = validate_testnet_ws_url(ws_url)?;
        Ok(Self {
            ws_url: valid_url.to_string(),
            coin: coin.into(),
            user_address,
        })
    }

    /// Spawn a resilient background task that continuously streams market data,
    /// reconnecting with exponential backoff on disconnect.
    pub fn spawn_feed_listener(
        self,
        buffer_size: usize,
    ) -> (mpsc::Receiver<MarketFeedEvent>, tokio::task::JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(buffer_size);
        let handle = tokio::spawn(async move {
            self.run_feed_loop(tx).await;
        });
        (rx, handle)
    }

    async fn run_feed_loop(&self, tx: mpsc::Sender<MarketFeedEvent>) {
        let mut backoff = Duration::from_millis(500);
        let max_backoff = Duration::from_secs(10);

        loop {
            info!(
                "Connecting to Hyperliquid Testnet WebSocket: {}",
                self.ws_url
            );
            match connect_async(&self.ws_url).await {
                Ok((ws_stream, _)) => {
                    info!("WebSocket connection established");
                    let _ = tx.send(MarketFeedEvent::Connected).await;
                    backoff = Duration::from_millis(500); // Reset backoff

                    let (mut write, mut read) = ws_stream.split();

                    // Subscribe to activeAssetCtx
                    let asset_sub = json!({
                        "method": "subscribe",
                        "subscription": {
                            "type": "activeAssetCtx",
                            "coin": self.coin
                        }
                    });

                    if let Err(e) = write
                        .send(Message::Text(asset_sub.to_string().into()))
                        .await
                    {
                        error!("Failed to send activeAssetCtx subscription: {}", e);
                        continue;
                    }

                    // Subscribe to userFundings if user address configured
                    if let Some(ref addr) = self.user_address {
                        let funding_sub = json!({
                            "method": "subscribe",
                            "subscription": {
                                "type": "userFundings",
                                "user": addr
                            }
                        });
                        let _ = write
                            .send(Message::Text(funding_sub.to_string().into()))
                            .await;
                    }

                    // Subscribe to 1m candles
                    let candle_sub = json!({
                        "method": "subscribe",
                        "subscription": {
                            "type": "candle",
                            "coin": self.coin,
                            "interval": "1m"
                        }
                    });
                    let _ = write
                        .send(Message::Text(candle_sub.to_string().into()))
                        .await;

                    while let Some(msg_res) = read.next().await {
                        match msg_res {
                            Ok(Message::Text(text)) => {
                                self.handle_incoming_text(&text, &tx).await;
                            }
                            Ok(Message::Ping(payload)) => {
                                let _ = write.send(Message::Pong(payload)).await;
                            }
                            Ok(Message::Close(frame)) => {
                                warn!("WebSocket server sent close frame: {:?}", frame);
                                break;
                            }
                            Err(e) => {
                                error!("WebSocket read error: {}", e);
                                break;
                            }
                            _ => {}
                        }
                    }

                    let _ = tx
                        .send(MarketFeedEvent::Disconnected("Connection closed".into()))
                        .await;
                }
                Err(e) => {
                    error!(
                        "Failed to connect to WebSocket: {}. Retrying in {:?}",
                        e, backoff
                    );
                    let _ = tx.send(MarketFeedEvent::Disconnected(e.to_string())).await;
                }
            }

            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(max_backoff);
        }
    }

    async fn handle_incoming_text(&self, text: &str, tx: &mpsc::Sender<MarketFeedEvent>) {
        let parsed: serde_json::Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return,
        };

        let channel = match parsed["channel"].as_str() {
            Some(c) => c,
            None => return,
        };

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        if channel == "activeAssetCtx" {
            let data = &parsed["data"];
            let coin = data["coin"].as_str().unwrap_or(&self.coin).to_string();

            // Extract mark price from ctx object
            let mark_px_str = data["ctx"]["markPx"].as_str();
            if let Some(px_str) = mark_px_str {
                if let Ok(dec_px) = Decimal::from_str(px_str) {
                    if let Ok(price) = Price::new(dec_px) {
                        let _ = tx
                            .send(MarketFeedEvent::MarkPriceUpdate {
                                coin,
                                mark_price: price,
                                timestamp_ms: now_ms,
                            })
                            .await;
                    }
                }
            }
        } else if channel == "candle" {
            let data = &parsed["data"];
            if let Some(snapshot) = Self::parse_candle_item(data, &self.coin) {
                let _ = tx.send(MarketFeedEvent::CandleUpdate(snapshot)).await;
            }
        } else if channel == "userFundings" {
            let data = &parsed["data"];
            if let Some(arr) = data.as_array() {
                for item in arr {
                    if let Some(record) = Self::parse_funding_item(item) {
                        let _ = tx.send(MarketFeedEvent::FundingUpdate(record)).await;
                    }
                }
            } else if let Some(record) = Self::parse_funding_item(data) {
                let _ = tx.send(MarketFeedEvent::FundingUpdate(record)).await;
            }
        }
    }

    fn parse_candle_item(c: &serde_json::Value, default_coin: &str) -> Option<CandleSnapshot> {
        let open_time_ms = c["t"].as_u64()?;
        let close_time_ms = c["T"].as_u64()?;
        let coin = c["s"].as_str().unwrap_or(default_coin).to_string();
        let interval = c["i"].as_str().unwrap_or("1m").to_string();
        let open = Price::new(c["o"].as_str()?.parse::<Decimal>().ok()?).ok()?;
        let high = Price::new(c["h"].as_str()?.parse::<Decimal>().ok()?).ok()?;
        let low = Price::new(c["l"].as_str()?.parse::<Decimal>().ok()?).ok()?;
        let close = Price::new(c["c"].as_str()?.parse::<Decimal>().ok()?).ok()?;
        let volume = c["v"].as_str()?.parse::<Decimal>().ok()?;
        let trade_count = c["n"].as_u64().unwrap_or(0);

        Some(CandleSnapshot {
            open_time_ms,
            close_time_ms,
            coin,
            interval,
            open,
            high,
            low,
            close,
            volume,
            trade_count,
        })
    }

    fn parse_funding_item(item: &serde_json::Value) -> Option<FundingRecord> {
        let delta = &item["delta"];
        let coin = delta["coin"].as_str().unwrap_or("BTC").to_string();
        let rate_str = delta["fundingRate"].as_str()?;
        let rate = Decimal::from_str(rate_str).ok()?;
        let szi_str = delta["szi"].as_str()?;
        let szi_dec = Decimal::from_str(szi_str).ok()?.abs();
        let payment_str = delta["usdc"].as_str()?;
        let payment_dec = Decimal::from_str(payment_str).ok()?;

        let time_ms = item["time"].as_i64().unwrap_or(0);
        let timestamp = DateTime::from_timestamp_millis(time_ms).unwrap_or_else(Utc::now);

        Some(FundingRecord {
            timestamp,
            coin,
            rate,
            position_size: Quantity::new(szi_dec).unwrap_or(Quantity::zero()),
            payment: SignedUsd::new(payment_dec),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_parse_candle_item_valid() {
        let json_val = serde_json::json!({
            "t": 1700000000000u64,
            "T": 1700000059999u64,
            "s": "BTC",
            "i": "1m",
            "o": "65000.0",
            "c": "65100.0",
            "h": "65200.0",
            "l": "64900.0",
            "v": "1.5",
            "n": 10
        });

        let candle =
            HyperliquidWsFeed::parse_candle_item(&json_val, "BTC").expect("Failed to parse candle");
        assert_eq!(candle.open_time_ms, 1700000000000);
        assert_eq!(candle.close_time_ms, 1700000059999);
        assert_eq!(candle.coin, "BTC");
        assert_eq!(candle.interval, "1m");
        assert_eq!(candle.open.value(), dec!(65000.0));
        assert_eq!(candle.close.value(), dec!(65100.0));
        assert_eq!(candle.high.value(), dec!(65200.0));
        assert_eq!(candle.low.value(), dec!(64900.0));
        assert_eq!(candle.volume, dec!(1.5));
        assert_eq!(candle.trade_count, 10);
    }

    #[test]
    fn test_parse_candle_item_malformed() {
        let json_val = serde_json::json!({
            "t": 1700000000000u64,
            // missing high, low, close
            "o": "65000.0"
        });

        assert!(HyperliquidWsFeed::parse_candle_item(&json_val, "BTC").is_none());
    }
}
