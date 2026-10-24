//! Strongly typed domain primitives for prices, quantities, monetary values, and trading sides.
//! Enforces exact fixed-point decimal arithmetic via `rust_decimal`.

use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

pub use crate::execution::SignerStatus;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum TypeError {
    #[error("Value must be non-negative: {0}")]
    NegativeValue(Decimal),
    #[error("Value must be strictly positive: {0}")]
    NonPositiveValue(Decimal),
    #[error("Division by zero")]
    DivisionByZero,
}

/// BTC Price in USD. Must be non-negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Price(Decimal);

impl Price {
    pub fn new(val: Decimal) -> Result<Self, TypeError> {
        if val < Decimal::ZERO {
            return Err(TypeError::NegativeValue(val));
        }
        Ok(Self(val))
    }

    pub fn new_strictly_positive(val: Decimal) -> Result<Self, TypeError> {
        if val <= Decimal::ZERO {
            return Err(TypeError::NonPositiveValue(val));
        }
        Ok(Self(val))
    }

    pub fn value(&self) -> Decimal {
        self.0
    }

    pub fn round_dp(&self, dp: u32) -> Self {
        Self(self.0.round_dp(dp))
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// BTC Contract Quantity. Must be non-negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Quantity(Decimal);

impl Quantity {
    pub fn new(val: Decimal) -> Result<Self, TypeError> {
        if val < Decimal::ZERO {
            return Err(TypeError::NegativeValue(val));
        }
        Ok(Self(val))
    }

    pub fn zero() -> Self {
        Self(Decimal::ZERO)
    }

    pub fn is_zero(&self) -> bool {
        self.0 == Decimal::ZERO
    }

    pub fn value(&self) -> Decimal {
        self.0
    }

    pub fn round_down_dp(&self, dp: u32) -> Self {
        Self(self.0.round_dp_with_strategy(dp, RoundingStrategy::ToZero))
    }
}

impl fmt::Display for Quantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Signed monetary amount in USD (used for P&L, balance changes, funding payments).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SignedUsd(Decimal);

impl SignedUsd {
    pub fn new(val: Decimal) -> Self {
        Self(val)
    }

    pub fn zero() -> Self {
        Self(Decimal::ZERO)
    }

    pub fn value(&self) -> Decimal {
        self.0
    }

    pub fn is_positive(&self) -> bool {
        self.0 > Decimal::ZERO
    }

    pub fn is_negative(&self) -> bool {
        self.0 < Decimal::ZERO
    }

    pub fn is_zero(&self) -> bool {
        self.0 == Decimal::ZERO
    }

    pub fn abs(&self) -> Self {
        Self(self.0.abs())
    }
}

impl std::ops::Add for SignedUsd {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl std::ops::Sub for SignedUsd {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl fmt::Display for SignedUsd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${:.4}", self.0)
    }
}

/// Non-negative initial margin allocation cap (hard-capped at $8.00 in configuration).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MarginAllocation(Decimal);

impl MarginAllocation {
    pub fn new(val: Decimal) -> Result<Self, TypeError> {
        if val < Decimal::ZERO {
            return Err(TypeError::NegativeValue(val));
        }
        Ok(Self(val))
    }

    pub fn value(&self) -> Decimal {
        self.0
    }
}

impl fmt::Display for MarginAllocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${:.2}", self.0)
    }
}

/// Position direction: Long or Short.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Long,
    Short,
}

impl Direction {
    pub fn opposite(&self) -> Self {
        match self {
            Self::Long => Self::Short,
            Self::Short => Self::Long,
        }
    }

    /// Sign multiplier: +1 for Long, -1 for Short.
    pub fn multiplier(&self) -> Decimal {
        match self {
            Self::Long => Decimal::ONE,
            Self::Short => -Decimal::ONE,
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Long => write!(f, "LONG"),
            Self::Short => write!(f, "SHORT"),
        }
    }
}

/// Order Side: Buy or Sell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn from_entry_direction(dir: Direction) -> Self {
        match dir {
            Direction::Long => Self::Buy,
            Direction::Short => Self::Sell,
        }
    }

    pub fn from_closing_direction(dir: Direction) -> Self {
        match dir {
            Direction::Long => Self::Sell,
            Direction::Short => Self::Buy,
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Buy => write!(f, "BUY"),
            Self::Sell => write!(f, "SELL"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_price_validation() {
        assert!(Price::new(dec!(65000.5)).is_ok());
        assert!(Price::new(dec!(0)).is_ok());
        assert!(Price::new(dec!(-1)).is_err());
        assert!(Price::new_strictly_positive(dec!(0)).is_err());
    }

    #[test]
    fn test_quantity_rounding_down() {
        let q = Quantity::new(dec!(0.00057891)).unwrap();
        let rounded = q.round_down_dp(5);
        assert_eq!(rounded.value(), dec!(0.00057));
    }

    #[test]
    fn test_signed_usd_arithmetic() {
        let a = SignedUsd::new(dec!(10.50));
        let b = SignedUsd::new(dec!(-3.25));
        let sum = a + b;
        assert_eq!(sum.value(), dec!(7.25));
        let diff = a - b;
        assert_eq!(diff.value(), dec!(13.75));
    }

    #[test]
    fn test_direction_multiplier() {
        assert_eq!(Direction::Long.multiplier(), dec!(1));
        assert_eq!(Direction::Short.multiplier(), dec!(-1));
        assert_eq!(Direction::Long.opposite(), Direction::Short);
    }
}
