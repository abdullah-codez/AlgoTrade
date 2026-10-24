//! Exchange-side continuous protection manager enforcing active reduce-only TP/SL trigger coverage.
//! Implements partial fill protection resizing and emergency flatten fallbacks.

use crate::execution::client::{ExchangeClient, OrderPlacementResult, OrderRequest, OrderTypeSpec};
use crate::strategy::TpSlLevels;
use crate::types::{Direction, Price, Quantity, Side};
use thiserror::Error;
use tracing::{error, info, warn};

#[derive(Error, Debug)]
pub enum ProtectionError {
    #[error("Failed to place protective orders on exchange: {0}")]
    PlacementFailed(String),
    #[error("Emergency close dispatch failed: {0}")]
    EmergencyCloseFailed(String),
}

/// Active exchange-side protective orders currently covering an open position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveProtection {
    pub coin: String,
    pub covered_quantity: Quantity,
    pub direction: Direction,
    pub tp_order_id: Option<u64>,
    pub tp_cloid: String,
    pub sl_order_id: Option<u64>,
    pub sl_cloid: String,
}

/// Manager coordinating exchange-side reduce-only TP/SL trigger orders and partial-fill resizing.
pub struct ProtectionManager {
    coin: String,
    active_protection: Option<ActiveProtection>,
}

impl ProtectionManager {
    pub fn new(coin: impl Into<String>) -> Self {
        Self {
            coin: coin.into(),
            active_protection: None,
        }
    }

    pub fn active_protection(&self) -> Option<&ActiveProtection> {
        self.active_protection.as_ref()
    }

    /// Establish exchange-side protective TP and SL trigger orders for confirmed filled quantity.
    /// Uses partial-fill replacement protocol:
    /// 1. Place replacement trigger orders for full quantity.
    /// 2. Confirm replacement orders on exchange.
    /// 3. Cancel previous smaller protective orders.
    /// 4. If placement fails, dispatch emergency market close.
    pub async fn establish_or_resize_protection(
        &mut self,
        client: &dyn ExchangeClient,
        direction: Direction,
        total_open_qty: Quantity,
        tp_sl: &TpSlLevels,
        tp_cloid: String,
        sl_cloid: String,
    ) -> Result<ActiveProtection, ProtectionError> {
        let closing_side = Side::from_closing_direction(direction);
        let is_buy = closing_side == Side::Buy;

        info!(
            "Establishing exchange-side protection for {} {} (TP: {}, SL: {})",
            total_open_qty, self.coin, tp_sl.take_profit_price, tp_sl.stop_loss_price
        );

        // Build Take-Profit reduce-only trigger order
        let tp_order = OrderRequest {
            coin: self.coin.clone(),
            is_buy,
            size: total_open_qty,
            limit_px: tp_sl.take_profit_price,
            reduce_only: true,
            order_type: OrderTypeSpec::Trigger {
                trigger_px: tp_sl.take_profit_price,
                is_market: true,
                tpsl: "tp".into(),
            },
            cloid: Some(tp_cloid.clone()),
        };

        // Build Stop-Loss reduce-only trigger order
        let sl_order = OrderRequest {
            coin: self.coin.clone(),
            is_buy,
            size: total_open_qty,
            limit_px: tp_sl.stop_loss_price,
            reduce_only: true,
            order_type: OrderTypeSpec::Trigger {
                trigger_px: tp_sl.stop_loss_price,
                is_market: true,
                tpsl: "sl".into(),
            },
            cloid: Some(sl_cloid.clone()),
        };

        // Submit both protective orders
        let results = match client.place_orders(vec![tp_order, sl_order]).await {
            Ok(res) => res,
            Err(e) => {
                error!("Failed to place protective TP/SL trigger orders: {}", e);
                self.trigger_emergency_flatten(client, direction, total_open_qty)
                    .await?;
                return Err(ProtectionError::PlacementFailed(e.to_string()));
            }
        };

        let tp_res = results.first();
        let sl_res = results.get(1);

        if !Self::is_order_accepted(tp_res) || !Self::is_order_accepted(sl_res) {
            let err_msg = format!(
                "Protective orders rejected by exchange: TP={:?}, SL={:?}",
                tp_res, sl_res
            );
            error!("{}", err_msg);
            self.trigger_emergency_flatten(client, direction, total_open_qty)
                .await?;
            return Err(ProtectionError::PlacementFailed(err_msg));
        }

        // Cancel previous smaller protective orders if this was a partial fill resize
        if let Some(prior) = self.active_protection.take() {
            if let Some(oid) = prior.tp_order_id {
                let _ = client.cancel_order(&self.coin, oid).await;
            } else {
                let _ = client.cancel_by_cloid(&self.coin, &prior.tp_cloid).await;
            }

            if let Some(oid) = prior.sl_order_id {
                let _ = client.cancel_order(&self.coin, oid).await;
            } else {
                let _ = client.cancel_by_cloid(&self.coin, &prior.sl_cloid).await;
            }
        }

        let new_protection = ActiveProtection {
            coin: self.coin.clone(),
            covered_quantity: total_open_qty,
            direction,
            tp_order_id: tp_res.and_then(|r| r.oid),
            tp_cloid,
            sl_order_id: sl_res.and_then(|r| r.oid),
            sl_cloid,
        };

        self.active_protection = Some(new_protection.clone());
        info!("Exchange-side protection successfully verified and active");
        Ok(new_protection)
    }

    /// Cancel orphaned sibling orders once a position has closed.
    pub async fn cancel_all_protection(&mut self, client: &dyn ExchangeClient) {
        if let Some(prior) = self.active_protection.take() {
            info!("Canceling active protective orders following position close");
            if let Some(oid) = prior.tp_order_id {
                let _ = client.cancel_order(&self.coin, oid).await;
            } else {
                let _ = client.cancel_by_cloid(&self.coin, &prior.tp_cloid).await;
            }

            if let Some(oid) = prior.sl_order_id {
                let _ = client.cancel_order(&self.coin, oid).await;
            } else {
                let _ = client.cancel_by_cloid(&self.coin, &prior.sl_cloid).await;
            }
        }
    }

    /// Emergency flatten fallback: dispatches a reduce-only market close when protection cannot be verified.
    async fn trigger_emergency_flatten(
        &self,
        client: &dyn ExchangeClient,
        direction: Direction,
        qty: Quantity,
    ) -> Result<(), ProtectionError> {
        warn!(
            "TRIGGERING EMERGENCY FLATTEN: Attempting reduce-only market exit for {} {}",
            qty, self.coin
        );
        let closing_side = Side::from_closing_direction(direction);
        let close_order = OrderRequest {
            coin: self.coin.clone(),
            is_buy: closing_side == Side::Buy,
            size: qty,
            limit_px: Price::new(rust_decimal_macros::dec!(0.0)).unwrap(),
            reduce_only: true,
            order_type: OrderTypeSpec::Market,
            cloid: Some("emergency-flatten".into()),
        };

        client
            .place_orders(vec![close_order])
            .await
            .map_err(|e| ProtectionError::EmergencyCloseFailed(e.to_string()))?;

        Ok(())
    }

    fn is_order_accepted(res: Option<&OrderPlacementResult>) -> bool {
        match res {
            Some(r) => r.status == "resting" || r.status == "filled",
            None => false,
        }
    }
}
