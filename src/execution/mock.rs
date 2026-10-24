//! In-memory mock exchange implementing `ExchangeClient` for deterministic testnet simulation.
//! Supports configurable failure injection: network drops, rate limits (HTTP 429),
//! partial fills (e.g. 40% then 60%), 10% trigger slippage, and lost responses.

use crate::config::MarketMetadata;
use crate::execution::client::{
    CandleSnapshot, ExchangeClient, ExchangeOpenOrder, ExecutionError, OrderPlacementResult,
    OrderRequest, OrderTypeSpec, PerpAccountSummary, PositionSummary, SignerStatus,
    UserStateSummary,
};
use crate::ledger::FundingRecord;
use crate::types::{Price, Quantity, Side, SignedUsd};
use async_trait::async_trait;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockFillMode {
    /// Orders remain resting in the order book.
    Resting,
    /// Non-trigger orders fill immediately at limit price.
    ImmediateFull,
    /// Fills a fraction first (e.g. 0.4), leaving the rest resting.
    PartialFill { first_ratio: Decimal },
}

#[derive(Debug, Clone)]
pub struct MockExchangeState {
    pub account_value: SignedUsd,
    pub total_margin_used: SignedUsd,
    pub withdrawable: SignedUsd,
    pub positions: HashMap<String, PositionSummary>,
    pub open_orders: Vec<ExchangeOpenOrder>,
    pub funding_history: Vec<FundingRecord>,
    pub leverage: (bool, u32), // (is_cross, leverage)
    pub current_mark_price: Option<Price>,
}

/// Thread-safe mock exchange client for deterministic integration testing.
#[derive(Clone)]
pub struct MockExchangeClient {
    state: Arc<Mutex<MockExchangeState>>,
    metadata: MarketMetadata,
    simulate_network_error: Arc<AtomicBool>,
    simulate_rate_limit: Arc<AtomicBool>,
    simulate_order_rejection: Arc<AtomicBool>,
    simulate_lost_response: Arc<AtomicBool>,
    simulate_leverage_failure: Arc<AtomicBool>,
    fill_mode: Arc<Mutex<MockFillMode>>,
    trigger_slippage_pct: Arc<Mutex<Decimal>>, // e.g. 0.10 for 10% slippage
    next_oid: Arc<AtomicU64>,
    signer_status: Arc<Mutex<SignerStatus>>,
}

impl MockExchangeClient {
    pub fn new(coin: &str, initial_balance: SignedUsd) -> Self {
        let metadata = MarketMetadata {
            coin: coin.to_string(),
            sz_decimals: 5,
            max_leverage: 50,
        };

        let state = MockExchangeState {
            account_value: initial_balance,
            total_margin_used: SignedUsd::zero(),
            withdrawable: initial_balance,
            positions: HashMap::new(),
            open_orders: Vec::new(),
            funding_history: Vec::new(),
            leverage: (true, 5),
            current_mark_price: None,
        };

        Self {
            state: Arc::new(Mutex::new(state)),
            metadata,
            simulate_network_error: Arc::new(AtomicBool::new(false)),
            simulate_rate_limit: Arc::new(AtomicBool::new(false)),
            simulate_order_rejection: Arc::new(AtomicBool::new(false)),
            simulate_lost_response: Arc::new(AtomicBool::new(false)),
            simulate_leverage_failure: Arc::new(AtomicBool::new(false)),
            fill_mode: Arc::new(Mutex::new(MockFillMode::Resting)),
            trigger_slippage_pct: Arc::new(Mutex::new(dec!(0.0))),
            next_oid: Arc::new(AtomicU64::new(1000)),
            signer_status: Arc::new(Mutex::new(SignerStatus::Active)),
        }
    }

    pub async fn set_signer_status(&self, status: SignerStatus) {
        *self.signer_status.lock().await = status;
    }

    pub fn set_network_error(&self, val: bool) {
        self.simulate_network_error.store(val, Ordering::SeqCst);
    }

    pub fn set_rate_limit(&self, val: bool) {
        self.simulate_rate_limit.store(val, Ordering::SeqCst);
    }

    pub fn set_order_rejection(&self, val: bool) {
        self.simulate_order_rejection.store(val, Ordering::SeqCst);
    }

    pub fn set_lost_response(&self, val: bool) {
        self.simulate_lost_response.store(val, Ordering::SeqCst);
    }

    pub fn set_leverage_failure(&self, val: bool) {
        self.simulate_leverage_failure.store(val, Ordering::SeqCst);
    }

    pub async fn set_fill_mode(&self, mode: MockFillMode) {
        let mut m = self.fill_mode.lock().await;
        *m = mode;
    }

