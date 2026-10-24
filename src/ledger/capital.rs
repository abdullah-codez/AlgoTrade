//! Bot Capital Ledger enforcing deterministic allocation of the fixed $10.00 strategy budget.
//!
//! Invariants:
//! 1. Strategy Budget: $10.00 fixed (B = 10.00).
//! 2. Maximum Trading Margin Cap: $8.00 (M_max = 8.00).
//! 3. Minimum Reserve Target: $2.00 (R_min = 2.00).
//! 4. Conservation: A_T + A_R == B == $10.00 at all times.
//! 5. Trading Bounds: $0.00 <= A_T <= M_max ($8.00).
//! 6. Reserve Bounds: R_min ($2.00) <= A_R <= B ($10.00).
//! 7. Committed Margin: $0.00 <= C_M <= A_T.
//! 8. Uncommitted Trading Margin: U_T = A_T - C_M >= $0.00.
//!
//! Reallocation rules:
//! - Trading to Reserve: Allowed if delta <= U_T.
//! - Reserve to Trading: Allowed only if A_T + delta <= M_max ($8.00) and delta <= A_R - R_min.
//! - Margin Release: Rejects non-positive amounts and amounts > C_M (no silent flooring).
//!
//! Scope & Safety Notice:
//! - This ledger tracks internal software-partitioned risk boundaries only.
//! - Bot-committed margin is internal software risk gating, distinct from exchange-reported margin.
//! - Reallocations are internal state changes; no on-chain or exchange transfers occur.

use crate::types::SignedUsd;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_STRATEGY_BUDGET: Decimal = dec!(10.00);
pub const DEFAULT_INITIAL_MARGIN_CAP: Decimal = dec!(8.00);
pub const DEFAULT_RESERVE_TARGET: Decimal = dec!(2.00);

#[derive(Error, Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub enum CapitalError {
    #[error("Capital allocation amount must be strictly positive, got: {0}")]
    NonPositiveAmount(SignedUsd),
    #[error(
        "Reallocation would cause trading allocation ({requested_total}) to exceed cap ({cap})"
    )]
    ExceedsTradingCap {
        requested_total: SignedUsd,
        cap: SignedUsd,
    },
    #[error(
        "Insufficient uncommitted trading allocation: requested {requested}, available {available}"
    )]
    InsufficientUncommittedAllocation {
        requested: SignedUsd,
        available: SignedUsd,
    },
    #[error("Reallocation would breach minimum reserve target: requested {requested}, available above reserve {available_above_reserve}")]
    ReserveTargetBreached {
        requested: SignedUsd,
        available_above_reserve: SignedUsd,
    },
    #[error("Attempted to release margin ({requested}) exceeding committed margin ({committed})")]
    AmountExceedsCommittedMargin {
        requested: SignedUsd,
        committed: SignedUsd,
    },
    #[error("Capital invariant violation: {0}")]
    InvariantViolation(String),
}

/// Direction for internal capital reallocation between trading allocation and reserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReallocationDirection {
    ToReserve,
    ToTrading,
}

/// Internal bot capital ledger tracking the $10.00 strategy budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotCapitalLedger {
    pub strategy_budget: SignedUsd,
    pub initial_margin_cap: SignedUsd,
    pub reserve_target: SignedUsd,
    pub allocated_trading: SignedUsd,
    pub allocated_reserve: SignedUsd,
    pub committed_margin: SignedUsd,
}

impl BotCapitalLedger {
    /// Create a new capital ledger with custom configured bounds.
    pub fn new(
        strategy_budget: SignedUsd,
        initial_margin_cap: SignedUsd,
        reserve_target: SignedUsd,
    ) -> Result<Self, CapitalError> {
        if !strategy_budget.is_positive() {
            return Err(CapitalError::NonPositiveAmount(strategy_budget));
        }
        if !initial_margin_cap.is_positive() {
            return Err(CapitalError::NonPositiveAmount(initial_margin_cap));
        }
        if !reserve_target.is_positive() {
            return Err(CapitalError::NonPositiveAmount(reserve_target));
        }
        if initial_margin_cap + reserve_target != strategy_budget {
            return Err(CapitalError::InvariantViolation(format!(
                "Initial margin cap ({}) + reserve target ({}) != strategy budget ({})",
                initial_margin_cap, reserve_target, strategy_budget
            )));
        }

        let ledger = Self {
            strategy_budget,
            initial_margin_cap,
            reserve_target,
            allocated_trading: initial_margin_cap,
            allocated_reserve: reserve_target,
            committed_margin: SignedUsd::zero(),
        };

        ledger.validate_invariants()?;
        Ok(ledger)
    }

