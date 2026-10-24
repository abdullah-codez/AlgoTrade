//! Atomic session state persistence and recovery.
//! Saves session state to session_state.json via atomic file replacement.
//! Guaranteed to never persist secrets or private keys.

use crate::config::EngineConfig;
use crate::ledger::{FillRecord, FundingRecord, MatchedTrade};
use crate::state::{EngineMode, PositionState};
use crate::types::SignedUsd;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::info;

pub const DEFAULT_STATE_FILE: &str = "session_state.json";

/// Complete serializable state of an engine session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedSessionState {
    pub updated_at: DateTime<Utc>,
    /// Persisted engine mode from prior run (used for discrepancy reporting).
    pub last_engine_mode: EngineMode,
    /// Active engine configuration.
    pub config: EngineConfig,
    /// Position state on exchange.
    pub position: PositionState,
    /// Closed trades history.
    pub closed_trades: Vec<MatchedTrade>,
    /// Active open fill lots.
    pub open_lots: Vec<FillRecord>,
    /// Recorded funding payments.
    pub funding_records: Vec<FundingRecord>,
    /// Current consecutive loss count.
    pub consecutive_losses: u32,
    /// Net realized P&L across session.
    pub total_realized_net_pnl: SignedUsd,
    /// Total taker fees paid across session.
    pub total_fees_paid: SignedUsd,
    /// Internal bot capital ledger tracking $10 budget, allocations, and committed margin.
    #[serde(default)]
    pub capital_ledger: Option<crate::ledger::BotCapitalLedger>,
}

impl PersistedSessionState {
    pub fn new_initial(config: EngineConfig) -> Self {
        Self {
            updated_at: Utc::now(),
            last_engine_mode: EngineMode::Stopped,
            config,
            position: PositionState::Flat,
            closed_trades: Vec::new(),
            open_lots: Vec::new(),
            funding_records: Vec::new(),
            consecutive_losses: 0,
            total_realized_net_pnl: SignedUsd::zero(),
            total_fees_paid: SignedUsd::zero(),
            capital_ledger: Some(crate::ledger::BotCapitalLedger::default()),
        }
    }
}

/// State persistence manager handling atomic writes and recovery.
pub struct StatePersistenceManager {
    file_path: PathBuf,
}

impl StatePersistenceManager {
    pub fn new(file_path: impl AsRef<Path>) -> Self {
        Self {
            file_path: file_path.as_ref().to_path_buf(),
        }
    }

    pub fn default_manager() -> Self {
        Self::new(DEFAULT_STATE_FILE)
    }

    /// Atomically persist state using write-then-rename pattern.
    pub fn save_state(&self, state: &PersistedSessionState) -> Result<(), String> {
        let tmp_path = self.file_path.with_extension("tmp");
        let json_bytes = serde_json::to_vec_pretty(state)
            .map_err(|e| format!("Failed to serialize session state: {}", e))?;

        // 1. Write to temporary file
        let mut file = File::create(&tmp_path)
            .map_err(|e| format!("Failed to create temporary state file: {}", e))?;
        file.write_all(&json_bytes)
            .map_err(|e| format!("Failed to write state bytes: {}", e))?;
        file.sync_all()
            .map_err(|e| format!("Failed to sync temporary state file: {}", e))?;

        // 2. Atomically rename temporary file over target file
        fs::rename(&tmp_path, &self.file_path)
            .map_err(|e| format!("Failed to atomically replace state file: {}", e))?;

        Ok(())
    }

    /// Load persisted state if file exists.
    /// Invariant: Does NOT treat loaded state as permission to trade.
    pub fn load_state(&self) -> Result<Option<PersistedSessionState>, String> {
        if !self.file_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&self.file_path)
            .map_err(|e| format!("Failed to read state file: {}", e))?;
        let state: PersistedSessionState = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to deserialize state file: {}", e))?;

        info!(
            "Loaded persisted session state from {}. Last engine mode: {:?}, Position: {:?}",
            self.file_path.display(),
            state.last_engine_mode,
            state.position
        );

        Ok(Some(state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_atomic_persistence_save_and_load() {
        let temp_dir = std::env::temp_dir();
        let state_file = temp_dir.join(format!(
            "test_session_state_{}.json",
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let manager = StatePersistenceManager::new(&state_file);

        let initial_state = PersistedSessionState::new_initial(EngineConfig::normal_default());
        manager.save_state(&initial_state).expect("Save failed");

        let loaded = manager
            .load_state()
            .expect("Load failed")
            .expect("Expected Some state");
        assert_eq!(loaded.last_engine_mode, EngineMode::Stopped);
        assert_eq!(loaded.config.profile.leverage, 5);

        // Clean up
        let _ = fs::remove_file(state_file);
    }
}
