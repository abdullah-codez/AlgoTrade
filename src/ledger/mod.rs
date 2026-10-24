//! Direction-aware Fill and Funding Ledgers with FIFO matching and real-time cost attribution.

pub mod capital;
pub use capital::*;

use crate::types::{Direction, Price, Quantity, Side, SignedUsd};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum LedgerError {
    #[error("Unsupported fee token: '{0}'. Only USD/USDC are supported on Hyperliquid perps")]
    UnsupportedFeeToken(String),
    #[error("Zero or negative price in fill record: {0}")]
    InvalidFillPrice(Decimal),
    #[error("Zero quantity in fill record")]
    ZeroFillQuantity,
    #[error("Cannot match fill: insufficient opposite open quantity")]
    InsufficientOpenQuantity,
    #[error("Mismatched side for closing fill")]
    MismatchedSide,
}

/// An immutable record of an individual order fill reported by the exchange or mock.
/// Parameters for constructing a new validated FillRecord.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewFillParams {
    pub fill_id: String,
    pub cloid: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub direction: Direction,
    pub side: Side,
    pub price: Price,
    pub quantity: Quantity,
    pub fee_paid: SignedUsd,
    pub fee_token: String,
    pub is_entry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillRecord {
    pub fill_id: String,
    pub cloid: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub direction: Direction,
    pub side: Side,
    pub price: Price,
    pub quantity: Quantity,
    pub fee_paid: SignedUsd,
    pub fee_token: String,
    pub is_entry: bool,
}

impl FillRecord {
    pub fn new(params: NewFillParams) -> Result<Self, LedgerError> {
        let token_upper = params.fee_token.to_uppercase();
        if token_upper != "USDC" && token_upper != "USD" {
            return Err(LedgerError::UnsupportedFeeToken(params.fee_token));
        }
        if params.quantity.is_zero() {
            return Err(LedgerError::ZeroFillQuantity);
        }
        Ok(Self {
            fill_id: params.fill_id,
            cloid: params.cloid,
            timestamp: params.timestamp,
            direction: params.direction,
            side: params.side,
            price: params.price,
            quantity: params.quantity,
            fee_paid: params.fee_paid,
            fee_token: params.fee_token,
            is_entry: params.is_entry,
        })
    }
}

/// An immutable record of a funding payment event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FundingRecord {
    pub timestamp: DateTime<Utc>,
    pub coin: String,
    pub rate: Decimal,
    pub position_size: Quantity,
    /// Cash flow: positive = cost paid (cash outflow), negative = income received (cash inflow).
    pub payment: SignedUsd,
}

/// An execution slippage measurement comparing actual fill price to reference/trigger price.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlippageRecord {
    pub timestamp: DateTime<Utc>,
    pub cloid: Option<String>,
    pub expected_price: Price,
    pub actual_price: Price,
    pub quantity: Quantity,
    pub slippage_usd: SignedUsd,
}

/// A matched closed trade combining matched entry and exit lots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchedTrade {
    pub direction: Direction,
    pub entry_price: Price,
    pub exit_price: Price,
    pub quantity: Quantity,
    pub entry_fee: SignedUsd,
    pub exit_fee: SignedUsd,
    pub attributed_funding: SignedUsd,
    pub gross_pnl: SignedUsd,
    pub net_pnl: SignedUsd,
}

/// In-memory FIFO Fill Ledger tracking active open lots and completed matched trades.
#[derive(Debug, Default, Clone)]
pub struct FillLedger {
    open_lots: VecDeque<FillRecord>,
    closed_trades: Vec<MatchedTrade>,
    slippage_log: Vec<SlippageRecord>,
}