    /// Construct default ledger: $10.00 budget, $8.00 trading cap, $2.00 reserve target.
    pub fn new_default() -> Self {
        Self {
            strategy_budget: SignedUsd::new(DEFAULT_STRATEGY_BUDGET),
            initial_margin_cap: SignedUsd::new(DEFAULT_INITIAL_MARGIN_CAP),
            reserve_target: SignedUsd::new(DEFAULT_RESERVE_TARGET),
            allocated_trading: SignedUsd::new(DEFAULT_INITIAL_MARGIN_CAP),
            allocated_reserve: SignedUsd::new(DEFAULT_RESERVE_TARGET),
            committed_margin: SignedUsd::zero(),
        }
    }

    /// Validate all hard mathematical invariants.
    pub fn validate_invariants(&self) -> Result<(), CapitalError> {
        // Invariant 1: Conservation of total budget
        if self.allocated_trading + self.allocated_reserve != self.strategy_budget {
            return Err(CapitalError::InvariantViolation(format!(
                "Total budget violated: trading ({}) + reserve ({}) != strategy budget ({})",
                self.allocated_trading, self.allocated_reserve, self.strategy_budget
            )));
        }

        // Invariant 2: Trading allocation bounds
        if self.allocated_trading.value() < Decimal::ZERO {
            return Err(CapitalError::InvariantViolation(format!(
                "Trading allocation negative: {}",
                self.allocated_trading
            )));
        }
        if self.allocated_trading > self.initial_margin_cap {
            return Err(CapitalError::InvariantViolation(format!(
                "Trading allocation ({}) exceeds cap ({})",
                self.allocated_trading, self.initial_margin_cap
            )));
        }

        // Invariant 3: Reserve bounds
        if self.allocated_reserve < self.reserve_target {
            return Err(CapitalError::InvariantViolation(format!(
                "Reserve allocation ({}) below target ({})",
                self.allocated_reserve, self.reserve_target
            )));
        }

        // Invariant 4: Committed margin bounds
        if self.committed_margin.value() < Decimal::ZERO {
            return Err(CapitalError::InvariantViolation(format!(
                "Committed margin negative: {}",
                self.committed_margin
            )));
        }
        if self.committed_margin > self.allocated_trading {
            return Err(CapitalError::InvariantViolation(format!(
                "Committed margin ({}) exceeds allocated trading ({})",
                self.committed_margin, self.allocated_trading
            )));
        }

        Ok(())
    }

    /// Return uncommitted trading margin available to open new positions: U_T = A_T - C_M.
    pub fn uncommitted_trading(&self) -> SignedUsd {
        self.allocated_trading - self.committed_margin
    }

    /// Reallocate internal capital based on direction.
    pub fn reallocate(
        &mut self,
        direction: ReallocationDirection,
        amount: SignedUsd,
    ) -> Result<(), CapitalError> {
        match direction {
            ReallocationDirection::ToReserve => self.reallocate_to_reserve(amount),
            ReallocationDirection::ToTrading => self.reallocate_to_trading(amount),
        }
    }

    /// Move funds from trading allocation to reserve buffer (A_T -> A_R).
    pub fn reallocate_to_reserve(&mut self, amount: SignedUsd) -> Result<(), CapitalError> {
        if !amount.is_positive() {
            return Err(CapitalError::NonPositiveAmount(amount));
        }

        let uncommitted = self.uncommitted_trading();
        if amount > uncommitted {
            return Err(CapitalError::InsufficientUncommittedAllocation {
                requested: amount,
                available: uncommitted,
            });
        }

        self.allocated_trading = self.allocated_trading - amount;
        self.allocated_reserve = self.allocated_reserve + amount;
        self.validate_invariants()?;
        Ok(())
    }

