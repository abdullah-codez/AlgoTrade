//! Decoupled state machine separating operational engine mode from on-exchange position lifecycle.
//! Includes deterministic seedable pseudo-random direction selector.

use crate::types::{Direction, Price, Quantity, Side};
use rand::Rng;
use rand_chacha::rand_core::SeedableRng;
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum StateError {
    #[error("Invalid state transition from {from:?} to {to:?}: {reason}")]
    InvalidTransition {
        from: String,
        to: String,
        reason: String,
    },
    #[error("Cannot reverse direction while prior position is not Flat. Current state: {0:?}")]
    PositionNotFlat(String),
}

/// Precise reason for halting trading. Evaluated in strict priority order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HaltReason {
    /// 1. MTM Hard Loss limit breached (e.g. <= -$1.00 or <= -$2.00)
    HardLossExceeded,
    /// 2. 2 Consecutive net-losing closed trades
    ConsecutiveLossesReached,
    /// 3. Cumulative net profit target reached (e.g. >= $0.70 or >= $2.00)
    ProfitTargetReached,
    /// 4. 2 Take-Profit trade exits completed
    TakeProfitCountReached,
    /// Emergency stop triggered manually (Ctrl+C or API signal)
    ManualEmergencyStop,
    /// Unprotected exposure detected (protective orders failed to place)
    UnprotectedExposure,
    /// Stale market data feed exceeded timeout threshold
    StaleDataHalt,
}

/// Operational state of the trading engine process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineMode {
    /// Initial boot or user-stopped state. Zero trades or orders executed.
    Stopped,
    /// Actively processing market ticks and evaluating signals.
    Running,
    /// Transiently pausing (e.g. recovering from network reconnect, stale data, or user pause).
    Pausing,
    /// Staged entry signal awaiting explicit user approval before submitting to exchange.
    AwaitingConfirmation,
    /// Permanently halted for the session due to a risk limit or target.
    Halted(HaltReason),
    /// Emergency shutdown actively flattening exposure.
    EmergencyShutdown(String),
}

/// Concrete lifecycle state of the on-exchange position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PositionState {
    /// Zero open position and zero open entry orders.
    Flat,
    /// Entry order is pending execution on the exchange.
    Opening {
        cloid: String,
        target_size: Quantity,
        side: Side,
        direction: Direction,
    },
    /// Position is filled on exchange and fully covered by active protective TP/SL trigger orders.
    OpenProtected {
        direction: Direction,
        size: Quantity,
        avg_entry_px: Price,
        tp_cloid: String,
        sl_cloid: String,
    },
    /// Position is actively closing (reduce-only market or limit order pending).
    Closing {
        cloid: String,
        direction: Direction,
        closing_size: Quantity,
        reason: String,
    },
}

/// Central state coordinator tracking engine operational status and position lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradingStateMachine {
    mode: EngineMode,
    position: PositionState,
    initial_direction: Option<Direction>,
    initial_seed: Option<u64>,
}

impl Default for TradingStateMachine {
    fn default() -> Self {
        Self {
            mode: EngineMode::Stopped,
            position: PositionState::Flat,
            initial_direction: None,
            initial_seed: None,
        }
    }
}

