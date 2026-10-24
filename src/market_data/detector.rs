//! Stale data detector (5,000 ms threshold) and 3-step recovery guard protocol.
//!
//! Stale recovery protocol: A trading session cannot resume trading immediately upon
//! receiving a single tick after staleness. It must verify:
//! 1. Fresh mark price received (age < threshold),
//! 2. Active protective orders verified on exchange,
//! 3. State reconciliation against exchange has zero discrepancies.

use thiserror::Error;
use tracing::{info, warn};

pub const DEFAULT_STALENESS_THRESHOLD_MS: u64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StalenessStatus {
    Fresh { age_ms: u64 },
    Stale { age_ms: u64, threshold_ms: u64 },
    MissingData,
}

impl StalenessStatus {
    pub fn is_fresh(&self) -> bool {
        matches!(self, StalenessStatus::Fresh { .. })
    }

    pub fn is_stale(&self) -> bool {
        !self.is_fresh()
    }
}

/// Detector assessing market data latency against a strict staleness threshold.
#[derive(Debug, Clone)]
pub struct StaleDataDetector {
    threshold_ms: u64,
}

impl StaleDataDetector {
    pub fn new(threshold_ms: u64) -> Self {
        Self { threshold_ms }
    }

    pub fn default_testnet() -> Self {
        Self::new(DEFAULT_STALENESS_THRESHOLD_MS)
    }

    pub fn threshold_ms(&self) -> u64 {
        self.threshold_ms
    }

    /// Check staleness of a timestamp against current time.
    pub fn evaluate(&self, last_update_ms: Option<u64>, current_time_ms: u64) -> StalenessStatus {
        match last_update_ms {
            None => StalenessStatus::MissingData,
            Some(0) => StalenessStatus::MissingData,
            Some(last_ms) => {
                let age_ms = current_time_ms.saturating_sub(last_ms);

                if age_ms <= self.threshold_ms {
                    StalenessStatus::Fresh { age_ms }
                } else {
                    warn!(
                        "MARKET DATA STALE: age {} ms exceeds threshold {} ms",
                        age_ms, self.threshold_ms
                    );
                    StalenessStatus::Stale {
                        age_ms,
                        threshold_ms: self.threshold_ms,
                    }
                }
            }
        }
    }
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum RecoveryBlocker {
    #[error("Market data is still stale or missing")]
    DataStillStale,
    #[error("Protective orders on exchange have not been verified")]
    ProtectiveCoverageUnverified,
    #[error("Exchange and local state reconciliation contains discrepancies")]
    ReconciliationDiscrepancy,
}

/// Guard coordinating the 3-step Stale Recovery Protocol.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StaleRecoveryGuard {
    is_in_stale_halt: bool,
}

impl StaleRecoveryGuard {
    pub fn new() -> Self {
        Self {
            is_in_stale_halt: false,
        }
    }

    pub fn is_in_stale_halt(&self) -> bool {
        self.is_in_stale_halt
    }

    pub fn trigger_stale_halt(&mut self) {
        if !self.is_in_stale_halt {
            warn!("ENTERING STALE DATA HALT: Trading suspended until full recovery verification");
            self.is_in_stale_halt = true;
        }
    }

    /// Verifies all three recovery criteria before lifting the stale data halt.
    pub fn attempt_recovery(
        &mut self,
        staleness: StalenessStatus,
        protective_coverage_verified: bool,
        state_synced: bool,
    ) -> Result<(), RecoveryBlocker> {
        if !self.is_in_stale_halt {
            return Ok(());
        }

        // Criterion 1: Market data must be fresh
        if !staleness.is_fresh() {
            return Err(RecoveryBlocker::DataStillStale);
        }

        // Criterion 2: Exchange-side protective orders verified active
        if !protective_coverage_verified {
            return Err(RecoveryBlocker::ProtectiveCoverageUnverified);
        }

        // Criterion 3: Full state reconciliation with zero discrepancies
        if !state_synced {
            return Err(RecoveryBlocker::ReconciliationDiscrepancy);
        }

        info!("STALE DATA RECOVERY COMPLETE: All 3 verification gates passed. Resuming normal operations");
        self.is_in_stale_halt = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_staleness_detector_thresholds() {
        let detector = StaleDataDetector::new(5_000);

        // Missing data
        assert_eq!(
            detector.evaluate(None, 10_000),
            StalenessStatus::MissingData
        );
        assert_eq!(
            detector.evaluate(Some(0), 10_000),
            StalenessStatus::MissingData
        );

        // Fresh data (age 4,999 ms)
        let fresh = detector.evaluate(Some(5_001), 10_000);
        assert_eq!(fresh, StalenessStatus::Fresh { age_ms: 4_999 });
        assert!(fresh.is_fresh());

        // Stale data (age 5,001 ms)
        let stale = detector.evaluate(Some(4_999), 10_000);
        assert_eq!(
            stale,
            StalenessStatus::Stale {
                age_ms: 5_001,
                threshold_ms: 5_000
            }
        );
        assert!(stale.is_stale());
    }

    #[test]
    fn test_stale_recovery_guard_requires_all_three_criteria() {
        let mut guard = StaleRecoveryGuard::new();
        guard.trigger_stale_halt();
        assert!(guard.is_in_stale_halt());

        let fresh_status = StalenessStatus::Fresh { age_ms: 100 };
        let stale_status = StalenessStatus::Stale {
            age_ms: 6_000,
            threshold_ms: 5_000,
        };

        // Attempt with stale data -> rejects
        assert_eq!(
            guard.attempt_recovery(stale_status, true, true),
            Err(RecoveryBlocker::DataStillStale)
        );
        assert!(guard.is_in_stale_halt());

        // Attempt with fresh data, but unverified protection -> rejects
        assert_eq!(
            guard.attempt_recovery(fresh_status, false, true),
            Err(RecoveryBlocker::ProtectiveCoverageUnverified)
        );
        assert!(guard.is_in_stale_halt());

        // Attempt with fresh data & protection, but state discrepancy -> rejects
        assert_eq!(
            guard.attempt_recovery(fresh_status, true, false),
            Err(RecoveryBlocker::ReconciliationDiscrepancy)
        );
        assert!(guard.is_in_stale_halt());

        // All 3 satisfied -> succeeds and clears halt
        assert!(guard.attempt_recovery(fresh_status, true, true).is_ok());
        assert!(!guard.is_in_stale_halt());
    }
}