    pub async fn set_trigger_slippage_pct(&self, slippage: Decimal) {
        let mut s = self.trigger_slippage_pct.lock().await;
        *s = slippage;
    }

    pub async fn set_position(
        &self,
        coin: &str,
        size: Quantity,
        is_long: bool,
        entry_px: Option<Price>,
    ) {
        let mut state = self.state.lock().await;
        if size.is_zero() {
            state.positions.remove(coin);
        } else {
            state.positions.insert(
                coin.to_string(),
                PositionSummary {
                    coin: coin.to_string(),
                    szi: size,
                    entry_px,
                    is_long,
                    unrealized_pnl: SignedUsd::zero(),
                },
            );
        }
    }

    pub async fn set_mark_price(&self, price: Price) {
        let mut state = self.state.lock().await;
        state.current_mark_price = Some(price);
    }

    pub async fn add_funding_record(&self, record: FundingRecord) {
        let mut state = self.state.lock().await;
        state.funding_history.push(record);
    }

    /// Check and execute any open TP/SL trigger orders against a new mark price.
    /// Returns the list of filled order IDs and fill prices.
    pub async fn evaluate_trigger_orders(
        &self,
        mark_px: Price,
    ) -> Vec<(u64, Price, Quantity, Side)> {
        let mut state = self.state.lock().await;
        state.current_mark_price = Some(mark_px);

        let slippage = *self.trigger_slippage_pct.lock().await;
        let mut triggered = Vec::new();
        let mut remaining_orders = Vec::new();

        for o in state.open_orders.drain(..) {
            if o.is_trigger {
                if let Some(t_px) = o.trigger_px {
                    let is_tp = o.trigger_type.as_deref() == Some("tp");
                    let should_fire = match (o.side, is_tp) {
                        // TP for Long: closing side Sell, triggers when mark_px >= t_px
                        (Side::Sell, true) => mark_px.value() >= t_px.value(),
                        // SL for Long: closing side Sell, triggers when mark_px <= t_px
                        (Side::Sell, false) => mark_px.value() <= t_px.value(),
                        // TP for Short: closing side Buy, triggers when mark_px <= t_px
                        (Side::Buy, true) => mark_px.value() <= t_px.value(),
                        // SL for Short: closing side Buy, triggers when mark_px >= t_px
                        (Side::Buy, false) => mark_px.value() >= t_px.value(),
                    };

                    if should_fire {
                        // Apply simulated slippage
                        let fill_px_dec = if o.side == Side::Buy {
                            // Buying back with upward slippage
                            mark_px.value() * (Decimal::ONE + slippage)
                        } else {
                            // Selling with downward slippage
                            mark_px.value() * (Decimal::ONE - slippage)
                        };
                        let fill_px = Price::new(fill_px_dec).unwrap_or(mark_px);
                        triggered.push((o.oid, fill_px, o.size, o.side));
                        continue;
                    }
                }
            }
            remaining_orders.push(o);
        }

        state.open_orders = remaining_orders;

        // Update position if triggered orders closed or reduced it
        for (_, _fill_px, fill_sz, side) in &triggered {
            if let Some(pos) = state.positions.get_mut(&self.metadata.coin) {
                let closing_match =
                    (pos.is_long && *side == Side::Sell) || (!pos.is_long && *side == Side::Buy);
                if closing_match {
                    let new_sz = if pos.szi.value() > fill_sz.value() {
                        Quantity::new(pos.szi.value() - fill_sz.value())
                            .unwrap_or_else(|_| Quantity::zero())
                    } else {
                        Quantity::zero()
                    };
                    pos.szi = new_sz;
                }
            }
        }

        // Clean up zero-size positions
        state.positions.retain(|_, pos| !pos.szi.is_zero());

        triggered
    }