    /// Move funds from reserve buffer back to trading allocation (A_R -> A_T).
    pub fn reallocate_to_trading(&mut self, amount: SignedUsd) -> Result<(), CapitalError> {
        if !amount.is_positive() {
            return Err(CapitalError::NonPositiveAmount(amount));
        }

        let new_trading = self.allocated_trading + amount;
        if new_trading > self.initial_margin_cap {
            return Err(CapitalError::ExceedsTradingCap {
                requested_total: new_trading,
                cap: self.initial_margin_cap,
            });
        }

        let available_above_reserve = self.allocated_reserve - self.reserve_target;
        if amount > available_above_reserve {
            return Err(CapitalError::ReserveTargetBreached {
                requested: amount,
                available_above_reserve,
            });
        }

        self.allocated_trading = self.allocated_trading + amount;
        self.allocated_reserve = self.allocated_reserve - amount;
        self.validate_invariants()?;
        Ok(())
    }

    /// Commit margin to support an open trade entry.
    pub fn commit_margin(&mut self, amount: SignedUsd) -> Result<(), CapitalError> {
        if !amount.is_positive() {
            return Err(CapitalError::NonPositiveAmount(amount));
        }

        let uncommitted = self.uncommitted_trading();
        if amount > uncommitted {
            return Err(CapitalError::InsufficientUncommittedAllocation {
                requested: amount,
                available: uncommitted,
            });
        }

        self.committed_margin = self.committed_margin + amount;
        self.validate_invariants()?;
        Ok(())
    }

    /// Release margin upon trade exit fill.
    ///
    /// Rejects non-positive amounts and amounts exceeding currently committed margin.
    /// Strictly avoids silent flooring to zero on accounting errors.
    pub fn release_margin(&mut self, amount: SignedUsd) -> Result<(), CapitalError> {
        if !amount.is_positive() {
            return Err(CapitalError::NonPositiveAmount(amount));
        }

        if amount > self.committed_margin {
            return Err(CapitalError::AmountExceedsCommittedMargin {
                requested: amount,
                committed: self.committed_margin,
            });
        }

        self.committed_margin = self.committed_margin - amount;
        self.validate_invariants()?;
        Ok(())
    }

    /// Snapshot summary of current capital state.
    pub fn summary(&self) -> BotCapitalSummary {
        BotCapitalSummary {
            strategy_budget: self.strategy_budget,
            initial_margin_cap: self.initial_margin_cap,
            reserve_target: self.reserve_target,
            allocated_trading: self.allocated_trading,
            allocated_reserve: self.allocated_reserve,
            committed_margin: self.committed_margin,
            uncommitted_trading: self.uncommitted_trading(),
        }
    }
}

impl Default for BotCapitalLedger {
    fn default() -> Self {
        Self::new_default()
    }
}