impl FillLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an entry fill to the open lots queue.
    pub fn add_entry_fill(&mut self, fill: FillRecord) {
        self.open_lots.push_back(fill);
    }

    /// Match an exit fill against open lots using FIFO accounting.
    /// Uses direction-aware formula: (exit_px - entry_px) * qty * direction_multiplier.
    pub fn match_exit_fill(
        &mut self,
        exit_fill: &FillRecord,
        attributed_funding: SignedUsd,
    ) -> Result<Vec<MatchedTrade>, LedgerError> {
        let mut matched = Vec::new();
        let mut remaining_exit_qty = exit_fill.quantity.value();

        while remaining_exit_qty > Decimal::ZERO {
            let Some(mut open_lot) = self.open_lots.pop_front() else {
                return Err(LedgerError::InsufficientOpenQuantity);
            };

            let open_lot_qty = open_lot.quantity.value();
            let matched_qty = remaining_exit_qty.min(open_lot_qty);

            // Pro-rate entry fee for the matched portion
            let entry_fee_portion = if open_lot_qty > Decimal::ZERO {
                SignedUsd::new(open_lot.fee_paid.value() * (matched_qty / open_lot_qty))
            } else {
                SignedUsd::zero()
            };

            // Pro-rate exit fee for the matched portion
            let exit_fee_portion = if exit_fill.quantity.value() > Decimal::ZERO {
                SignedUsd::new(
                    exit_fill.fee_paid.value() * (matched_qty / exit_fill.quantity.value()),
                )
            } else {
                SignedUsd::zero()
            };

            // Direction-aware Gross P&L: (P_exit - P_entry) * Q * D
            let dir_multiplier = open_lot.direction.multiplier();
            let price_diff = exit_fill.price.value() - open_lot.price.value();
            let gross_val = price_diff * matched_qty * dir_multiplier;
            let gross_pnl = SignedUsd::new(gross_val);

            // Net P&L = Gross P&L - Entry Fee - Exit Fee + Signed Attributed Funding Cash Flow
            // (positive attributed_funding = income received; negative = cost paid)
            let net_val = gross_val - entry_fee_portion.value() - exit_fee_portion.value()
                + attributed_funding.value();
            let net_pnl = SignedUsd::new(net_val);

            let trade = MatchedTrade {
                direction: open_lot.direction,
                entry_price: open_lot.price,
                exit_price: exit_fill.price,
                quantity: Quantity::new(matched_qty).unwrap(),
                entry_fee: entry_fee_portion,
                exit_fee: exit_fee_portion,
                attributed_funding,
                gross_pnl,
                net_pnl,
            };

            matched.push(trade.clone());
            self.closed_trades.push(trade);

            remaining_exit_qty -= matched_qty;

            // If the open lot was partially filled, push the remainder back to the front
            let lot_remainder = open_lot_qty - matched_qty;
            if lot_remainder > Decimal::ZERO {
                open_lot.quantity = Quantity::new(lot_remainder).unwrap();
                open_lot.fee_paid =
                    SignedUsd::new(open_lot.fee_paid.value() - entry_fee_portion.value());
                self.open_lots.push_front(open_lot);
            }
        }

        Ok(matched)
    }

    /// Record execution slippage for quality reporting.
    pub fn record_slippage(
        &mut self,
        timestamp: DateTime<Utc>,
        cloid: Option<String>,
        expected: Price,
        actual: Price,
        qty: Quantity,
    ) {
        let diff = (actual.value() - expected.value()).abs() * qty.value();
        self.slippage_log.push(SlippageRecord {
            timestamp,
            cloid,
            expected_price: expected,
            actual_price: actual,
            quantity: qty,
            slippage_usd: SignedUsd::new(diff),
        });
    }

    pub fn total_realized_net_pnl(&self) -> SignedUsd {
        self.closed_trades
            .iter()
            .map(|t| t.net_pnl)
            .fold(SignedUsd::zero(), |acc, p| acc + p)
    }

    pub fn total_realized_gross_pnl(&self) -> SignedUsd {
        self.closed_trades
            .iter()
            .map(|t| t.gross_pnl)
            .fold(SignedUsd::zero(), |acc, p| acc + p)
    }

    pub fn total_fees_paid(&self) -> SignedUsd {
        self.closed_trades
            .iter()
            .map(|t| t.entry_fee + t.exit_fee)
            .fold(SignedUsd::zero(), |acc, f| acc + f)
    }

    pub fn closed_trades(&self) -> &[MatchedTrade] {
        &self.closed_trades
    }

    pub fn open_lots(&self) -> &VecDeque<FillRecord> {
        &self.open_lots
    }

    pub fn current_open_quantity(&self) -> Quantity {
        let total = self
            .open_lots
            .iter()
            .map(|l| l.quantity.value())
            .sum::<Decimal>();
        Quantity::new(total).unwrap_or_else(|_| Quantity::zero())
    }
}

/// Ledger tracking real-time funding payments received/paid during the session.
#[derive(Debug, Default, Clone)]
pub struct FundingLedger {
    records: Vec<FundingRecord>,
}