    fn check_fault_injections(&self) -> Result<(), ExecutionError> {
        if self.simulate_network_error.load(Ordering::SeqCst) {
            return Err(ExecutionError::ApiRejection(
                "Simulated network timeout/drop".to_string(),
            ));
        }
        if self.simulate_rate_limit.load(Ordering::SeqCst) {
            return Err(ExecutionError::ApiRejection(
                "HTTP 429 Too Many Requests: Rate limit exceeded".to_string(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl ExchangeClient for MockExchangeClient {
    async fn get_metadata(&self, coin: &str) -> Result<MarketMetadata, ExecutionError> {
        self.check_fault_injections()?;
        if self.metadata.coin.eq_ignore_ascii_case(coin) {
            Ok(self.metadata.clone())
        } else {
            Err(ExecutionError::DataParsing(format!(
                "Coin {} not found",
                coin
            )))
        }
    }

    async fn get_user_state(&self, _address: &str) -> Result<UserStateSummary, ExecutionError> {
        self.check_fault_injections()?;
        let state = self.state.lock().await;
        Ok(UserStateSummary {
            account_value: state.account_value,
            total_margin_used: state.total_margin_used,
            withdrawable: state.withdrawable,
            positions: state.positions.values().cloned().collect(),
        })
    }

    async fn get_perp_account_summary(
        &self,
        address: &str,
    ) -> Result<PerpAccountSummary, ExecutionError> {
        self.check_fault_injections()?;
        let state = self.state.lock().await;
        let cross_maint = SignedUsd::new(state.total_margin_used.value() * dec!(0.25));
        Ok(PerpAccountSummary {
            account_address: address.to_string(),
            shortened_address: crate::execution::client::shorten_address(address),
            account_value: state.account_value,
            total_margin_used: state.total_margin_used,
            withdrawable: state.withdrawable,
            cross_maintenance_margin_used: cross_maint,
            positions: state.positions.values().cloned().collect(),
            signer_status: *self.signer_status.lock().await,
            exchange_time_ms: 1700000000000,
            source: "Local Mock Engine".to_string(),
            is_mock: true,
        })
    }

    async fn get_candle_snapshot(
        &self,
        coin: &str,
        interval: &str,
        start_time_ms: u64,
        end_time_ms: u64,
    ) -> Result<Vec<CandleSnapshot>, ExecutionError> {
        self.check_fault_injections()?;
        crate::execution::client::validate_candle_interval(interval)?;

        let mut candles = Vec::new();
        let step_ms = match interval {
            "1m" => 60_000,
            "5m" => 300_000,
            "15m" => 900_000,
            "1h" => 3_600_000,
            _ => 60_000,
        };

        let start = if start_time_ms > 0 {
            start_time_ms
        } else {
            1700000000000
        };
        let end = if end_time_ms > start {
            end_time_ms
        } else {
            start + step_ms * 5
        };

        let mut t = start;
        let mut base_px = dec!(80000.0);
        while t < end && candles.len() < 300 {
            let open = Price::new(base_px).unwrap();
            let high = Price::new(base_px + dec!(50.0)).unwrap();
            let low = Price::new(base_px - dec!(30.0)).unwrap();
            let close = Price::new(base_px + dec!(20.0)).unwrap();
            candles.push(CandleSnapshot {
                open_time_ms: t,
                close_time_ms: t + step_ms - 1,
                coin: coin.to_string(),
                interval: interval.to_string(),
                open,
                high,
                low,
                close,
                volume: dec!(1.5),
                trade_count: 42,
            });
            base_px += dec!(10.0);
            t += step_ms;
        }

        Ok(candles)
    }

    async fn get_open_orders(
        &self,
        _address: &str,
    ) -> Result<Vec<ExchangeOpenOrder>, ExecutionError> {
        self.check_fault_injections()?;
        let state = self.state.lock().await;
        Ok(state.open_orders.clone())
    }

    async fn get_user_funding(
        &self,
        _address: &str,
        start_time_ms: u64,
    ) -> Result<Vec<FundingRecord>, ExecutionError> {
        self.check_fault_injections()?;
        let state = self.state.lock().await;
        let filtered = state
            .funding_history
            .iter()
            .filter(|f| f.timestamp.timestamp_millis() as u64 >= start_time_ms)
            .cloned()
            .collect();
        Ok(filtered)
    }

    async fn update_leverage(
        &self,
        _coin: &str,
        is_cross: bool,
        leverage: u32,
    ) -> Result<(), ExecutionError> {
        self.check_fault_injections()?;
        if self.simulate_leverage_failure.load(Ordering::SeqCst) {
            return Err(ExecutionError::ApiRejection(
                "Simulated leverage update failure on exchange".to_string(),
            ));
        }
        let mut state = self.state.lock().await;
        state.leverage = (is_cross, leverage);
        Ok(())
    }

    async fn place_orders(
        &self,
        orders: Vec<OrderRequest>,
    ) -> Result<Vec<OrderPlacementResult>, ExecutionError> {
        self.check_fault_injections()?;

        if self.simulate_order_rejection.load(Ordering::SeqCst) {
            let mut rej = Vec::new();
            for o in orders {
                rej.push(OrderPlacementResult {
                    oid: None,
                    cloid: o.cloid,
                    status: "error: Order rejected by risk limit".into(),
                });
            }
            return Ok(rej);
        }

        let is_lost_response = self.simulate_lost_response.load(Ordering::SeqCst);
        let fill_mode = *self.fill_mode.lock().await;
        let mut results = Vec::new();
        let mut state = self.state.lock().await;

        for o in orders {
            let oid = self.next_oid.fetch_add(1, Ordering::SeqCst);
            let side = if o.is_buy { Side::Buy } else { Side::Sell };

            match o.order_type {
                OrderTypeSpec::Trigger {
                    trigger_px, tpsl, ..
                } => {
                    // Trigger orders rest on exchange until evaluated
                    state.open_orders.push(ExchangeOpenOrder {
                        coin: o.coin.clone(),
                        side,
                        limit_px: o.limit_px,
                        size: o.size,
                        oid,
                        cloid: o.cloid.clone(),
                        is_trigger: true,
                        trigger_px: Some(trigger_px),
                        trigger_type: Some(tpsl),
                    });

                    results.push(OrderPlacementResult {
                        oid: Some(oid),
                        cloid: o.cloid,
                        status: "resting".into(),
                    });
                }
                _ => {
                    // Non-trigger orders (Market / Limit)
                    match fill_mode {
                        MockFillMode::Resting => {
                            state.open_orders.push(ExchangeOpenOrder {
                                coin: o.coin.clone(),
                                side,
                                limit_px: o.limit_px,
                                size: o.size,
                                oid,
                                cloid: o.cloid.clone(),
                                is_trigger: false,
                                trigger_px: None,
                                trigger_type: None,
                            });
                            results.push(OrderPlacementResult {
                                oid: Some(oid),
                                cloid: o.cloid,
                                status: "resting".into(),
                            });
                        }
                        MockFillMode::ImmediateFull => {
                            if o.reduce_only {
                                if let Some(existing) = state.positions.get_mut(&o.coin) {
                                    if o.size.value() >= existing.szi.value() {
                                        state.positions.remove(&o.coin);
                                    } else {
                                        existing.szi =
                                            Quantity::new(existing.szi.value() - o.size.value())
                                                .unwrap_or(Quantity::zero());
                                    }
                                }
                            } else {
                                // Update position immediately
                                let is_long = o.is_buy;
                                let entry_px = Some(o.limit_px);
                                state.positions.insert(
                                    o.coin.clone(),
                                    PositionSummary {
                                        coin: o.coin.clone(),
                                        szi: o.size,
                                        entry_px,
                                        is_long,
                                        unrealized_pnl: SignedUsd::zero(),
                                    },
                                );
                            }
                            results.push(OrderPlacementResult {
                                oid: Some(oid),
                                cloid: o.cloid,
                                status: "filled".into(),
                            });
                        }
                        MockFillMode::PartialFill { first_ratio } => {
                            let filled_sz_dec = o.size.value() * first_ratio;
                            let remaining_sz_dec = o.size.value() - filled_sz_dec;
                            let filled_sz =
                                Quantity::new(filled_sz_dec).unwrap_or(Quantity::zero());
                            let rem_sz =
                                Quantity::new(remaining_sz_dec).unwrap_or(Quantity::zero());

                            // Create partial position
                            let is_long = o.is_buy;
                            let entry_px = Some(o.limit_px);
                            state.positions.insert(
                                o.coin.clone(),
                                PositionSummary {
                                    coin: o.coin.clone(),
                                    szi: filled_sz,
                                    entry_px,
                                    is_long,
                                    unrealized_pnl: SignedUsd::zero(),
                                },
                            );

                            // Keep remainder resting
                            state.open_orders.push(ExchangeOpenOrder {
                                coin: o.coin.clone(),
                                side,
                                limit_px: o.limit_px,
                                size: rem_sz,
                                oid,
                                cloid: o.cloid.clone(),
                                is_trigger: false,
                                trigger_px: None,
                                trigger_type: None,
                            });

                            results.push(OrderPlacementResult {
                                oid: Some(oid),
                                cloid: o.cloid,
                                status: "resting".into(),
                            });
                        }
                    }
                }
            }
        }

        if is_lost_response {
            // Simulated network timeout right after placement: the exchange has the orders,
            // but the caller never received the response!
            return Err(ExecutionError::ApiRejection(
                "Simulated connection dropped during response delivery".to_string(),
            ));
        }

        Ok(results)
    }

    async fn cancel_order(&self, _coin: &str, oid: u64) -> Result<(), ExecutionError> {
        self.check_fault_injections()?;
        let mut state = self.state.lock().await;
        state.open_orders.retain(|o| o.oid != oid);
        Ok(())
    }

    async fn cancel_by_cloid(&self, _coin: &str, cloid: &str) -> Result<(), ExecutionError> {
        self.check_fault_injections()?;
        let mut state = self.state.lock().await;
        state
            .open_orders
            .retain(|o| o.cloid.as_deref() != Some(cloid));
        Ok(())
    }
}