impl TradingStateMachine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a state machine initialized in Running mode (primarily for tests).
    pub fn new_running() -> Self {
        Self {
            mode: EngineMode::Running,
            position: PositionState::Flat,
            initial_direction: None,
            initial_seed: None,
        }
    }

    pub fn mode(&self) -> &EngineMode {
        &self.mode
    }

    pub fn position(&self) -> &PositionState {
        &self.position
    }

    pub fn initial_direction(&self) -> Option<Direction> {
        self.initial_direction
    }

    pub fn initial_seed(&self) -> Option<u64> {
        self.initial_seed
    }

    /// Select and record initial direction using ChaCha8 deterministic RNG.
    pub fn initialize_direction(&mut self, seed: Option<u64>) -> Direction {
        let mut rng = match seed {
            Some(s) => ChaCha8Rng::seed_from_u64(s),
            None => ChaCha8Rng::from_entropy(),
        };

        let dir = if rng.gen_bool(0.5) {
            Direction::Long
        } else {
            Direction::Short
        };

        self.initial_direction = Some(dir);
        self.initial_seed = seed;
        dir
    }

    /// Transition to Opening state when an entry order is submitted.
    pub fn transition_to_opening(
        &mut self,
        cloid: String,
        target_size: Quantity,
        side: Side,
        direction: Direction,
    ) -> Result<(), StateError> {
        if self.mode != EngineMode::Running {
            return Err(StateError::InvalidTransition {
                from: format!("{:?}", self.mode),
                to: "Opening".into(),
                reason: "Cannot open position while engine is not Running".into(),
            });
        }

        if !matches!(self.position, PositionState::Flat) {
            return Err(StateError::InvalidTransition {
                from: format!("{:?}", self.position),
                to: "Opening".into(),
                reason: "Position must be completely Flat before dispatching a new entry order"
                    .into(),
            });
        }

        self.position = PositionState::Opening {
            cloid,
            target_size,
            side,
            direction,
        };
        Ok(())
    }

    /// Transition to OpenProtected when entry is filled and protective TP/SL orders are confirmed.
    pub fn transition_to_open_protected(
        &mut self,
        direction: Direction,
        size: Quantity,
        avg_entry_px: Price,
        tp_cloid: String,
        sl_cloid: String,
    ) -> Result<(), StateError> {
        if !matches!(
            self.position,
            PositionState::Opening { .. } | PositionState::OpenProtected { .. }
        ) {
            return Err(StateError::InvalidTransition {
                from: format!("{:?}", self.position),
                to: "OpenProtected".into(),
                reason:
                    "Must be in Opening state (or resizing OpenProtected) to enter OpenProtected"
                        .into(),
            });
        }

        self.position = PositionState::OpenProtected {
            direction,
            size,
            avg_entry_px,
            tp_cloid,
            sl_cloid,
        };
        Ok(())
    }

    /// Transition to Closing state when an exit trigger occurs.
    pub fn transition_to_closing(
        &mut self,
        cloid: String,
        direction: Direction,
        closing_size: Quantity,
        reason: String,
    ) -> Result<(), StateError> {
        if !matches!(self.position, PositionState::OpenProtected { .. }) {
            return Err(StateError::InvalidTransition {
                from: format!("{:?}", self.position),
                to: "Closing".into(),
                reason: "Cannot close position unless currently in OpenProtected state".into(),
            });
        }

        self.position = PositionState::Closing {
            cloid,
            direction,
            closing_size,
            reason,
        };
        Ok(())
    }

    /// Transition to Flat when closing order fill is confirmed on exchange.
    pub fn transition_to_flat(&mut self) -> Result<(), StateError> {
        if !matches!(
            self.position,
            PositionState::Closing { .. } | PositionState::Opening { .. }
        ) {
            return Err(StateError::InvalidTransition {
                from: format!("{:?}", self.position),
                to: "Flat".into(),
                reason: "Must be in Closing or cancelled Opening state to reach Flat".into(),
            });
        }

        self.position = PositionState::Flat;
        Ok(())
    }

    /// Halt engine with a specific reason.
    pub fn halt(&mut self, reason: HaltReason) {
        self.mode = EngineMode::Halted(reason);
    }

    /// Pause engine (e.g. for stale data).
    pub fn pause(&mut self) {
        if self.mode == EngineMode::Running {
            self.mode = EngineMode::Pausing;
        }
    }

    /// Resume engine from Pausing back to Running.
    pub fn resume(&mut self) -> Result<(), StateError> {
        if self.mode != EngineMode::Pausing {
            return Err(StateError::InvalidTransition {
                from: format!("{:?}", self.mode),
                to: "Running".into(),
                reason: "Cannot resume unless currently in Pausing state".into(),
            });
        }
        self.mode = EngineMode::Running;
        Ok(())
    }

    /// Explicitly start the engine, transitioning from Stopped or Pausing to Running.
    pub fn start(&mut self) -> Result<(), StateError> {
        match self.mode {
            EngineMode::Stopped | EngineMode::Pausing => {
                self.mode = EngineMode::Running;
                Ok(())
            }
            EngineMode::Running => Ok(()),
            EngineMode::AwaitingConfirmation => {
                self.mode = EngineMode::Running;
                Ok(())
            }
            EngineMode::Halted(_) => Err(StateError::InvalidTransition {
                from: format!("{:?}", self.mode),
                to: "Running".into(),
                reason: "Cannot start or resume an engine that has breached risk limits and halted"
                    .into(),
            }),
            EngineMode::EmergencyShutdown(_) => Err(StateError::InvalidTransition {
                from: format!("{:?}", self.mode),
                to: "Running".into(),
                reason: "Cannot start an engine in emergency shutdown".into(),
            }),
        }
    }

    /// Transition to Stopped mode.
    pub fn stop(&mut self) {
        self.mode = EngineMode::Stopped;
    }

    /// Transition to AwaitingConfirmation when an entry signal is proposed in confirmation mode.
    pub fn transition_to_awaiting_confirmation(&mut self) -> Result<(), StateError> {
        if self.mode != EngineMode::Running {
            return Err(StateError::InvalidTransition {
                from: format!("{:?}", self.mode),
                to: "AwaitingConfirmation".into(),
                reason: "Cannot await entry confirmation while engine is not Running".into(),
            });
        }
        if !matches!(self.position, PositionState::Flat) {
            return Err(StateError::InvalidTransition {
                from: format!("{:?}", self.position),
                to: "AwaitingConfirmation".into(),
                reason: "Cannot propose new entry unless position is Flat".into(),
            });
        }
        self.mode = EngineMode::AwaitingConfirmation;
        Ok(())
    }

    /// Approve confirmed entry: transitions from AwaitingConfirmation to Running.
    pub fn approve_confirmation(&mut self) -> Result<(), StateError> {
        if self.mode != EngineMode::AwaitingConfirmation {
            return Err(StateError::InvalidTransition {
                from: format!("{:?}", self.mode),
                to: "Running".into(),
                reason: "Cannot approve confirmation when not in AwaitingConfirmation state".into(),
            });
        }
        self.mode = EngineMode::Running;
        Ok(())
    }

    /// Reject confirmed entry: returns from AwaitingConfirmation to Running (ready for next tick/signal).
    pub fn reject_confirmation(&mut self) -> Result<(), StateError> {
        if self.mode != EngineMode::AwaitingConfirmation {
            return Err(StateError::InvalidTransition {
                from: format!("{:?}", self.mode),
                to: "Running".into(),
                reason: "Cannot reject confirmation when not in AwaitingConfirmation state".into(),
            });
        }
        self.mode = EngineMode::Running;
        Ok(())
    }

    /// Trigger emergency shutdown.
    pub fn trigger_emergency_shutdown(&mut self, reason: String) {
        self.mode = EngineMode::EmergencyShutdown(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_deterministic_seeded_direction() {
        let mut sm1 = TradingStateMachine::new();
        let dir1 = sm1.initialize_direction(Some(42));

        let mut sm2 = TradingStateMachine::new();
        let dir2 = sm2.initialize_direction(Some(42));

        assert_eq!(dir1, dir2);
    }

    #[test]
    fn test_engine_starts_stopped_and_requires_explicit_start() {
        let mut sm = TradingStateMachine::new();
        assert_eq!(*sm.mode(), EngineMode::Stopped);

        // Cannot transition to Opening while Stopped
        let err = sm.transition_to_opening(
            "cloid-entry".into(),
            Quantity::new(dec!(0.00057)).unwrap(),
            Side::Buy,
            Direction::Long,
        );
        assert!(err.is_err());

        // Start explicitly
        sm.start().unwrap();
        assert_eq!(*sm.mode(), EngineMode::Running);

        // Now Opening succeeds
        sm.transition_to_opening(
            "cloid-entry".into(),
            Quantity::new(dec!(0.00057)).unwrap(),
            Side::Buy,
            Direction::Long,
        )
        .unwrap();
    }

    #[test]
    fn test_valid_lifecycle_transitions() {
        let mut sm = TradingStateMachine::new_running();
        assert_eq!(*sm.position(), PositionState::Flat);
        assert_eq!(*sm.mode(), EngineMode::Running);

        // 1. Flat -> Opening
        sm.transition_to_opening(
            "cloid-entry".into(),
            Quantity::new(dec!(0.00057)).unwrap(),
            Side::Buy,
            Direction::Long,
        )
        .unwrap();

        // 2. Opening -> OpenProtected
        sm.transition_to_open_protected(
            Direction::Long,
            Quantity::new(dec!(0.00057)).unwrap(),
            Price::new(dec!(70000.0)).unwrap(),
            "tp-1".into(),
            "sl-1".into(),
        )
        .unwrap();

        // 3. OpenProtected -> Closing
        sm.transition_to_closing(
            "cloid-exit".into(),
            Direction::Long,
            Quantity::new(dec!(0.00057)).unwrap(),
            "TP Hit".into(),
        )
        .unwrap();

        // 4. Closing -> Flat
        sm.transition_to_flat().unwrap();
        assert_eq!(*sm.position(), PositionState::Flat);
    }

    #[test]
    fn test_illegal_transition_rejected() {
        let mut sm = TradingStateMachine::new_running();
        // Cannot close when Flat
        let res = sm.transition_to_closing(
            "cloid-exit".into(),
            Direction::Long,
            Quantity::new(dec!(0.00057)).unwrap(),
            "Invalid".into(),
        );
        assert!(res.is_err());
    }
}
