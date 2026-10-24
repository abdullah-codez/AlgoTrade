//! Pure strategy rules: TP/SL price triggers, directional trailing stops, and reversal logic.

use crate::types::{Direction, Price};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TpSlLevels {
    pub take_profit_price: Price,
    pub stop_loss_price: Price,
}

/// Calculate exact TP and SL trigger price levels for Long and Short positions based on percentage price movements.
pub fn calculate_tp_sl_prices(
    entry_price: Price,
    direction: Direction,
    tp_pct: Decimal,
    sl_pct: Decimal,
) -> TpSlLevels {
    let entry = entry_price.value();
    match direction {
        Direction::Long => {
            let tp = entry * (Decimal::ONE + tp_pct);
            let sl = entry * (Decimal::ONE - sl_pct);
            TpSlLevels {
                take_profit_price: Price::new(tp).unwrap_or(entry_price),
                stop_loss_price: Price::new(sl).unwrap_or(entry_price),
            }
        }
        Direction::Short => {
            let tp = entry * (Decimal::ONE - tp_pct);
            let sl = entry * (Decimal::ONE + sl_pct);
            TpSlLevels {
                take_profit_price: Price::new(tp).unwrap_or(entry_price),
                stop_loss_price: Price::new(sl).unwrap_or(entry_price),
            }
        }
    }
}

/// Directional Trailing Stop Calculator:
/// - Long: Activates when mark_price >= entry * (1 + trigger_pct). Tracks high-water mark; trails 0.5% below it.
/// - Short: Activates when mark_price <= entry * (1 - trigger_pct). Tracks low-water mark; trails 0.5% above it.
pub fn calculate_trailing_stop_price(
    entry_price: Price,
    direction: Direction,
    extreme_price: Price, // High-water mark for Long, Low-water mark for Short
    trigger_pct: Decimal,
    trail_pct: Decimal,
) -> Option<Price> {
    let entry = entry_price.value();
    let extreme = extreme_price.value();

    match direction {
        Direction::Long => {
            let activation_threshold = entry * (Decimal::ONE + trigger_pct);
            if extreme >= activation_threshold {
                let trail_stop = extreme * (Decimal::ONE - trail_pct);
                Some(Price::new(trail_stop).unwrap_or(entry_price))
            } else {
                None
            }
        }
        Direction::Short => {
            let activation_threshold = entry * (Decimal::ONE - trigger_pct);
            if extreme <= activation_threshold {
                let trail_stop = extreme * (Decimal::ONE + trail_pct);
                Some(Price::new(trail_stop).unwrap_or(entry_price))
            } else {
                None
            }
        }
    }
}

/// Determine next trade direction based on exit outcome:
/// - After a Take-Profit exit: Re-enter in the SAME direction.
/// - After a Stop-Loss exit: REVERSE direction.
pub fn determine_next_direction(
    last_exit_was_tp: bool,
    current_direction: Direction,
) -> (Direction, &'static str) {
    if last_exit_was_tp {
        (
            current_direction,
            "Same-direction re-entry following take-profit",
        )
    } else {
        (
            current_direction.opposite(),
            "Reversal to opposite direction following stop-loss",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_long_tp_sl_calculation() {
        let entry = Price::new(dec!(70000.0)).unwrap();
        // 1.0% TP, 0.5% SL
        let levels = calculate_tp_sl_prices(entry, Direction::Long, dec!(0.01), dec!(0.005));
        assert_eq!(levels.take_profit_price.value(), dec!(70700.0));
        assert_eq!(levels.stop_loss_price.value(), dec!(69650.0));
    }

    #[test]
    fn test_short_tp_sl_calculation() {
        let entry = Price::new(dec!(70000.0)).unwrap();
        // 1.0% TP, 0.5% SL
        let levels = calculate_tp_sl_prices(entry, Direction::Short, dec!(0.01), dec!(0.005));
        assert_eq!(levels.take_profit_price.value(), dec!(69300.0));
        assert_eq!(levels.stop_loss_price.value(), dec!(70350.0));
    }

    #[test]
    fn test_long_trailing_stop_activation() {
        let entry = Price::new(dec!(70000.0)).unwrap();
        // Trigger: +0.75% = $70,525. Trail: 0.5%
        // Case 1: HWM below trigger ($70,400) -> None
        let stop1 = calculate_trailing_stop_price(
            entry,
            Direction::Long,
            Price::new(dec!(70400.0)).unwrap(),
            dec!(0.0075),
            dec!(0.005),
        );
        assert!(stop1.is_none());

        // Case 2: HWM reaches $71,000 (+1.428% > 0.75%) -> Trail stop = 71,000 * 0.995 = $70,645.0
        let stop2 = calculate_trailing_stop_price(
            entry,
            Direction::Long,
            Price::new(dec!(71000.0)).unwrap(),
            dec!(0.0075),
            dec!(0.005),
        );
        assert_eq!(stop2.unwrap().value(), dec!(70645.0));
    }

    #[test]
    fn test_short_trailing_stop_activation() {
        let entry = Price::new(dec!(70000.0)).unwrap();
        // Trigger: -0.75% favorable move = $69,475. Trail: 0.5%
        // Case 1: LWM not yet at trigger ($69,600) -> None
        let stop1 = calculate_trailing_stop_price(
            entry,
            Direction::Short,
            Price::new(dec!(69600.0)).unwrap(),
            dec!(0.0075),
            dec!(0.005),
        );
        assert!(stop1.is_none());

        // Case 2: LWM drops to $69,000 (-1.428% favorable) -> Trail stop = 69,000 * 1.005 = $69,345.0
        let stop2 = calculate_trailing_stop_price(
            entry,
            Direction::Short,
            Price::new(dec!(69000.0)).unwrap(),
            dec!(0.0075),
            dec!(0.005),
        );
        assert_eq!(stop2.unwrap().value(), dec!(69345.0));
    }

    #[test]
    fn test_next_direction_rules() {
        let (next_dir, reason) = determine_next_direction(true, Direction::Long);
        assert_eq!(next_dir, Direction::Long);
        assert!(reason.contains("Same-direction"));

        let (next_dir, reason) = determine_next_direction(false, Direction::Long);
        assert_eq!(next_dir, Direction::Short);
        assert!(reason.contains("Reversal"));
    }
}
