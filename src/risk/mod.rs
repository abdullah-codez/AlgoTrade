//! Pure risk management: position sizing, mark-to-market exposure with exit costs, and loss streak tracking.

use crate::config::MarketMetadata;
use crate::types::{Direction, MarginAllocation, Price, Quantity, SignedUsd};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum RiskError {
    #[error(
        "Calculated order notional ${actual_notional} is below exchange minimum ${min_notional}"
    )]
    OrderBelowMinimumNotional {
        actual_notional: SignedUsd,
        min_notional: SignedUsd,
    },
    #[error("Calculated quantity is zero after rounding to {sz_decimals} decimals")]
    ZeroQuantityAfterRounding { sz_decimals: u32 },
    #[error("Mark price is zero or invalid: {0}")]
    InvalidMarkPrice(Price),
}

/// Sizing result containing calculated contract size and effective initial margin/notional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizingResult {
    pub quantity: Quantity,
    pub notional: SignedUsd,
    pub deployed_margin: SignedUsd,
}

/// Calculate contract position size strictly respecting initial margin cap ($8 max),
/// asset lot precision (szDecimals), and exchange minimum order notional ($10).
pub fn calculate_order_size(
    margin_cap: MarginAllocation,
    leverage: u32,
    mark_price: Price,
    metadata: &MarketMetadata,
    min_notional: SignedUsd,
) -> Result<SizingResult, RiskError> {
    if mark_price.value() <= Decimal::ZERO {
        return Err(RiskError::InvalidMarkPrice(mark_price));
    }

    let max_notional = margin_cap.value() * Decimal::from(leverage);
    let raw_qty = max_notional / mark_price.value();

    // Round down to asset lot decimals (e.g. 5 decimals for BTC)
    let rounded_qty = Quantity::new(raw_qty)
        .map_err(|_| RiskError::InvalidMarkPrice(mark_price))?
        .round_down_dp(metadata.sz_decimals);

    if rounded_qty.is_zero() {
        return Err(RiskError::ZeroQuantityAfterRounding {
            sz_decimals: metadata.sz_decimals,
        });
    }

    let actual_notional_val = rounded_qty.value() * mark_price.value();
    let actual_notional = SignedUsd::new(actual_notional_val);

    if actual_notional_val < min_notional.value() {
        return Err(RiskError::OrderBelowMinimumNotional {
            actual_notional,
            min_notional,
        });
    }

    let deployed_margin = SignedUsd::new(actual_notional_val / Decimal::from(leverage));

    Ok(SizingResult {
        quantity: rounded_qty,
        notional: actual_notional,
        deployed_margin,
    })
}

/// Calculate unrealized mark-to-market Gross P&L for an active position:
/// Unrealized Gross P&L = (current_mark - entry_px) * qty * direction_multiplier.
pub fn calculate_unrealized_gross_pnl(
    entry_price: Price,
    current_mark_price: Price,
    quantity: Quantity,
    direction: Direction,
) -> SignedUsd {
    let diff = current_mark_price.value() - entry_price.value();
    let gross = diff * quantity.value() * direction.multiplier();
    SignedUsd::new(gross)
}

/// Calculate Total Session Mark-to-Market Equity Change including estimated exit costs:
/// MTM Session P&L = Realized Net P&L + Unrealized Gross P&L - Estimated Exit Taker Fee - Estimated Exit Slippage.
pub fn calculate_mtm_session_equity_change(
    realized_net_pnl: SignedUsd,
    unrealized_gross_pnl: SignedUsd,
    open_quantity: Quantity,
    current_mark_price: Price,
    taker_fee_rate: Decimal,
    est_slippage_rate: Decimal,
) -> SignedUsd {
    let current_notional = open_quantity.value() * current_mark_price.value();
    let est_exit_fee = current_notional * taker_fee_rate;
    let est_exit_slippage = current_notional * est_slippage_rate;

    let net_mtm =
        realized_net_pnl.value() + unrealized_gross_pnl.value() - est_exit_fee - est_exit_slippage;
    SignedUsd::new(net_mtm)
}

/// Tracks consecutive closed trades that resulted in a net loss (Net Realized P&L < 0).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsecutiveLossTracker {
    consecutive_losses: u32,
    max_consecutive_losses_observed: u32,
}