impl FundingLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a funding payment record and update real-time session impact.
    pub fn add_funding_record(&mut self, record: FundingRecord) {
        self.records.push(record);
    }

    /// Total signed funding cash flow across session:
    /// Positive = net income received (cash inflow), Negative = net cost paid (cash outflow).
    pub fn total_signed_funding_cash_flow(&self) -> SignedUsd {
        self.records
            .iter()
            .map(|r| r.payment)
            .fold(SignedUsd::zero(), |acc, p| acc + p)
    }

    /// Net funding cost across session (positive = net cost paid, negative = net income received).
    /// Net Cost = -SignedCashFlow.
    pub fn total_net_funding_cost(&self) -> SignedUsd {
        SignedUsd::new(-self.total_signed_funding_cash_flow().value())
    }

    pub fn records(&self) -> &[FundingRecord] {
        &self.records
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_direction_aware_long_profit() {
        let mut ledger = FillLedger::new();
        let now = Utc::now();

        // Long entry: 0.00057 BTC at $70,000, fee $0.017955
        let entry = FillRecord::new(NewFillParams {
            fill_id: "fill-1".into(),
            cloid: Some("cloid-1".into()),
            timestamp: now,
            direction: Direction::Long,
            side: Side::Buy,
            price: Price::new(dec!(70000.0)).unwrap(),
            quantity: Quantity::new(dec!(0.00057)).unwrap(),
            fee_paid: SignedUsd::new(dec!(0.017955)),
            fee_token: "USDC".into(),
            is_entry: true,
        })
        .unwrap();
        ledger.add_entry_fill(entry);

        // Long exit at 1% TP: $70,700.00 (+1% move), fee $0.018135
        let exit = FillRecord::new(NewFillParams {
            fill_id: "fill-2".into(),
            cloid: Some("cloid-2".into()),
            timestamp: now,
            direction: Direction::Long,
            side: Side::Sell,
            price: Price::new(dec!(70700.0)).unwrap(),
            quantity: Quantity::new(dec!(0.00057)).unwrap(),
            fee_paid: SignedUsd::new(dec!(0.018135)),
            fee_token: "USDC".into(),
            is_entry: false,
        })
        .unwrap();

        let matched = ledger.match_exit_fill(&exit, SignedUsd::zero()).unwrap();
        assert_eq!(matched.len(), 1);
        let trade = &matched[0];

        // Gross P&L = (70700 - 70000) * 0.00057 * (+1) = 700 * 0.00057 = $0.3990
        assert_eq!(trade.gross_pnl.value(), dec!(0.3990));
        // Fees = 0.017955 + 0.018135 = 0.03609
        // Net P&L = 0.3990 - 0.03609 = 0.36291
        assert_eq!(trade.net_pnl.value(), dec!(0.36291));
    }

    #[test]
    fn test_direction_aware_short_profit() {
        let mut ledger = FillLedger::new();
        let now = Utc::now();

        // Short entry: 0.00057 BTC at $70,000, fee $0.018
        let entry = FillRecord::new(NewFillParams {
            fill_id: "fill-1".into(),
            cloid: Some("cloid-1".into()),
            timestamp: now,
            direction: Direction::Short,
            side: Side::Sell,
            price: Price::new(dec!(70000.0)).unwrap(),
            quantity: Quantity::new(dec!(0.00057)).unwrap(),
            fee_paid: SignedUsd::new(dec!(0.018)),
            fee_token: "USDC".into(),
            is_entry: true,
        })
        .unwrap();
        ledger.add_entry_fill(entry);

        // Short exit at 1% TP: $69,300.00 (-1% move), fee $0.018
        let exit = FillRecord::new(NewFillParams {
            fill_id: "fill-2".into(),
            cloid: Some("cloid-2".into()),
            timestamp: now,
            direction: Direction::Short,
            side: Side::Buy,
            price: Price::new(dec!(69300.0)).unwrap(),
            quantity: Quantity::new(dec!(0.00057)).unwrap(),
            fee_paid: SignedUsd::new(dec!(0.018)),
            fee_token: "USDC".into(),
            is_entry: false,
        })
        .unwrap();

        let matched = ledger.match_exit_fill(&exit, SignedUsd::zero()).unwrap();
        assert_eq!(matched.len(), 1);
        let trade = &matched[0];

        // Gross P&L = (69300 - 70000) * 0.00057 * (-1) = -700 * 0.00057 * (-1) = +$0.3990
        assert_eq!(trade.gross_pnl.value(), dec!(0.3990));
        // Net P&L = 0.3990 - 0.036 = $0.3630
        assert_eq!(trade.net_pnl.value(), dec!(0.3630));
    }

    #[test]
    fn test_direction_aware_short_loss() {
        let mut ledger = FillLedger::new();
        let now = Utc::now();

        // Short entry: 0.00057 BTC at $70,000
        let entry = FillRecord::new(NewFillParams {
            fill_id: "fill-1".into(),
            cloid: None,
            timestamp: now,
            direction: Direction::Short,
            side: Side::Sell,
            price: Price::new(dec!(70000.0)).unwrap(),
            quantity: Quantity::new(dec!(0.00057)).unwrap(),
            fee_paid: SignedUsd::new(dec!(0.018)),
            fee_token: "USDC".into(),
            is_entry: true,
        })
        .unwrap();
        ledger.add_entry_fill(entry);

        // Short stop loss at +0.5% adverse price move: $70,350
        let exit = FillRecord::new(NewFillParams {
            fill_id: "fill-2".into(),
            cloid: None,
            timestamp: now,
            direction: Direction::Short,
            side: Side::Buy,
            price: Price::new(dec!(70350.0)).unwrap(),
            quantity: Quantity::new(dec!(0.00057)).unwrap(),
            fee_paid: SignedUsd::new(dec!(0.018)),
            fee_token: "USDC".into(),
            is_entry: false,
        })
        .unwrap();

        let matched = ledger.match_exit_fill(&exit, SignedUsd::zero()).unwrap();
        let trade = &matched[0];

        // Gross P&L = (70350 - 70000) * 0.00057 * (-1) = 350 * 0.00057 * (-1) = -$0.1995
        assert_eq!(trade.gross_pnl.value(), dec!(-0.1995));
        // Net P&L = -0.1995 - 0.036 = -$0.2355
        assert_eq!(trade.net_pnl.value(), dec!(-0.2355));
    }

    #[test]
    fn test_unsupported_fee_token_rejected() {
        let res = FillRecord::new(NewFillParams {
            fill_id: "fill-1".into(),
            cloid: None,
            timestamp: Utc::now(),
            direction: Direction::Long,
            side: Side::Buy,
            price: Price::new(dec!(70000.0)).unwrap(),
            quantity: Quantity::new(dec!(0.001)).unwrap(),
            fee_paid: SignedUsd::zero(),
            fee_token: "ETH".into(),
            is_entry: true,
        });
        assert!(matches!(res, Err(LedgerError::UnsupportedFeeToken(_))));
    }

    #[test]
    fn test_funding_cash_flow_positive_and_negative_impact() {
        let mut ledger = FillLedger::new();
        let now = Utc::now();

        // Entry fill
        let entry = FillRecord::new(NewFillParams {
            fill_id: "fill-f1".into(),
            cloid: None,
            timestamp: now,
            direction: Direction::Long,
            side: Side::Buy,
            price: Price::new(dec!(70000.0)).unwrap(),
            quantity: Quantity::new(dec!(0.00057)).unwrap(),
            fee_paid: SignedUsd::new(dec!(0.018)),
            fee_token: "USDC".into(),
            is_entry: true,
        })
        .unwrap();
        ledger.add_entry_fill(entry);

        // Exit fill at break-even ($70,000)
        let exit = FillRecord::new(NewFillParams {
            fill_id: "fill-f2".into(),
            cloid: None,
            timestamp: now,
            direction: Direction::Long,
            side: Side::Sell,
            price: Price::new(dec!(70000.0)).unwrap(),
            quantity: Quantity::new(dec!(0.00057)).unwrap(),
            fee_paid: SignedUsd::new(dec!(0.018)),
            fee_token: "USDC".into(),
            is_entry: false,
        })
        .unwrap();

        // 1. Case A: Positive funding cash flow (income received = +$0.010)
        // Net P&L = Gross(0) - Fees(0.036) + Funding(+0.010) = -0.026
        let pos_funding = SignedUsd::new(dec!(0.010));
        let matched_a = ledger.match_exit_fill(&exit, pos_funding).unwrap();
        assert_eq!(matched_a[0].gross_pnl.value(), dec!(0.0));
        assert_eq!(matched_a[0].net_pnl.value(), dec!(-0.026));

        // Re-add entry for Case B
        let entry_b = FillRecord::new(NewFillParams {
            fill_id: "fill-f3".into(),
            cloid: None,
            timestamp: now,
            direction: Direction::Long,
            side: Side::Buy,
            price: Price::new(dec!(70000.0)).unwrap(),
            quantity: Quantity::new(dec!(0.00057)).unwrap(),
            fee_paid: SignedUsd::new(dec!(0.018)),
            fee_token: "USDC".into(),
            is_entry: true,
        })
        .unwrap();
        ledger.add_entry_fill(entry_b);

        // 2. Case B: Negative funding cash flow (cost paid = -$0.010)
        // Net P&L = Gross(0) - Fees(0.036) + Funding(-0.010) = -0.046
        let neg_funding = SignedUsd::new(dec!(-0.010));
        let matched_b = ledger.match_exit_fill(&exit, neg_funding).unwrap();
        assert_eq!(matched_b[0].gross_pnl.value(), dec!(0.0));
        assert_eq!(matched_b[0].net_pnl.value(), dec!(-0.046));
    }
}
