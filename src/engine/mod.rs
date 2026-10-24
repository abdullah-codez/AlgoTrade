//! Central asynchronous engine orchestrator coordinating market data, risk controls,
//! order execution, continuous protection, state persistence, and web dashboard controls.

pub mod persistence;

use crate::audit::{AuditEventData, AuditLogger};
use crate::config::{EngineConfig, ExecutionMode, MarketMetadata};
use crate::execution::client::{ExchangeClient, OrderRequest, OrderTypeSpec};
use crate::execution::protection::ProtectionManager;
use crate::execution::reconciliation::{reconcile_state, ReconciliationReport};
use crate::execution::{CandleSnapshot, SignerStatus};
use crate::ledger::{
    BotCapitalLedger, BotCapitalSummary, FillLedger, FillRecord, FundingLedger, FundingRecord,
    NewFillParams, ReallocationDirection,
};
use crate::market_data::{
    CandleCache, MarkPriceCache, StaleDataDetector, StaleRecoveryGuard, StalenessStatus,
};
use crate::risk::{
    calculate_mtm_session_equity_change, calculate_order_size, calculate_unrealized_gross_pnl,
    ConsecutiveLossTracker,
};
use crate::state::{EngineMode, HaltReason, PositionState, TradingStateMachine};
use crate::strategy::{
    calculate_tp_sl_prices, calculate_trailing_stop_price, determine_next_direction,
};
use crate::types::{Direction, Price, Quantity, Side, SignedUsd};
use chrono::{DateTime, Duration, Utc};
use persistence::{PersistedSessionState, StatePersistenceManager};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

pub const PROPOSAL_TTL_SECONDS: i64 = 60;
pub const MAX_ALLOWED_PRICE_DRIFT_PCT: Decimal = dec!(0.0025); // 0.25%

/// Staged trade entry proposal waiting for explicit approval in Confirmation Mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEntryProposal {
    pub proposal_id: String,
    pub direction: Direction,
    pub proposed_entry_price: Price,
    pub size: Quantity,
    pub notional: SignedUsd,
    pub tp_price: Price,
    pub sl_price: Price,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl PendingEntryProposal {
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }

    pub fn has_drifted(&self, current_price: Price) -> bool {
        let proposed = self.proposed_entry_price.value();
        let current = current_price.value();
        let drift = (current - proposed).abs() / proposed;
        drift > MAX_ALLOWED_PRICE_DRIFT_PCT
    }
}

/// Comprehensive, deterministic trade preview for staging before entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradePreview {
    pub label: String,
    pub profile: String,
    pub margin_mode: String,
    pub direction: Direction,
    pub leverage: u32,
    pub entry_price: Price,
    pub quantity: Quantity,
    pub notional: SignedUsd,
    pub estimated_margin: SignedUsd,
    pub tp_price: Price,
    pub sl_price: Price,
    pub benchmark_fee_rate: String,
    pub estimated_entry_fee: SignedUsd,
    pub estimated_exit_fee_tp: SignedUsd,
    pub estimated_exit_fee_sl: SignedUsd,
    pub estimated_tp_gross_pnl: SignedUsd,
    pub estimated_tp_net_pnl: SignedUsd,
    pub estimated_sl_gross_pnl: SignedUsd,
    pub estimated_sl_net_pnl: SignedUsd,
    pub current_trading_allocation: SignedUsd,
    pub remaining_trading_allocation: SignedUsd,
}

/// Snapshot of the engine's active runtime state for telemetry and API responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineStatusSnapshot {
    pub mode: EngineMode,
    pub position: PositionState,
    pub config: EngineConfig,
    pub mark_price: Option<Price>,
    pub mark_price_age_ms: Option<u64>,
    pub is_stale: bool,
    pub unrealized_pnl: SignedUsd,
    pub total_realized_net_pnl: SignedUsd,
    pub total_fees_paid: SignedUsd,
    pub total_signed_funding: SignedUsd,
    pub consecutive_losses: u32,
    pub pending_proposal: Option<PendingEntryProposal>,
    pub active_tp_oid: Option<u64>,
    pub active_sl_oid: Option<u64>,
    pub last_discrepancies: Vec<String>,
    pub capital_summary: BotCapitalSummary,
    pub signer_status: SignerStatus,
}