impl ConsecutiveLossTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_streak(streak: u32) -> Self {
        Self {
            consecutive_losses: streak,
            max_consecutive_losses_observed: streak,
        }
    }

    /// Record a closed trade result:
    /// - If net_pnl < 0: increment consecutive loss streak.
    /// - If net_pnl >= 0: reset streak to 0.
    pub fn record_closed_trade(&mut self, net_pnl: SignedUsd) -> u32 {
        if net_pnl.is_negative() {
            self.consecutive_losses += 1;
            if self.consecutive_losses > self.max_consecutive_losses_observed {
                self.max_consecutive_losses_observed = self.consecutive_losses;
            }
        } else {
            self.consecutive_losses = 0;
        }
        self.consecutive_losses
    }

    pub fn current_streak(&self) -> u32 {
        self.consecutive_losses
    }

    pub fn max_streak_observed(&self) -> u32 {
        self.max_consecutive_losses_observed
    }

    pub fn has_breached_limit(&self, limit: u32) -> bool {
        self.consecutive_losses >= limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_normal_sizing_at_70k() {
        let margin_cap = MarginAllocation::new(dec!(8.00)).unwrap();
        let leverage = 5;
        let mark_price = Price::new(dec!(70000.0)).unwrap();
        let metadata = MarketMetadata::btc_fixture(); // 5 decimals
        let min_notional = SignedUsd::new(dec!(10.00));

        let sizing =
            calculate_order_size(margin_cap, leverage, mark_price, &metadata, min_notional)
                .unwrap();
        // Max notional = $40.00. 40 / 70000 = 0.000571428...
        // Rounded down to 5 decimals = 0.00057 BTC
        assert_eq!(sizing.quantity.value(), dec!(0.00057));
        // Actual notional = 0.00057 * 70,000 = $39.90
        assert_eq!(sizing.notional.value(), dec!(39.90));
        // Deployed margin = 39.90 / 5 = $7.98
        assert_eq!(sizing.deployed_margin.value(), dec!(7.98));
    }

    #[test]
    fn test_greedy_sizing_at_70k() {
        let margin_cap = MarginAllocation::new(dec!(8.00)).unwrap();
        let leverage = 10;
        let mark_price = Price::new(dec!(70000.0)).unwrap();
        let metadata = MarketMetadata::btc_fixture();
        let min_notional = SignedUsd::new(dec!(10.00));

        let sizing =
            calculate_order_size(margin_cap, leverage, mark_price, &metadata, min_notional)
                .unwrap();
        // Max notional = $80.00. 80 / 70000 = 0.0011428... -> 0.00114 BTC
        assert_eq!(sizing.quantity.value(), dec!(0.00114));
        assert_eq!(sizing.notional.value(), dec!(79.80));
        assert_eq!(sizing.deployed_margin.value(), dec!(7.98));
    }

    #[test]
    fn test_sub_ten_dollar_order_rejected() {
        let margin_cap = MarginAllocation::new(dec!(1.00)).unwrap(); // $1 margin
        let leverage = 5; // $5 max notional < $10 exchange min
        let mark_price = Price::new(dec!(70000.0)).unwrap();
        let metadata = MarketMetadata::btc_fixture();
        let min_notional = SignedUsd::new(dec!(10.00));

        let res = calculate_order_size(margin_cap, leverage, mark_price, &metadata, min_notional);
        assert!(matches!(
            res,
            Err(RiskError::OrderBelowMinimumNotional { .. })
        ));
    }

    #[test]
    fn test_unrealized_gross_pnl_direction() {
        let entry = Price::new(dec!(70000.0)).unwrap();
        let qty = Quantity::new(dec!(0.00057)).unwrap();

        // Long position at 70,700: +$0.399
        let pnl_long = calculate_unrealized_gross_pnl(
            entry,
            Price::new(dec!(70700.0)).unwrap(),
            qty,
            Direction::Long,
        );
        assert_eq!(pnl_long.value(), dec!(0.3990));

        // Short position at 70,700: -$0.399
        let pnl_short = calculate_unrealized_gross_pnl(
            entry,
            Price::new(dec!(70700.0)).unwrap(),
            qty,
            Direction::Short,
        );
        assert_eq!(pnl_short.value(), dec!(-0.3990));
    }

    #[test]
    fn test_consecutive_loss_tracker() {
        let mut tracker = ConsecutiveLossTracker::new();
        assert_eq!(tracker.current_streak(), 0);

        // Loss 1
        tracker.record_closed_trade(SignedUsd::new(dec!(-0.25)));
        assert_eq!(tracker.current_streak(), 1);
        assert!(!tracker.has_breached_limit(2));

        // Loss 2
        tracker.record_closed_trade(SignedUsd::new(dec!(-0.30)));
        assert_eq!(tracker.current_streak(), 2);
        assert!(tracker.has_breached_limit(2));

        // Profit resets
        tracker.record_closed_trade(SignedUsd::new(dec!(0.40)));
        assert_eq!(tracker.current_streak(), 0);
        assert_eq!(tracker.max_streak_observed(), 2);
    }
}
