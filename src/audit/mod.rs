//! Structured append-only JSONL audit event logger and live event broadcaster.
//! Logs all operational and financial lifecycle transitions to events.jsonl without leaking secrets.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tracing::error;

pub const DEFAULT_EVENTS_LOG_PATH: &str = "events.jsonl";
pub const MAX_IN_MEMORY_LOGS: usize = 100;

/// Structured event payload recorded in the audit log and broadcast via WebSocket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum AuditEventData {
    EngineStarted {
        profile: String,
        margin_mode: String,
        execution_mode: String,
        leverage: u32,
    },
    EngineStopped {
        reason: String,
    },
    EnginePaused {
        reason: String,
    },
    EmergencyFlattenTriggered {
        reason: String,
    },
    EntryProposed {
        proposal_id: String,
        direction: String,
        price: String,
        size: String,
        notional: String,
        tp_price: String,
        sl_price: String,
        expires_at: DateTime<Utc>,
    },
    EntryApproved {
        proposal_id: String,
    },
    EntryRejected {
        proposal_id: String,
        reason: String,
    },
    OrderSubmitted {
        cloid: String,
        coin: String,
        side: String,
        size: String,
        price: String,
        reduce_only: bool,
    },
    FillRecorded {
        fill_id: String,
        cloid: Option<String>,
        direction: String,
        side: String,
        price: String,
        size: String,
        fee: String,
        gross_pnl: Option<String>,
        net_pnl: Option<String>,
    },
    ProtectionEstablished {
        tp_oid: u64,
        sl_oid: u64,
        tp_price: String,
        sl_price: String,
        size: String,
    },
    ProtectionResized {
        old_size: String,
        new_size: String,
    },
    TrailingStopUpdated {
        previous_stop: String,
        new_stop: String,
        high_water_mark: String,
    },
    FundingReceived {
        payment: String,
        rate: String,
    },
    RiskLimitBreached {
        halt_reason: String,
        details: String,
    },
    ReconciliationDiscrepancy {
        discrepancies: Vec<String>,
    },
    AgentAuthorizationFailed {
        message: String,
    },
    CapitalReallocated {
        direction: String,
        amount: String,
        new_trading_allocation: String,
        new_reserve_balance: String,
    },
    SystemInfo {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub timestamp: DateTime<Utc>,
    pub event: AuditEventData,
}

impl AuditEvent {
    pub fn new(event: AuditEventData) -> Self {
        Self {
            timestamp: Utc::now(),
            event,
        }
    }
}

/// Thread-safe append-only event logger with file persistence and in-memory replay buffer.
pub struct AuditLogger {
    file_path: PathBuf,
    recent_events: Arc<Mutex<VecDeque<AuditEvent>>>,
    broadcast_tx: broadcast::Sender<AuditEvent>,
}

impl AuditLogger {
    pub fn new(file_path: impl AsRef<Path>) -> Self {
        let (broadcast_tx, _) = broadcast::channel(256);
        Self {
            file_path: file_path.as_ref().to_path_buf(),
            recent_events: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_IN_MEMORY_LOGS))),
            broadcast_tx,
        }
    }

    pub fn default_logger() -> Self {
        Self::new(DEFAULT_EVENTS_LOG_PATH)
    }

    /// Record an event: writes to events.jsonl, saves to in-memory buffer, and broadcasts via channel.
    pub fn record(&self, event_data: AuditEventData) {
        let event = AuditEvent::new(event_data);

        // 1. Append to JSONL file
        if let Ok(json_line) = serde_json::to_string(&event) {
            match OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.file_path)
            {
                Ok(mut file) => {
                    if let Err(e) = writeln!(file, "{}", json_line) {
                        error!(
                            "Failed to write to audit log {}: {}",
                            self.file_path.display(),
                            e
                        );
                    }
                }
                Err(e) => {
                    error!(
                        "Failed to open audit log {}: {}",
                        self.file_path.display(),
                        e
                    );
                }
            }
        }

        // 2. Push to recent in-memory ring buffer
        if let Ok(mut buffer) = self.recent_events.lock() {
            if buffer.len() >= MAX_IN_MEMORY_LOGS {
                buffer.pop_front();
            }
            buffer.push_back(event.clone());
        }

        // 3. Broadcast to active WebSocket subscribers
        let _ = self.broadcast_tx.send(event);
    }

    /// Get recent in-memory events for REST API inspection.
    pub fn recent_events(&self) -> Vec<AuditEvent> {
        self.recent_events
            .lock()
            .map(|buf| buf.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Subscribe to the live broadcast channel for WebSockets.
    pub fn subscribe(&self) -> broadcast::Receiver<AuditEvent> {
        self.broadcast_tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_logger_records_and_buffers() {
        let temp_dir = std::env::temp_dir();
        let log_file = temp_dir.join(format!(
            "test_events_{}.jsonl",
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));

        let logger = AuditLogger::new(&log_file);
        let mut rx = logger.subscribe();

        logger.record(AuditEventData::SystemInfo {
            message: "Testing audit log".into(),
        });

        let events = logger.recent_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0].event,
            AuditEventData::SystemInfo { ref message } if message == "Testing audit log"
        ));

        // Receiver should receive the broadcast event
        let recv_event = rx.try_recv().unwrap();
        assert_eq!(recv_event, events[0]);

        // Clean up temp file
        let _ = std::fs::remove_file(log_file);
    }
}