/// Serializable snapshot of bot capital ledger state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotCapitalSummary {
    pub strategy_budget: SignedUsd,
    pub initial_margin_cap: SignedUsd,
    pub reserve_target: SignedUsd,
    pub allocated_trading: SignedUsd,
    pub allocated_reserve: SignedUsd,
    pub committed_margin: SignedUsd,
    pub uncommitted_trading: SignedUsd,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_capital_state() {
        let ledger = BotCapitalLedger::new_default();
        assert_eq!(ledger.strategy_budget.value(), dec!(10.00));
        assert_eq!(ledger.initial_margin_cap.value(), dec!(8.00));
        assert_eq!(ledger.reserve_target.value(), dec!(2.00));
        assert_eq!(ledger.allocated_trading.value(), dec!(8.00));
        assert_eq!(ledger.allocated_reserve.value(), dec!(2.00));
        assert_eq!(ledger.committed_margin.value(), dec!(0.00));
        assert_eq!(ledger.uncommitted_trading().value(), dec!(8.00));
        assert!(ledger.validate_invariants().is_ok());
    }

    #[test]
    fn test_reallocate_trading_to_reserve() {
        let mut ledger = BotCapitalLedger::new_default();
        let one_dollar = SignedUsd::new(dec!(1.00));

        // Move $1.00 from trading to reserve
        assert!(ledger.reallocate_to_reserve(one_dollar).is_ok());
        assert_eq!(ledger.allocated_trading.value(), dec!(7.00));
        assert_eq!(ledger.allocated_reserve.value(), dec!(3.00));
        assert_eq!(ledger.committed_margin.value(), dec!(0.00));
        assert_eq!(ledger.uncommitted_trading().value(), dec!(7.00));
        assert_eq!(
            ledger.allocated_trading + ledger.allocated_reserve,
            ledger.strategy_budget
        );
        assert!(ledger.validate_invariants().is_ok());
    }

    #[test]
    fn test_reallocate_reserve_to_trading() {
        let mut ledger = BotCapitalLedger::new_default();
        let one_dollar = SignedUsd::new(dec!(1.00));

        // First transition to $7.00 / $3.00
        assert!(ledger.reallocate_to_reserve(one_dollar).is_ok());
        assert_eq!(ledger.allocated_trading.value(), dec!(7.00));
        assert_eq!(ledger.allocated_reserve.value(), dec!(3.00));

        // Return $1.00 from reserve back to trading
        assert!(ledger.reallocate_to_trading(one_dollar).is_ok());
        assert_eq!(ledger.allocated_trading.value(), dec!(8.00));
        assert_eq!(ledger.allocated_reserve.value(), dec!(2.00));
        assert_eq!(ledger.uncommitted_trading().value(), dec!(8.00));
        assert_eq!(
            ledger.allocated_trading + ledger.allocated_reserve,
            ledger.strategy_budget
        );
        assert!(ledger.validate_invariants().is_ok());
    }

    #[test]
    fn test_reallocate_reserve_exceeds_cap_fails() {
        let mut ledger = BotCapitalLedger::new_default();
        let one_dollar = SignedUsd::new(dec!(1.00));

        // From initial state ($8.00 trading, $2.00 reserve), attempting to move $1.00 to trading
        // would reach $9.00, exceeding the $8.00 cap. Must be strictly rejected.
        let result = ledger.reallocate_to_trading(one_dollar);
        assert_eq!(
            result,
            Err(CapitalError::ExceedsTradingCap {
                requested_total: SignedUsd::new(dec!(9.00)),
                cap: SignedUsd::new(dec!(8.00)),
            })
        );

        // State remains unchanged
        assert_eq!(ledger.allocated_trading.value(), dec!(8.00));
        assert_eq!(ledger.allocated_reserve.value(), dec!(2.00));
    }

    #[test]
    fn test_reallocate_committed_margin_fails() {
        let mut ledger = BotCapitalLedger::new_default();
        let margin_commit = SignedUsd::new(dec!(7.50));

        // Commit $7.50 of trading allocation to an open position
        assert!(ledger.commit_margin(margin_commit).is_ok());
        assert_eq!(ledger.committed_margin.value(), dec!(7.50));
        assert_eq!(ledger.uncommitted_trading().value(), dec!(0.50));

        // Attempting to move $1.00 to reserve exceeds uncommitted allocation ($0.50)
        let result = ledger.reallocate_to_reserve(SignedUsd::new(dec!(1.00)));
        assert_eq!(
            result,
            Err(CapitalError::InsufficientUncommittedAllocation {
                requested: SignedUsd::new(dec!(1.00)),
                available: SignedUsd::new(dec!(0.50)),
            })
        );

        // State remains intact
        assert_eq!(ledger.allocated_trading.value(), dec!(8.00));
        assert_eq!(ledger.allocated_reserve.value(), dec!(2.00));
        assert_eq!(ledger.committed_margin.value(), dec!(7.50));
    }

    #[test]
    fn test_reallocate_non_positive_amount_fails() {
        let mut ledger = BotCapitalLedger::new_default();
        let zero = SignedUsd::zero();
        let negative = SignedUsd::new(dec!(-1.00));

        assert_eq!(
            ledger.reallocate_to_reserve(zero),
            Err(CapitalError::NonPositiveAmount(zero))
        );
        assert_eq!(
            ledger.reallocate_to_reserve(negative),
            Err(CapitalError::NonPositiveAmount(negative))
        );
        assert_eq!(
            ledger.reallocate_to_trading(zero),
            Err(CapitalError::NonPositiveAmount(zero))
        );
        assert_eq!(
            ledger.reallocate_to_trading(negative),
            Err(CapitalError::NonPositiveAmount(negative))
        );
    }

    #[test]
    fn test_margin_commitment_and_release() {
        let mut ledger = BotCapitalLedger::new_default();

        // 1. Commit $3.00
        assert!(ledger.commit_margin(SignedUsd::new(dec!(3.00))).is_ok());
        assert_eq!(ledger.committed_margin.value(), dec!(3.00));
        assert_eq!(ledger.uncommitted_trading().value(), dec!(5.00));

        // 2. Commit additional $5.00 (reaching full $8.00 cap)
        assert!(ledger.commit_margin(SignedUsd::new(dec!(5.00))).is_ok());
        assert_eq!(ledger.committed_margin.value(), dec!(8.00));
        assert_eq!(ledger.uncommitted_trading().value(), dec!(0.00));

        // 3. Attempting to commit any more fails
        assert_eq!(
            ledger.commit_margin(SignedUsd::new(dec!(0.01))),
            Err(CapitalError::InsufficientUncommittedAllocation {
                requested: SignedUsd::new(dec!(0.01)),
                available: SignedUsd::zero(),
            })
        );

        // 4. Release $5.00
        assert!(ledger.release_margin(SignedUsd::new(dec!(5.00))).is_ok());
        assert_eq!(ledger.committed_margin.value(), dec!(3.00));
        assert_eq!(ledger.uncommitted_trading().value(), dec!(5.00));

        // 5. Release remaining $3.00
        assert!(ledger.release_margin(SignedUsd::new(dec!(3.00))).is_ok());
        assert_eq!(ledger.committed_margin.value(), dec!(0.00));
        assert_eq!(ledger.uncommitted_trading().value(), dec!(8.00));
    }

    #[test]
    fn test_release_margin_exceeds_committed_fails() {
        let mut ledger = BotCapitalLedger::new_default();
        assert!(ledger.commit_margin(SignedUsd::new(dec!(2.00))).is_ok());

        // Attempting to release $2.50 when only $2.00 is committed must fail explicitly.
        // No silent flooring!
        let result = ledger.release_margin(SignedUsd::new(dec!(2.50)));
        assert_eq!(
            result,
            Err(CapitalError::AmountExceedsCommittedMargin {
                requested: SignedUsd::new(dec!(2.50)),
                committed: SignedUsd::new(dec!(2.00)),
            })
        );

        // Committed margin remains $2.00
        assert_eq!(ledger.committed_margin.value(), dec!(2.00));
    }

    #[test]
    fn test_release_margin_non_positive_fails() {
        let mut ledger = BotCapitalLedger::new_default();
        assert!(ledger.commit_margin(SignedUsd::new(dec!(2.00))).is_ok());

        let zero = SignedUsd::zero();
        let negative = SignedUsd::new(dec!(-1.00));
        assert_eq!(
            ledger.release_margin(zero),
            Err(CapitalError::NonPositiveAmount(zero))
        );
        assert_eq!(
            ledger.release_margin(negative),
            Err(CapitalError::NonPositiveAmount(negative))
        );
    }

    #[test]
    fn test_reallocation_direction_dispatch() {
        let mut ledger = BotCapitalLedger::new_default();
        let one_dollar = SignedUsd::new(dec!(1.00));

        assert!(ledger
            .reallocate(ReallocationDirection::ToReserve, one_dollar)
            .is_ok());
        assert_eq!(ledger.allocated_trading.value(), dec!(7.00));
        assert_eq!(ledger.allocated_reserve.value(), dec!(3.00));

        assert!(ledger
            .reallocate(ReallocationDirection::ToTrading, one_dollar)
            .is_ok());
        assert_eq!(ledger.allocated_trading.value(), dec!(8.00));
        assert_eq!(ledger.allocated_reserve.value(), dec!(2.00));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut ledger = BotCapitalLedger::new_default();
        ledger
            .reallocate_to_reserve(SignedUsd::new(dec!(1.00)))
            .unwrap();
        ledger.commit_margin(SignedUsd::new(dec!(2.50))).unwrap();

        let json = serde_json::to_string(&ledger).expect("Failed to serialize ledger");
        let deserialized: BotCapitalLedger =
            serde_json::from_str(&json).expect("Failed to deserialize ledger");

        assert_eq!(ledger, deserialized);
        assert_eq!(deserialized.allocated_trading.value(), dec!(7.00));
        assert_eq!(deserialized.allocated_reserve.value(), dec!(3.00));
        assert_eq!(deserialized.committed_margin.value(), dec!(2.50));
        assert_eq!(deserialized.uncommitted_trading().value(), dec!(4.50));
        assert!(deserialized.validate_invariants().is_ok());
    }
}