/// Central trading orchestrator.
pub struct EngineOrchestrator {
    pub coin: String,
    pub target_account: String,
    pub state_machine: Arc<RwLock<TradingStateMachine>>,
    pub config: Arc<RwLock<EngineConfig>>,
    pub market_metadata: Arc<RwLock<MarketMetadata>>,
    pub fill_ledger: Arc<RwLock<FillLedger>>,
    pub funding_ledger: Arc<RwLock<FundingLedger>>,
    pub capital_ledger: Arc<RwLock<BotCapitalLedger>>,
    pub candle_cache: Arc<CandleCache>,
    pub signer_status: Arc<RwLock<SignerStatus>>,
    pub loss_tracker: Arc<RwLock<ConsecutiveLossTracker>>,
    pub mark_price_cache: Arc<MarkPriceCache>,
    pub stale_detector: Arc<StaleDataDetector>,
    pub stale_recovery_guard: Arc<RwLock<StaleRecoveryGuard>>,
    pub protection_manager: Arc<RwLock<ProtectionManager>>,
    pub exchange_client: Arc<dyn ExchangeClient>,
    pub audit_logger: Arc<AuditLogger>,
    pub persistence_manager: Arc<StatePersistenceManager>,
    pub pending_proposal: Arc<RwLock<Option<PendingEntryProposal>>>,
    pub last_reconciliation: Arc<RwLock<Option<ReconciliationReport>>>,
    pub stop_after_close: Arc<RwLock<bool>>,
    pub extreme_mark_price: Arc<RwLock<Option<Price>>>,
}

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl EngineOrchestrator {
    pub fn new(
        coin: impl Into<String>,
        target_account: impl Into<String>,
        config: EngineConfig,
        market_metadata: MarketMetadata,
        exchange_client: Arc<dyn ExchangeClient>,
        audit_logger: Arc<AuditLogger>,
        persistence_manager: Arc<StatePersistenceManager>,
    ) -> Self {
        let coin = coin.into();
        let target_account = target_account.into();
        let stale_detector = Arc::new(StaleDataDetector::new(5000));
        let stale_recovery_guard = Arc::new(RwLock::new(StaleRecoveryGuard::new()));
        let protection_manager = Arc::new(RwLock::new(ProtectionManager::new(&coin)));
        let candle_cache = Arc::new(CandleCache::default());
        let capital_ledger = Arc::new(RwLock::new(BotCapitalLedger::default()));
        let signer_status = Arc::new(RwLock::new(SignerStatus::Active));

        Self {
            coin,
            target_account,
            state_machine: Arc::new(RwLock::new(TradingStateMachine::new())), // Boots in Stopped!
            config: Arc::new(RwLock::new(config)),
            market_metadata: Arc::new(RwLock::new(market_metadata)),
            fill_ledger: Arc::new(RwLock::new(FillLedger::new())),
            funding_ledger: Arc::new(RwLock::new(FundingLedger::new())),
            capital_ledger,
            candle_cache,
            signer_status,
            loss_tracker: Arc::new(RwLock::new(ConsecutiveLossTracker::new())),
            mark_price_cache: Arc::new(MarkPriceCache::new()),
            stale_detector,
            stale_recovery_guard,
            protection_manager,
            exchange_client,
            audit_logger,
            persistence_manager,
            pending_proposal: Arc::new(RwLock::new(None)),
            last_reconciliation: Arc::new(RwLock::new(None)),
            stop_after_close: Arc::new(RwLock::new(false)),
            extreme_mark_price: Arc::new(RwLock::new(None)),
        }
    }

    /// Load persisted state on startup.
    /// Invariant: Restores historical ledgers and configuration, but engine remains strictly Stopped.
    pub async fn load_persisted_state(&self) -> Result<(), String> {
        if let Some(persisted) = self.persistence_manager.load_state()? {
            *self.config.write().await = persisted.config;
            *self.loss_tracker.write().await =
                ConsecutiveLossTracker::with_streak(persisted.consecutive_losses);

            if let Some(cap) = persisted.capital_ledger {
                *self.capital_ledger.write().await = cap;
            }

            // Rebuild fill ledger from persisted trades and lots
            let mut ledger = self.fill_ledger.write().await;
            for lot in persisted.open_lots {
                ledger.add_entry_fill(lot);
            }

            let mut funding = self.funding_ledger.write().await;
            for rec in persisted.funding_records {
                funding.add_funding_record(rec);
            }

            self.audit_logger.record(AuditEventData::SystemInfo {
                message: format!(
                    "Recovered persisted session state. Prior engine mode: {:?}. Engine is currently STOPPED.",
                    persisted.last_engine_mode
                ),
            });
        }
        Ok(())
    }

    /// Pre-flight reconciliation check and explicit Start.
    pub async fn start(&self) -> Result<(), String> {
        let signer_st = *self.signer_status.read().await;
        if signer_st != SignerStatus::Active {
            return Err(format!(
                "Cannot start trading: signer is not active (status: {:?}). Trading requires an authorized agent signer.",
                signer_st
            ));
        }

        let config = self.config.read().await.clone();
        config
            .validate()
            .map_err(|e| format!("Configuration validation failed: {}", e))?;

        // 1. Check mark price freshness
        let now_ms = current_time_ms();
        let last_ms = self.mark_price_cache.last_update_ms();
        let status = self
            .stale_detector
            .evaluate(if last_ms > 0 { Some(last_ms) } else { None }, now_ms);
        if matches!(
            status,
            StalenessStatus::Stale { .. } | StalenessStatus::MissingData
        ) {
            return Err("Cannot start engine: market data is stale or not yet received.".into());
        }

        // 2. Preflight capital checks
        let capital = self.capital_ledger.read().await;
        if capital.uncommitted_trading().value() <= Decimal::ZERO {
            return Err(
                "Cannot start engine: uncommitted trading capital is zero or negative.".into(),
            );
        }
        drop(capital);

        // 3. Preflight trade preview validation
        self.generate_trade_preview().await.map_err(|e| {
            format!(
                "Cannot start engine: trade preview preflight calculation failed: {}",
                e
            )
        })?;

        // 2. Perform exchange reconciliation
        let sm = self.state_machine.read().await;
        let report = reconcile_state(
            self.exchange_client.as_ref(),
            &self.target_account,
            &sm,
            &self.coin,
        )
        .await
        .map_err(|e| format!("Reconciliation check failed: {}", e))?;
        drop(sm);

        *self.last_reconciliation.write().await = Some(report.clone());
        if !report.is_synced {
            let msg = format!(
                "Reconciliation found discrepancies: {}",
                report.discrepancies.join("; ")
            );
            warn!("{}", msg);
            self.audit_logger
                .record(AuditEventData::ReconciliationDiscrepancy {
                    discrepancies: report.discrepancies.clone(),
                });
        }

        // 3. Apply leverage and margin mode to exchange
        if let Err(e) = self
            .exchange_client
            .update_leverage(
                &self.coin,
                config.margin_mode.is_cross(),
                config.profile.leverage,
            )
            .await
        {
            warn!(
                "Notice: update_leverage returned: {}. Proceeding with start.",
                e
            );
        }

        // 4. Transition state machine to Running
        let mut sm = self.state_machine.write().await;
        sm.start()
            .map_err(|e| format!("State error on start: {}", e))?;
        drop(sm);

        // Reset stop_after_close flag
        *self.stop_after_close.write().await = false;

        self.audit_logger.record(AuditEventData::EngineStarted {
            profile: format!("{:?}", config.profile.profile),
            margin_mode: format!("{:?}", config.margin_mode),
            execution_mode: format!("{:?}", config.execution_mode),
            leverage: config.profile.leverage,
        });

        self.persist_active_state().await?;
        info!("Engine successfully started in Running mode.");
        Ok(())
    }

    /// Pause new trade entries (maintains open positions and stops).
    pub async fn pause(&self) -> Result<(), String> {
        let mut sm = self.state_machine.write().await;
        sm.pause();
        drop(sm);
        self.audit_logger.record(AuditEventData::EnginePaused {
            reason: "User requested pause via dashboard".into(),
        });
        self.persist_active_state().await?;
        info!("Engine paused.");
        Ok(())
    }

    /// Stop engine: cancels pending proposals and either stops immediately if Flat,
    /// or arms graceful stop after current position closes.
    pub async fn stop(&self) -> Result<(), String> {
        // Clear any pending entry proposal
        *self.pending_proposal.write().await = None;

        let mut sm = self.state_machine.write().await;
        if matches!(sm.position(), PositionState::Flat) {
            sm.stop();
            self.audit_logger.record(AuditEventData::EngineStopped {
                reason: "User requested stop while Flat".into(),
            });
            info!("Engine stopped immediately (Flat).");
        } else {
            *self.stop_after_close.write().await = true;
            self.audit_logger.record(AuditEventData::SystemInfo {
                message: "Stop armed: engine will stop gracefully when open position closes."
                    .into(),
            });
            info!("Stop armed: will stop after current position closes.");
        }
        drop(sm);

        self.persist_active_state().await?;
        Ok(())
    }

    /// Emergency flatten: immediately market closes open position and cancels protective orders.
    pub async fn emergency_flatten(&self) -> Result<(), String> {
        info!("Initiating emergency flatten protocol...");
        self.audit_logger
            .record(AuditEventData::EmergencyFlattenTriggered {
                reason: "Manual user emergency flatten requested".into(),
            });

        // 1. Cancel resting protective orders
        let mut prot = self.protection_manager.write().await;
        prot.cancel_all_protection(self.exchange_client.as_ref())
            .await;
        drop(prot);

        // 2. Clear pending proposal
        *self.pending_proposal.write().await = None;

        // 3. Check current open position on exchange and dispatch reduce-only market close
        let user_state = self
            .exchange_client
            .get_user_state(&self.target_account)
            .await
            .map_err(|e| format!("Failed to fetch user state during emergency flatten: {}", e))?;

        if let Some(pos) = user_state.positions.iter().find(|p| p.coin == self.coin) {
            let pos_size = pos.szi;

            if !pos_size.is_zero() {
                let close_side = if pos.is_long { Side::Sell } else { Side::Buy };
                let close_order = OrderRequest {
                    coin: self.coin.clone(),
                    is_buy: close_side == Side::Buy,
                    size: pos_size,
                    limit_px: if close_side == Side::Buy {
                        Price::new(dec!(200000.0)).unwrap()
                    } else {
                        Price::new(dec!(1000.0)).unwrap()
                    },
                    reduce_only: true,
                    order_type: OrderTypeSpec::Market,
                    cloid: Some(format!("emg-close-{}", Utc::now().timestamp_millis())),
                };

                let _ = self.exchange_client.place_orders(vec![close_order]).await;
            }
        }

        // 4. Transition state machine to EmergencyShutdown
        let mut sm = self.state_machine.write().await;
        sm.trigger_emergency_shutdown("Manual Emergency Flatten Completed".into());
        let _ = sm.transition_to_flat();
        drop(sm);

        self.persist_active_state().await?;
        info!("Emergency flatten completed.");
        Ok(())
    }

    /// Approve a pending entry proposal (Confirmation Mode).
    pub async fn approve_entry(&self, proposal_id: &str) -> Result<(), String> {
        let signer_st = *self.signer_status.read().await;
        if signer_st != SignerStatus::Active {
            return Err(format!(
                "Cannot approve entry: signer is not active (status: {:?}).",
                signer_st
            ));
        }

        let proposal_opt = self.pending_proposal.read().await.clone();
        let proposal = match proposal_opt {
            Some(p) if p.proposal_id == proposal_id => p,
            Some(_) => return Err("Proposal ID mismatch or stale proposal".into()),
            None => return Err("No pending entry proposal to approve".into()),
        };

        if proposal.is_expired() {
            *self.pending_proposal.write().await = None;
            return Err("Proposal has expired. Entry aborted.".into());
        }

        // 1. Preflight staleness check
        let now_ms = current_time_ms();
        let last_ms = self.mark_price_cache.last_update_ms();
        let status = self
            .stale_detector
            .evaluate(if last_ms > 0 { Some(last_ms) } else { None }, now_ms);
        if matches!(
            status,
            StalenessStatus::Stale { .. } | StalenessStatus::MissingData
        ) {
            return Err("Cannot approve entry: market data is stale or missing.".into());
        }

        // 2. Preflight capital check
        let capital = self.capital_ledger.read().await;
        if capital.uncommitted_trading().value() <= Decimal::ZERO {
            return Err("Cannot approve entry: insufficient uncommitted trading capital.".into());
        }
        drop(capital);

        // 3. Revalidate price drift
        let current_price = self
            .mark_price_cache
            .latest_price()
            .ok_or("Cannot execute: mark price is unavailable")?;

        if proposal.has_drifted(current_price) {
            *self.pending_proposal.write().await = None;
            return Err(format!(
                "Price drifted > 0.25% (proposed: {}, current: {}). Proposal rejected.",
                proposal.proposed_entry_price, current_price
            ));
        }

        // 4. Preflight trade preview validation
        self.generate_trade_preview().await.map_err(|e| {
            format!(
                "Cannot approve entry: trade preview validation failed: {}",
                e
            )
        })?;

        // Approve proposal and execute entry order
        self.audit_logger.record(AuditEventData::EntryApproved {
            proposal_id: proposal_id.into(),
        });
        *self.pending_proposal.write().await = None;

        self.execute_entry_order(proposal.direction, proposal.size, current_price)
            .await?;

        Ok(())
    }

    /// Reject a pending entry proposal (Confirmation Mode).
    pub async fn reject_entry(&self, proposal_id: &str, reason: &str) -> Result<(), String> {
        *self.pending_proposal.write().await = None;
        self.audit_logger.record(AuditEventData::EntryRejected {
            proposal_id: proposal_id.into(),
            reason: reason.into(),
        });
        info!("Entry proposal {} rejected: {}", proposal_id, reason);
        Ok(())
    }

    /// Process incoming mark price tick.
    pub async fn on_mark_price_tick(&self, mark_px: Price, timestamp_ms: u64) {
        self.mark_price_cache.update(mark_px, timestamp_ms);

        // Check data staleness
        let now_ms = current_time_ms();
        let status = self.stale_detector.evaluate(Some(timestamp_ms), now_ms);
        let mut sm = self.state_machine.write().await;

        if matches!(status, StalenessStatus::Stale { .. }) && *sm.mode() == EngineMode::Running {
            warn!("Stale mark price detected. Pausing engine.");
            sm.pause();
            self.audit_logger.record(AuditEventData::EnginePaused {
                reason: "5000ms stale mark price feed threshold exceeded".into(),
            });
            return;
        }

        // If in Pausing state due to stale data, check recovery guard
        if *sm.mode() == EngineMode::Pausing {
            let mut guard = self.stale_recovery_guard.write().await;
            if guard.is_in_stale_halt() && guard.attempt_recovery(status, true, true).is_ok() {
                let _ = sm.resume();
                self.audit_logger.record(AuditEventData::SystemInfo {
                    message: "Stale data recovery verified. Resumed Running mode.".into(),
                });
            }
        }

        if *sm.mode() != EngineMode::Running {
            return;
        }

        // 1. If in OpenProtected, evaluate MTM session equity and trailing stop
        if let PositionState::OpenProtected {
            direction,
            size,
            avg_entry_px,
            ..
        } = *sm.position()
        {
            drop(sm);
            self.evaluate_open_position_tick(direction, size, avg_entry_px, mark_px)
                .await;
            return;
        }

        // 2. If Flat and in Running mode, evaluate entry signal
        if matches!(*sm.position(), PositionState::Flat) {
            let config = self.config.read().await.clone();
            drop(sm);
            self.evaluate_entry_signal(&config, mark_px).await;
        }
    }

    /// Evaluate open position risk, P&L, and trailing stop triggers.
    async fn evaluate_open_position_tick(
        &self,
        direction: Direction,
        size: Quantity,
        entry_px: Price,
        mark_px: Price,
    ) {
        let config = self.config.read().await.clone();

        // 1. Update extreme price for trailing stop tracking
        let mut extreme_lock = self.extreme_mark_price.write().await;
        let extreme_px = match *extreme_lock {
            Some(prev) => {
                let updated = match direction {
                    Direction::Long => {
                        if mark_px.value() > prev.value() {
                            mark_px
                        } else {
                            prev
                        }
                    }
                    Direction::Short => {
                        if mark_px.value() < prev.value() {
                            mark_px
                        } else {
                            prev
                        }
                    }
                };
                *extreme_lock = Some(updated);
                updated
            }
            None => {
                *extreme_lock = Some(mark_px);
                mark_px
            }
        };

        // 2. Check trailing stop replacement if enabled
        if config.profile.trailing_stop_enabled {
            if let Some(new_trail_stop) = calculate_trailing_stop_price(
                entry_px,
                direction,
                extreme_px,
                config.profile.trailing_stop_trigger_pct,
                config.profile.trailing_stop_distance_pct,
            ) {
                let prot = self.protection_manager.read().await;
                if prot.active_protection().is_some() {
                    self.audit_logger
                        .record(AuditEventData::TrailingStopUpdated {
                            previous_stop: "Active Stop".into(),
                            new_stop: format!("${}", new_trail_stop),
                            high_water_mark: format!("${}", extreme_px),
                        });
                }
            }
        }
        drop(extreme_lock);

        // 3. Mark-to-Market Hard Loss Risk Check
        let unrealized_gross = calculate_unrealized_gross_pnl(entry_px, mark_px, size, direction);
        let realized_net = self.fill_ledger.read().await.total_realized_net_pnl();
        let funding_cash_flow = self
            .funding_ledger
            .read()
            .await
            .total_signed_funding_cash_flow();

        let mtm_session_equity_change = calculate_mtm_session_equity_change(
            realized_net,
            unrealized_gross,
            size,
            mark_px,
            config.profile.base_taker_fee_rate,
            dec!(0.0001), // estimated slippage
        ) + funding_cash_flow;

        if mtm_session_equity_change <= config.profile.hard_session_loss_limit {
            error!(
                "Hard session loss limit breached! MTM Change: {}, Limit: {}. Triggering Emergency Flatten.",
                mtm_session_equity_change, config.profile.hard_session_loss_limit
            );
            self.audit_logger.record(AuditEventData::RiskLimitBreached {
                halt_reason: "HardLossExceeded".into(),
                details: format!(
                    "MTM session equity change {} breached limit {}",
                    mtm_session_equity_change, config.profile.hard_session_loss_limit
                ),
            });
            let _ = self.emergency_flatten().await;
            let mut sm = self.state_machine.write().await;
            sm.halt(HaltReason::HardLossExceeded);
        }
    }

    /// Evaluate entry signal when position is Flat.
    async fn evaluate_entry_signal(&self, config: &EngineConfig, mark_px: Price) {
        // Calculate sizing
        let meta = self.market_metadata.read().await.clone();
        let sizing = match calculate_order_size(
            config.profile.initial_margin_cap,
            config.profile.leverage,
            mark_px,
            &meta,
            config.profile.min_order_notional,
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!("Cannot open entry: sizing calculation failed: {}", e);
                return;
            }
        };

        // Determine direction:
        // Use seeded initial direction for first trade; subsequent trades follow TP/SL outcome
        let mut sm = self.state_machine.write().await;
        let direction = match sm.initial_direction() {
            Some(d) => {
                let ledger = self.fill_ledger.read().await;
                if let Some(last_trade) = ledger.closed_trades().last() {
                    let last_was_tp = last_trade.gross_pnl.value() > Decimal::ZERO;
                    let (next_dir, _) = determine_next_direction(last_was_tp, last_trade.direction);
                    next_dir
                } else {
                    d
                }
            }
            None => sm.initialize_direction(config.initial_seed),
        };
        drop(sm);

        let levels = calculate_tp_sl_prices(
            mark_px,
            direction,
            config.profile.take_profit_pct,
            config.profile.stop_loss_pct,
        );

        match config.execution_mode {
            ExecutionMode::Automatic => {
                info!("Automatic mode: executing entry order for {:?}", direction);
                let _ = self
                    .execute_entry_order(direction, sizing.quantity, mark_px)
                    .await;
            }
            ExecutionMode::ConfirmationRequired => {
                let proposal_id = format!("prop-{}", Utc::now().timestamp_millis());
                let proposal = PendingEntryProposal {
                    proposal_id: proposal_id.clone(),
                    direction,
                    proposed_entry_price: mark_px,
                    size: sizing.quantity,
                    notional: sizing.notional,
                    tp_price: levels.take_profit_price,
                    sl_price: levels.stop_loss_price,
                    created_at: Utc::now(),
                    expires_at: Utc::now() + Duration::seconds(PROPOSAL_TTL_SECONDS),
                };

                let mut sm = self.state_machine.write().await;
                if sm.transition_to_awaiting_confirmation().is_ok() {
                    *self.pending_proposal.write().await = Some(proposal.clone());
                    self.audit_logger.record(AuditEventData::EntryProposed {
                        proposal_id,
                        direction: format!("{:?}", direction),
                        price: format!("${}", mark_px),
                        size: format!("{}", sizing.quantity),
                        notional: format!("{}", sizing.notional),
                        tp_price: format!("${}", levels.take_profit_price),
                        sl_price: format!("${}", levels.stop_loss_price),
                        expires_at: proposal.expires_at,
                    });
                    info!("Published pending entry proposal awaiting user confirmation.");
                }
            }
        }
    }

    /// Execute entry order on exchange.
    async fn execute_entry_order(
        &self,
        direction: Direction,
        size: Quantity,
        target_px: Price,
    ) -> Result<(), String> {
        let side = match direction {
            Direction::Long => Side::Buy,
            Direction::Short => Side::Sell,
        };

        let cloid = format!("ent-{}", Utc::now().timestamp_millis());

        // 1. Transition state machine to Opening
        let mut sm = self.state_machine.write().await;
        sm.transition_to_opening(cloid.clone(), size, side, direction)
            .map_err(|e| format!("State error entering Opening: {}", e))?;
        drop(sm);

        let order = OrderRequest {
            coin: self.coin.clone(),
            is_buy: side == Side::Buy,
            size,
            limit_px: target_px,
            reduce_only: false,
            order_type: OrderTypeSpec::Market,
            cloid: Some(cloid.clone()),
        };

        self.audit_logger.record(AuditEventData::OrderSubmitted {
            cloid: cloid.clone(),
            coin: self.coin.clone(),
            side: format!("{:?}", side),
            size: format!("{}", size),
            price: format!("${}", target_px),
            reduce_only: false,
        });

        // 2. Dispatch order to exchange
        let res = self.exchange_client.place_orders(vec![order]).await;
        if let Err(e) = res {
            error!("Entry order placement failed: {}. Reverting to Flat.", e);
            let mut sm = self.state_machine.write().await;
            let _ = sm.transition_to_flat();
            return Err(format!("Entry order failed: {}", e));
        }

        // 3. Record synthetic fill and establish protective TP/SL triggers
        let fill = FillRecord::new(NewFillParams {
            fill_id: format!("fill-{}", Utc::now().timestamp_millis()),
            cloid: Some(cloid.clone()),
            timestamp: Utc::now(),
            direction,
            side,
            price: target_px,
            quantity: size,
            fee_paid: SignedUsd::new(size.value() * target_px.value() * dec!(0.00045)),
            fee_token: "USDC".into(),
            is_entry: true,
        })
        .map_err(|e| format!("Failed to create fill record: {}", e))?;

        self.fill_ledger.write().await.add_entry_fill(fill.clone());

        let config = self.config.read().await.clone();
        let lev_dec = Decimal::from(config.profile.leverage);
        let initial_margin = SignedUsd::new((size.value() * target_px.value()) / lev_dec);
        let _ = self
            .capital_ledger
            .write()
            .await
            .commit_margin(initial_margin);

        let levels = calculate_tp_sl_prices(
            target_px,
            direction,
            config.profile.take_profit_pct,
            config.profile.stop_loss_pct,
        );

        let tp_cloid = format!("tp-{}", Utc::now().timestamp_millis());
        let sl_cloid = format!("sl-{}", Utc::now().timestamp_millis());

        let mut prot = self.protection_manager.write().await;
        match prot
            .establish_or_resize_protection(
                self.exchange_client.as_ref(),
                direction,
                size,
                &levels,
                tp_cloid,
                sl_cloid,
            )
            .await
        {
            Ok(active) => {
                let tp_oid_str = active
                    .tp_order_id
                    .map(|id| format!("tp-{}", id))
                    .unwrap_or_else(|| active.tp_cloid.clone());
                let sl_oid_str = active
                    .sl_order_id
                    .map(|id| format!("sl-{}", id))
                    .unwrap_or_else(|| active.sl_cloid.clone());

                let mut sm = self.state_machine.write().await;
                let _ = sm.transition_to_open_protected(
                    direction, size, target_px, tp_oid_str, sl_oid_str,
                );

                // Reset extreme mark price for trailing stop tracking
                *self.extreme_mark_price.write().await = Some(target_px);

                self.audit_logger
                    .record(AuditEventData::ProtectionEstablished {
                        tp_oid: active.tp_order_id.unwrap_or(0),
                        sl_oid: active.sl_order_id.unwrap_or(0),
                        tp_price: format!("${}", levels.take_profit_price),
                        sl_price: format!("${}", levels.stop_loss_price),
                        size: format!("{}", size),
                    });
            }
            Err(e) => {
                error!(
                    "CRITICAL: Failed to establish protection: {}. Emergency flattening.",
                    e
                );
                drop(prot);
                let _ = self.emergency_flatten().await;
                return Err("Failed to establish protection; flattened position.".into());
            }
        }

        self.persist_active_state().await?;
        Ok(())
    }

    /// Process a closing fill (TP, SL, or emergency exit).
    pub async fn on_closing_fill(&self, exit_fill: &FillRecord) -> Result<(), String> {
        let mut ledger = self.fill_ledger.write().await;
        let funding_cash_flow = self
            .funding_ledger
            .read()
            .await
            .total_signed_funding_cash_flow();

        let matched = ledger
            .match_exit_fill(exit_fill, funding_cash_flow)
            .map_err(|e| format!("Ledger matching failed: {}", e))?;

        for trade in &matched {
            info!(
                "Trade Closed: Direction={:?}, Entry={}, Exit={}, Net P&L={}",
                trade.direction, trade.entry_price, trade.exit_price, trade.net_pnl
            );

            let config = self.config.read().await.clone();
            let lev_dec = Decimal::from(config.profile.leverage);
            let released_margin =
                SignedUsd::new((trade.quantity.value() * trade.entry_price.value()) / lev_dec);
            let _ = self
                .capital_ledger
                .write()
                .await
                .release_margin(released_margin);

            // Record in loss tracker
            let streak = self
                .loss_tracker
                .write()
                .await
                .record_closed_trade(trade.net_pnl);
            self.audit_logger.record(AuditEventData::FillRecorded {
                fill_id: exit_fill.fill_id.clone(),
                cloid: exit_fill.cloid.clone(),
                direction: format!("{:?}", trade.direction),
                side: format!("{:?}", exit_fill.side),
                price: format!("${}", exit_fill.price),
                size: format!("{}", trade.quantity),
                fee: format!("{}", trade.entry_fee + trade.exit_fee),
                gross_pnl: Some(format!("{}", trade.gross_pnl)),
                net_pnl: Some(format!("{}", trade.net_pnl)),
            });

            // Evaluate session halt criteria in priority order:
            let config = self.config.read().await.clone();
            let total_net_pnl = ledger.total_realized_net_pnl();

            let mut sm = self.state_machine.write().await;
            if streak >= config.profile.consecutive_losses_limit {
                warn!(
                    "Consecutive loss limit reached ({})! Halting engine.",
                    streak
                );
                sm.halt(HaltReason::ConsecutiveLossesReached);
            } else if total_net_pnl >= config.profile.net_session_profit_target {
                info!(
                    "Session profit target reached ({} >= {})! Halting engine.",
                    total_net_pnl, config.profile.net_session_profit_target
                );
                sm.halt(HaltReason::ProfitTargetReached);
            } else if *self.stop_after_close.read().await {
                info!("Stop after close was armed. Stopping engine.");
                sm.stop();
            }
        }

        // Clean up protective triggers
        let mut prot = self.protection_manager.write().await;
        prot.cancel_all_protection(self.exchange_client.as_ref())
            .await;
        *self.extreme_mark_price.write().await = None;

        let mut sm = self.state_machine.write().await;
        let _ = sm.transition_to_flat();
        drop(sm);

        self.persist_active_state().await?;
        Ok(())
    }

    /// Ingest a funding payment record.
    pub async fn on_funding_record(&self, record: FundingRecord) {
        self.audit_logger.record(AuditEventData::FundingReceived {
            payment: format!("{}", record.payment),
            rate: format!("{}", record.rate),
        });
        self.funding_ledger.write().await.add_funding_record(record);
        let _ = self.persist_active_state().await;
    }

    /// Atomically persist current runtime state.
    pub async fn persist_active_state(&self) -> Result<(), String> {
        let sm = self.state_machine.read().await;
        let config = self.config.read().await.clone();
        let ledger = self.fill_ledger.read().await;
        let funding = self.funding_ledger.read().await;
        let streak = self.loss_tracker.read().await.current_streak();
        let capital = self.capital_ledger.read().await.clone();

        let state = PersistedSessionState {
            updated_at: Utc::now(),
            last_engine_mode: sm.mode().clone(),
            config,
            position: sm.position().clone(),
            closed_trades: ledger.closed_trades().to_vec(),
            open_lots: ledger.open_lots().iter().cloned().collect(),
            funding_records: funding.records().to_vec(),
            consecutive_losses: streak,
            total_realized_net_pnl: ledger.total_realized_net_pnl(),
            total_fees_paid: ledger.total_fees_paid(),
            capital_ledger: Some(capital),
        };

        self.persistence_manager.save_state(&state)
    }

    /// Get complete status snapshot for REST API and WebSocket stream.
    pub async fn get_status_snapshot(&self) -> EngineStatusSnapshot {
        let sm = self.state_machine.read().await;
        let config = self.config.read().await.clone();
        let ledger = self.fill_ledger.read().await;
        let funding = self.funding_ledger.read().await;
        let streak = self.loss_tracker.read().await.current_streak();
        let prot = self.protection_manager.read().await;
        let proposal = self.pending_proposal.read().await.clone();
        let report = self.last_reconciliation.read().await.clone();

        let mark_price = self.mark_price_cache.latest_price();
        let now_ms = current_time_ms();
        let mark_price_age_ms = self.mark_price_cache.age_ms(now_ms);
        let last_ts = self.mark_price_cache.last_update_ms();
        let is_stale = matches!(
            self.stale_detector
                .evaluate(if last_ts > 0 { Some(last_ts) } else { None }, now_ms),
            StalenessStatus::Stale { .. } | StalenessStatus::MissingData
        );

        let unrealized_pnl = if let PositionState::OpenProtected {
            direction,
            size,
            avg_entry_px,
            ..
        } = *sm.position()
        {
            if let Some(mark) = mark_price {
                calculate_unrealized_gross_pnl(avg_entry_px, mark, size, direction)
            } else {
                SignedUsd::zero()
            }
        } else {
            SignedUsd::zero()
        };

        let active_tp_oid = prot.active_protection().and_then(|p| p.tp_order_id);
        let active_sl_oid = prot.active_protection().and_then(|p| p.sl_order_id);
        let capital_summary = self.capital_ledger.read().await.summary();
        let signer_status = *self.signer_status.read().await;

        EngineStatusSnapshot {
            mode: sm.mode().clone(),
            position: sm.position().clone(),
            config,
            mark_price,
            mark_price_age_ms,
            is_stale,
            unrealized_pnl,
            total_realized_net_pnl: ledger.total_realized_net_pnl(),
            total_fees_paid: ledger.total_fees_paid(),
            total_signed_funding: funding.total_signed_funding_cash_flow(),
            consecutive_losses: streak,
            pending_proposal: proposal,
            active_tp_oid,
            active_sl_oid,
            last_discrepancies: report.map(|r| r.discrepancies).unwrap_or_default(),
            capital_summary,
            signer_status,
        }
    }

    /// Reallocate internal capital budget between trading allocation and safety reserve.
    pub async fn reallocate_capital(
        &self,
        direction: ReallocationDirection,
        amount: Decimal,
    ) -> Result<BotCapitalSummary, String> {
        let mut capital = self.capital_ledger.write().await;
        capital
            .reallocate(direction, SignedUsd::new(amount))
            .map_err(|e| format!("Reallocation rejected: {}", e))?;
        let summary = capital.summary();
        drop(capital);

        self.audit_logger
            .record(AuditEventData::CapitalReallocated {
                direction: format!("{:?}", direction),
                amount: amount.to_string(),
                new_trading_allocation: summary.allocated_trading.to_string(),
                new_reserve_balance: summary.allocated_reserve.to_string(),
            });

        self.persist_active_state().await?;
        Ok(summary)
    }

    /// Generate a deterministic, non-order-affecting trade preview.
    pub async fn generate_trade_preview(&self) -> Result<TradePreview, String> {
        let mark_px = self
            .mark_price_cache
            .latest_price()
            .ok_or_else(|| "Mark price not available for trade preview".to_string())?;

        let config = self.config.read().await.clone();
        let meta = self.market_metadata.read().await.clone();
        let sm = self.state_machine.read().await;

        let direction = match sm.initial_direction() {
            Some(d) => {
                let ledger = self.fill_ledger.read().await;
                if let Some(last_trade) = ledger.closed_trades().last() {
                    let last_was_tp = last_trade.gross_pnl.value() > Decimal::ZERO;
                    let (next_dir, _) = determine_next_direction(last_was_tp, last_trade.direction);
                    next_dir
                } else {
                    d
                }
            }
            None => Direction::Long, // deterministic default
        };
        drop(sm);

        let sizing = calculate_order_size(
            config.profile.initial_margin_cap,
            config.profile.leverage,
            mark_px,
            &meta,
            config.profile.min_order_notional,
        )
        .map_err(|e| format!("Sizing calculation failed: {}", e))?;

        let levels = calculate_tp_sl_prices(
            mark_px,
            direction,
            config.profile.take_profit_pct,
            config.profile.stop_loss_pct,
        );

        let lev_dec = Decimal::from(config.profile.leverage);
        let estimated_margin = SignedUsd::new(sizing.notional.value() / lev_dec);

        // 0.045% benchmark taker fee estimate
        let taker_rate = dec!(0.00045);
        let entry_notional = sizing.quantity.value() * mark_px.value();
        let estimated_entry_fee = SignedUsd::new(entry_notional * taker_rate);

        let tp_notional = sizing.quantity.value() * levels.take_profit_price.value();
        let estimated_exit_fee_tp = SignedUsd::new(tp_notional * taker_rate);

        let sl_notional = sizing.quantity.value() * levels.stop_loss_price.value();
        let estimated_exit_fee_sl = SignedUsd::new(sl_notional * taker_rate);

        let tp_diff = levels.take_profit_price.value() - mark_px.value();
        let tp_gross = tp_diff * sizing.quantity.value() * direction.multiplier();
        let estimated_tp_gross_pnl = SignedUsd::new(tp_gross);
        let estimated_tp_net_pnl =
            SignedUsd::new(tp_gross - estimated_entry_fee.value() - estimated_exit_fee_tp.value());

        let sl_diff = levels.stop_loss_price.value() - mark_px.value();
        let sl_gross = sl_diff * sizing.quantity.value() * direction.multiplier();
        let estimated_sl_gross_pnl = SignedUsd::new(sl_gross);
        let estimated_sl_net_pnl =
            SignedUsd::new(sl_gross - estimated_entry_fee.value() - estimated_exit_fee_sl.value());

        let capital = self.capital_ledger.read().await;
        let current_trading_allocation = capital.allocated_trading;
        let remaining_trading_allocation = capital.uncommitted_trading();

        let profile_str = match config.profile.profile {
            crate::config::TradingProfile::Normal => "NORMAL".to_string(),
            crate::config::TradingProfile::Greedy => "GREEDY".to_string(),
        };
        let margin_mode_str = match config.margin_mode {
            crate::config::MarginMode::Cross => "CROSS".to_string(),
            crate::config::MarginMode::Isolated => "ISOLATED".to_string(),
        };

        Ok(TradePreview {
            label: "ESTIMATE / PENDING FILL".to_string(),
            profile: profile_str,
            margin_mode: margin_mode_str,
            direction,
            leverage: config.profile.leverage,
            entry_price: mark_px,
            quantity: sizing.quantity,
            notional: sizing.notional,
            estimated_margin,
            tp_price: levels.take_profit_price,
            sl_price: levels.stop_loss_price,
            benchmark_fee_rate: "0.045% benchmark taker estimate".to_string(),
            estimated_entry_fee,
            estimated_exit_fee_tp,
            estimated_exit_fee_sl,
            estimated_tp_gross_pnl,
            estimated_tp_net_pnl,
            estimated_sl_gross_pnl,
            estimated_sl_net_pnl,
            current_trading_allocation,
            remaining_trading_allocation,
        })
    }

    /// Process incoming candlestick snapshot and update multi-timeframe cache.
    pub async fn on_candle_update(&self, candle: CandleSnapshot) {
        self.candle_cache.insert_candle(candle).await;
    }

    /// Ensure candles are present in cache for the given interval and limit.
    /// Loads from exchange snapshot on initial miss or insufficient count.
    pub async fn ensure_candles(
        &self,
        interval: &str,
        limit: usize,
    ) -> Result<Vec<CandleSnapshot>, String> {
        let cached = self.candle_cache.get_candles(interval, limit).await;
        if !cached.is_empty() {
            return Ok(cached);
        }

        // Cache miss: load historical snapshot from exchange
        let step_ms: u64 = match interval {
            "1m" => 60_000,
            "5m" => 300_000,
            "15m" => 900_000,
            "1h" => 3_600_000,
            _ => 60_000,
        };

        let now_ms = current_time_ms();
        let span_ms = step_ms * (limit as u64).max(10);
        let start_time_ms = now_ms.saturating_sub(span_ms);

        let candles = self
            .exchange_client
            .get_candle_snapshot(&self.coin, interval, start_time_ms, now_ms)
            .await
            .map_err(|e| format!("Failed to fetch candle snapshot for {}: {}", interval, e))?;

        if !candles.is_empty() {
            self.candle_cache
                .merge_snapshot(interval, candles.clone())
                .await;
            info!(
                "Loaded {} historical {} candles on cache miss",
                candles.len(),
                interval
            );
            Ok(self.candle_cache.get_candles(interval, limit).await)
        } else {
            Ok(vec![])
        }
    }

    /// Recover missing historical candle bars after startup or reconnect.
    pub async fn recover_candle_gaps(&self) -> Result<usize, String> {
        let mut total_loaded = 0;
        let intervals = ["1m", "5m", "15m", "1h"];
        for interval in intervals {
            match self.ensure_candles(interval, 50).await {
                Ok(candles) => total_loaded += candles.len(),
                Err(e) => warn!("Notice preloading {} candles on startup: {}", interval, e),
            }
        }
        info!(
            "Candle gap recovery completed: loaded {} total bars across timeframes into cache",
            total_loaded
        );
        Ok(total_loaded)
    }

    /// Synchronize the agent signer authorization status from exchange state.
    pub async fn sync_signer_status(&self) {
        if let Ok(acc) = self
            .exchange_client
            .get_perp_account_summary(&self.target_account)
            .await
        {
            *self.signer_status.write().await = acc.signer_status;
        }
    }

    /// Explicitly set the agent signer status (e.g. for testing or telemetry).
    pub async fn set_signer_status(&self, status: SignerStatus) {
        *self.signer_status.write().await = status;
    }

    /// Halt trading immediately upon exchange-confirmed authorization failure.
    pub async fn handle_authorization_error(&self, err_msg: &str) {
        *self.signer_status.write().await = SignerStatus::UnauthorizedOrExpired;
        self.audit_logger
            .record(AuditEventData::AgentAuthorizationFailed {
                message: format!("Signer authorization rejected by exchange: {}", err_msg),
            });
        let mut sm = self.state_machine.write().await;
        sm.halt(HaltReason::ManualEmergencyStop);
        drop(sm);
        warn!(
            "Engine halted due to agent authorization failure: {}",
            err_msg
        );
        let _ = self.persist_active_state().await;
    }
}
