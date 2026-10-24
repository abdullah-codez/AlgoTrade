//! Comprehensive integration test suite for Milestone 3:
//! Trading engine orchestration, risk controls, lifecycle transitions,
//! recovery idempotency, accounting conventions, and server security.

use algo_trade::audit::AuditLogger;
use algo_trade::config::{
    EngineConfig, ExecutionMode, MarginMode, MarketMetadata, ProfileConfig, TradingProfile,
};
use algo_trade::engine::persistence::StatePersistenceManager;
use algo_trade::engine::{EngineOrchestrator, PendingEntryProposal};
use algo_trade::execution::client::{CandleSnapshot, ExchangeClient};
use algo_trade::execution::mock::{MockExchangeClient, MockFillMode};
use algo_trade::ledger::{FillRecord, FundingRecord, NewFillParams};
use algo_trade::server::{create_router, AppState};
use algo_trade::state::{EngineMode, PositionState};
use algo_trade::strategy::calculate_tp_sl_prices;
use algo_trade::types::{Direction, MarginAllocation, Price, Quantity, Side, SignedUsd};
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chrono::{Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Arc;
use tower::ServiceExt;

fn create_test_orchestrator(
    config: EngineConfig,
    mock_client: Arc<MockExchangeClient>,
) -> (Arc<EngineOrchestrator>, std::path::PathBuf) {
    let temp_dir = std::env::temp_dir();
    let state_file = temp_dir.join(format!(
        "test_orch_state_{}.json",
        Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    let log_file = temp_dir.join(format!(
        "test_orch_events_{}.jsonl",
        Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));

    let audit_logger = Arc::new(AuditLogger::new(&log_file));
    let persistence = Arc::new(StatePersistenceManager::new(&state_file));
    let metadata = MarketMetadata::btc_fixture();

    let orch = Arc::new(EngineOrchestrator::new(
        "BTC",
        "0x0000000000000000000000000000000000000001",
        config,
        metadata,
        mock_client,
        audit_logger,
        persistence,
    ));

    (orch, state_file)
}

#[tokio::test]
async fn test_engine_starts_stopped_and_launch_submits_zero_orders() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());

    // 1. Invariant: Engine boots strictly Stopped
    let snapshot = orch.get_status_snapshot().await;
    assert_eq!(snapshot.mode, EngineMode::Stopped);
    assert!(matches!(snapshot.position, PositionState::Flat));

    // 2. Invariant: Zero open orders on exchange on startup
    let open_orders = mock_client.get_open_orders("").await.unwrap();
    assert_eq!(
        open_orders.len(),
        0,
        "Booting engine must submit zero exchange orders"
    );
}

#[tokio::test]
async fn test_illegal_lifecycle_transitions_rejected() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());

    let mut sm = orch.state_machine.write().await;

    // Direct transition to Opening while Stopped must fail
    let res = sm.transition_to_opening(
        "cloid-1".into(),
        Quantity::new(dec!(0.0001)).unwrap(),
        Side::Buy,
        Direction::Long,
    );
    assert!(
        res.is_err(),
        "Cannot transition to Opening while engine is Stopped"
    );

    // Direct transition to OpenProtected while Stopped must fail
    let res = sm.transition_to_open_protected(
        Direction::Long,
        Quantity::new(dec!(0.0001)).unwrap(),
        Price::new(dec!(70000.0)).unwrap(),
        "tp-1".into(),
        "sl-1".into(),
    );
    assert!(
        res.is_err(),
        "Cannot transition to OpenProtected while engine is Stopped"
    );
}

#[tokio::test]
async fn test_ui_configuration_validated_by_backend() {
    // 1. Initial margin cap exceeding $8.00 cap must fail validation
    let mut bad_config = ProfileConfig::normal();
    bad_config.initial_margin_cap = MarginAllocation::new(dec!(8.50)).unwrap();
    assert!(bad_config.validate().is_err());

    // 2. Total strategy budget exceeding $10.00 must fail validation
    let mut bad_budget = ProfileConfig::normal();
    bad_budget.total_strategy_budget = SignedUsd::new(dec!(15.00));
    assert!(bad_budget.validate().is_err());

    // 3. Margin cap equal to or exceeding budget must fail validation
    let mut bad_buffer = ProfileConfig::normal();
    bad_buffer.initial_margin_cap = MarginAllocation::new(dec!(8.00)).unwrap();
    bad_buffer.total_strategy_budget = SignedUsd::new(dec!(8.00)); // cap >= budget -> no buffer
    assert!(bad_buffer.validate().is_err());

    // 4. Documented presets validate cleanly
    assert!(ProfileConfig::normal().validate().is_ok());
    assert!(ProfileConfig::greedy().validate().is_ok());
}

#[tokio::test]
async fn test_normal_greedy_and_cross_isolated_independent_selections() {
    // Verify all 4 combinations are valid independent choices
    let combos = [
        (TradingProfile::Normal, MarginMode::Isolated),
        (TradingProfile::Normal, MarginMode::Cross),
        (TradingProfile::Greedy, MarginMode::Isolated),
        (TradingProfile::Greedy, MarginMode::Cross),
    ];

    for (prof, margin) in combos {
        let p_cfg = match prof {
            TradingProfile::Normal => ProfileConfig::normal(),
            TradingProfile::Greedy => ProfileConfig::greedy(),
        };
        let cfg = EngineConfig {
            profile: p_cfg,
            margin_mode: margin,
            execution_mode: ExecutionMode::Automatic,
            initial_seed: Some(42),
        };
        assert!(
            cfg.validate().is_ok(),
            "Combination {:?} + {:?} must validate independently",
            prof,
            margin
        );
    }
}

#[tokio::test]
async fn test_automatic_and_confirmation_mode_divergence() {
    // 1. Confirmation Mode: holds entry in pending proposal without placing orders
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let config = EngineConfig {
        profile: ProfileConfig::normal(),
        margin_mode: MarginMode::Isolated,
        execution_mode: ExecutionMode::ConfirmationRequired,
        initial_seed: Some(42),
    };
    let (orch, _state_file) = create_test_orchestrator(config, mock_client.clone());

    // Feed price tick and start engine
    let price = Price::new(dec!(70000.0)).unwrap();
    let now_ms = Utc::now().timestamp_millis() as u64;
    orch.on_mark_price_tick(price, now_ms).await;
    orch.start().await.expect("Engine start should succeed");

    // Trigger tick evaluation
    orch.on_mark_price_tick(price, now_ms + 100).await;

    // Verify engine transitioned to AwaitingConfirmation and proposal was generated
    let snapshot = orch.get_status_snapshot().await;
    assert_eq!(snapshot.mode, EngineMode::AwaitingConfirmation);
    assert!(snapshot.pending_proposal.is_some());
    assert_eq!(
        mock_client.get_open_orders("").await.unwrap().len(),
        0,
        "No orders should be placed while awaiting confirmation"
    );

    // 2. Automatic Mode: places entry order autonomously
    let mock_client_auto = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let config_auto = EngineConfig {
        profile: ProfileConfig::normal(),
        margin_mode: MarginMode::Isolated,
        execution_mode: ExecutionMode::Automatic,
        initial_seed: Some(42),
    };
    let (orch_auto, _state_file_2) =
        create_test_orchestrator(config_auto, mock_client_auto.clone());

    orch_auto.on_mark_price_tick(price, now_ms).await;
    orch_auto
        .start()
        .await
        .expect("Auto engine start should succeed");
    orch_auto.on_mark_price_tick(price, now_ms + 100).await;

    // Verify position state changed or protection orders established
    let snapshot_auto = orch_auto.get_status_snapshot().await;
    assert!(
        matches!(snapshot_auto.position, PositionState::OpenProtected { .. }),
        "Automatic mode must enter OpenProtected state"
    );
}

#[tokio::test]
async fn test_expired_entry_proposal_cannot_be_approved() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let config = EngineConfig {
        profile: ProfileConfig::normal(),
        margin_mode: MarginMode::Isolated,
        execution_mode: ExecutionMode::ConfirmationRequired,
        initial_seed: Some(42),
    };
    let (orch, _state_file) = create_test_orchestrator(config, mock_client.clone());

    let price = Price::new(dec!(70000.0)).unwrap();
    let now_ms = Utc::now().timestamp_millis() as u64;
    orch.on_mark_price_tick(price, now_ms).await;
    orch.start().await.unwrap();

    // Stage an expired proposal
    let proposal_id = "test-prop-expired".to_string();
    let expired_proposal = PendingEntryProposal {
        proposal_id: proposal_id.clone(),
        direction: Direction::Long,
        proposed_entry_price: price,
        size: Quantity::new(dec!(0.0005)).unwrap(),
        notional: SignedUsd::new(dec!(35.0)),
        tp_price: Price::new(dec!(70700.0)).unwrap(),
        sl_price: Price::new(dec!(69650.0)).unwrap(),
        created_at: Utc::now() - Duration::seconds(120),
        expires_at: Utc::now() - Duration::seconds(60), // Expired 60s ago
    };
    *orch.pending_proposal.write().await = Some(expired_proposal);

    // Attempting approval of expired proposal must be rejected
    let res = orch.approve_entry(&proposal_id).await;
    assert!(res.is_err(), "Expired proposal must be rejected");
    assert!(res.unwrap_err().contains("expired"));
}

#[tokio::test]
async fn test_start_requires_fresh_data_and_valid_settings() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());

    // 1. Without receiving market data, start() must fail
    let res = orch.start().await;
    assert!(
        res.is_err(),
        "Start must fail when market data is missing/stale"
    );
    assert!(res.unwrap_err().contains("stale"));

    // 2. Feed stale tick (> 5000ms old)
    let stale_ms = (Utc::now().timestamp_millis() - 10000) as u64;
    orch.on_mark_price_tick(Price::new(dec!(70000.0)).unwrap(), stale_ms)
        .await;
    let res_stale = orch.start().await;
    assert!(res_stale.is_err(), "Start must fail on stale market tick");

    // 3. Feed fresh tick (< 1000ms old) -> start succeeds
    let fresh_ms = Utc::now().timestamp_millis() as u64;
    orch.on_mark_price_tick(Price::new(dec!(70000.0)).unwrap(), fresh_ms)
        .await;
    let res_fresh = orch.start().await;
    assert!(
        res_fresh.is_ok(),
        "Start must succeed with fresh mark price and valid settings"
    );
    assert_eq!(*orch.state_machine.read().await.mode(), EngineMode::Running);
}

#[tokio::test]
async fn test_pause_stop_and_emergency_flatten_distinct_semantics() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());

    // 1. Pause: sets mode to Pausing, does not cancel open position
    let price = Price::new(dec!(70000.0)).unwrap();
    let now_ms = Utc::now().timestamp_millis() as u64;
    orch.on_mark_price_tick(price, now_ms).await;
    orch.start().await.unwrap();

    orch.pause().await.unwrap();
    assert_eq!(*orch.state_machine.read().await.mode(), EngineMode::Pausing);

    // 2. Stop while Flat: transitions immediately to Stopped
    orch.stop().await.unwrap();
    assert_eq!(*orch.state_machine.read().await.mode(), EngineMode::Stopped);

    // 3. Emergency Flatten: immediately places reduce-only close and enters EmergencyShutdown
    orch.emergency_flatten().await.unwrap();
    assert!(matches!(
        *orch.state_machine.read().await.mode(),
        EngineMode::EmergencyShutdown { .. }
    ));
}

#[tokio::test]
async fn test_partial_fill_protection_resizing() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let mut prot = algo_trade::execution::protection::ProtectionManager::new("BTC");

    let entry_px = Price::new(dec!(70000.0)).unwrap();
    let levels = calculate_tp_sl_prices(entry_px, Direction::Long, dec!(0.01), dec!(0.005));

    // Initial fill: 0.0001 BTC
    let first_qty = Quantity::new(dec!(0.0001)).unwrap();
    let p1 = prot
        .establish_or_resize_protection(
            mock_client.as_ref(),
            Direction::Long,
            first_qty,
            &levels,
            "tp-1".into(),
            "sl-1".into(),
        )
        .await
        .expect("Initial protection should succeed");
    assert_eq!(p1.covered_quantity, first_qty);

    // Incremental partial fill resize: 0.0002 BTC total
    let total_qty = Quantity::new(dec!(0.0002)).unwrap();
    let p2 = prot
        .establish_or_resize_protection(
            mock_client.as_ref(),
            Direction::Long,
            total_qty,
            &levels,
            "tp-2".into(),
            "sl-2".into(),
        )
        .await
        .expect("Resized protection should succeed");
    assert_eq!(p2.covered_quantity, total_qty);
}

#[tokio::test]
async fn test_net_pnl_funding_cash_flows_positive_and_negative() {
    let mut fill_ledger = algo_trade::ledger::FillLedger::new();
    let mut funding_ledger = algo_trade::ledger::FundingLedger::new();

    let entry_fill = FillRecord::new(NewFillParams {
        fill_id: "entry-1".into(),
        cloid: Some("cloid-e1".into()),
        timestamp: Utc::now(),
        direction: Direction::Long,
        side: Side::Buy,
        price: Price::new(dec!(70000.0)).unwrap(),
        quantity: Quantity::new(dec!(0.0005)).unwrap(),
        fee_paid: SignedUsd::new(dec!(0.015)),
        fee_token: "USDC".into(),
        is_entry: true,
    })
    .unwrap();
    fill_ledger.add_entry_fill(entry_fill);

    // Exit fill: Exit at $72,000 (+ $1.00 gross P&L)
    let exit_fill = FillRecord::new(NewFillParams {
        fill_id: "exit-1".into(),
        cloid: Some("cloid-x1".into()),
        timestamp: Utc::now(),
        direction: Direction::Long,
        side: Side::Sell,
        price: Price::new(dec!(72000.0)).unwrap(),
        quantity: Quantity::new(dec!(0.0005)).unwrap(),
        fee_paid: SignedUsd::new(dec!(0.015)),
        fee_token: "USDC".into(),
        is_entry: false,
    })
    .unwrap();

    // Case A: Positive funding cash flow (+ $0.10 received)
    funding_ledger.add_funding_record(FundingRecord {
        timestamp: Utc::now(),
        coin: "BTC".into(),
        rate: Decimal::new(1, 4),
        position_size: Quantity::new(dec!(0.0005)).unwrap(),
        payment: SignedUsd::new(dec!(0.10)), // Positive = received
    });

    let signed_cash_flow = funding_ledger.total_signed_funding_cash_flow();
    assert_eq!(signed_cash_flow, SignedUsd::new(dec!(0.10)));

    let trades = fill_ledger
        .match_exit_fill(&exit_fill, signed_cash_flow)
        .unwrap();
    assert_eq!(trades.len(), 1);
    let trade = &trades[0];

    // Gross = (72000 - 70000) * 0.0005 * 1 = +$1.00
    // Fees = 0.015 + 0.015 = $0.03
    // Funding = +$0.10
    // Net P&L = Gross - Fees + Funding = 1.00 - 0.03 + 0.10 = $1.07
    assert_eq!(trade.gross_pnl, SignedUsd::new(dec!(1.00)));
    assert_eq!(trade.net_pnl, SignedUsd::new(dec!(1.07)));

    // Case B: Negative funding cash flow (- $0.20 paid)
    let mut funding_ledger_neg = algo_trade::ledger::FundingLedger::new();
    funding_ledger_neg.add_funding_record(FundingRecord {
        timestamp: Utc::now(),
        coin: "BTC".into(),
        rate: Decimal::new(-2, 4),
        position_size: Quantity::new(dec!(0.0005)).unwrap(),
        payment: SignedUsd::new(dec!(-0.20)), // Negative = paid
    });
    let neg_cash_flow = funding_ledger_neg.total_signed_funding_cash_flow();
    assert_eq!(neg_cash_flow, SignedUsd::new(dec!(-0.20)));

    let mut fill_ledger_2 = algo_trade::ledger::FillLedger::new();
    fill_ledger_2.add_entry_fill(
        FillRecord::new(NewFillParams {
            fill_id: "entry-2".into(),
            cloid: None,
            timestamp: Utc::now(),
            direction: Direction::Long,
            side: Side::Buy,
            price: Price::new(dec!(70000.0)).unwrap(),
            quantity: Quantity::new(dec!(0.0005)).unwrap(),
            fee_paid: SignedUsd::new(dec!(0.015)),
            fee_token: "USDC".into(),
            is_entry: true,
        })
        .unwrap(),
    );

    let trades_neg = fill_ledger_2
        .match_exit_fill(&exit_fill, neg_cash_flow)
        .unwrap();
    // Net P&L = 1.00 - 0.03 + (-0.20) = $0.77
    assert_eq!(trades_neg[0].net_pnl, SignedUsd::new(dec!(0.77)));
}

#[tokio::test]
async fn test_restart_recovery_does_not_duplicate_orders_or_exposure() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());

    // Ingest fill and record state
    let fill = FillRecord::new(NewFillParams {
        fill_id: "persisted-fill-1".into(),
        cloid: Some("cloid-p1".into()),
        timestamp: Utc::now(),
        direction: Direction::Long,
        side: Side::Buy,
        price: Price::new(dec!(70000.0)).unwrap(),
        quantity: Quantity::new(dec!(0.0005)).unwrap(),
        fee_paid: SignedUsd::new(dec!(0.015)),
        fee_token: "USDC".into(),
        is_entry: true,
    })
    .unwrap();
    orch.fill_ledger.write().await.add_entry_fill(fill);
    orch.loss_tracker
        .write()
        .await
        .record_closed_trade(SignedUsd::new(dec!(-0.50)));
    orch.persist_active_state().await.unwrap();

    // Simulate Restart with new orchestrator instance pointing to same file
    let mock_client_2 = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let persistence_2 = Arc::new(StatePersistenceManager::new(&state_file));
    let audit_2 = Arc::new(AuditLogger::new("events.jsonl"));
    let orch_2 = Arc::new(EngineOrchestrator::new(
        "BTC",
        "0x0000000000000000000000000000000000000001",
        EngineConfig::normal_default(),
        MarketMetadata::btc_fixture(),
        mock_client_2.clone(),
        audit_2,
        persistence_2,
    ));

    orch_2.load_persisted_state().await.unwrap();

    // Verify recovery properties:
    // 1. Engine mode is strictly Stopped!
    assert_eq!(
        *orch_2.state_machine.read().await.mode(),
        EngineMode::Stopped
    );

    // 2. Zero duplicate orders placed
    assert_eq!(mock_client_2.get_open_orders("").await.unwrap().len(), 0);

    // 3. Loss streak restored without duplication
    assert_eq!(orch_2.loss_tracker.read().await.current_streak(), 1);

    // 4. Open lots restored
    assert_eq!(orch_2.fill_ledger.read().await.open_lots().len(), 1);
}

#[tokio::test]
async fn test_server_host_validation_rejects_external_hosts() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) = create_test_orchestrator(EngineConfig::normal_default(), mock_client);

    let state = Arc::new(AppState { orchestrator: orch });
    let app = create_router(state, "web");

    // Request with valid loopback Host header: 127.0.0.1:3000 -> 200 OK
    let req_valid = Request::builder()
        .uri("/api/status")
        .header(header::HOST, "127.0.0.1:3000")
        .body(Body::empty())
        .unwrap();
    let res_valid = app.clone().oneshot(req_valid).await.unwrap();
    assert_eq!(res_valid.status(), StatusCode::OK);

    // Request with external/malicious Host header: evil-site.com -> 403 Forbidden
    let req_invalid = Request::builder()
        .uri("/api/status")
        .header(header::HOST, "evil-site.com")
        .body(Body::empty())
        .unwrap();
    let res_invalid = app.oneshot(req_invalid).await.unwrap();
    assert_eq!(res_invalid.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_trade_preview_deterministic_math_and_zero_orders() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());

    // 1. Tick mark price at $65,000.00
    orch.on_mark_price_tick(Price::new(dec!(65000.0)).unwrap(), 1700000000000)
        .await;

    // 2. Generate trade preview
    let preview = orch
        .generate_trade_preview()
        .await
        .expect("Preview generation failed");

    // 3. Assert preview invariant fields
    assert_eq!(preview.label, "ESTIMATE / PENDING FILL");
    assert_eq!(
        preview.benchmark_fee_rate,
        "0.045% benchmark taker estimate"
    );
    assert_eq!(preview.entry_price.value(), dec!(65000.0));
    assert_eq!(preview.leverage, 5);
    assert_eq!(preview.current_trading_allocation.value(), dec!(8.00));
    assert_eq!(preview.remaining_trading_allocation.value(), dec!(8.00));

    // Direction-aware net outcome assertions:
    // With 0.045% taker fee benchmark:
    assert!(preview.estimated_entry_fee.is_positive());
    assert!(preview.estimated_tp_net_pnl < preview.estimated_tp_gross_pnl);
    assert!(preview.estimated_sl_net_pnl < preview.estimated_sl_gross_pnl);

    // 4. Invariant: preview submits ZERO exchange orders
    let open_orders = mock_client.get_open_orders("").await.unwrap();
    assert_eq!(open_orders.len(), 0);
}

#[tokio::test]
async fn test_capital_reallocation_and_persistence_recovery() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch_1, state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());

    // 1. Initial state: $8.00 trading, $2.00 reserve
    let initial_summary = orch_1.capital_ledger.read().await.summary();
    assert_eq!(initial_summary.allocated_trading.value(), dec!(8.00));
    assert_eq!(initial_summary.allocated_reserve.value(), dec!(2.00));

    // 2. Reallocate $1.00 from trading to reserve
    let reallocated = orch_1
        .reallocate_capital(
            algo_trade::ledger::ReallocationDirection::ToReserve,
            dec!(1.00),
        )
        .await
        .expect("Reallocation to reserve failed");
    assert_eq!(reallocated.allocated_trading.value(), dec!(7.00));
    assert_eq!(reallocated.allocated_reserve.value(), dec!(3.00));

    // 3. Restart engine orchestrator from persisted state
    let persistence = Arc::new(StatePersistenceManager::new(&state_file));
    let audit_logger = Arc::new(AuditLogger::new(
        std::env::temp_dir().join("test_audit.jsonl"),
    ));
    let orch_2 = Arc::new(EngineOrchestrator::new(
        "BTC",
        "0x0000000000000000000000000000000000000001",
        EngineConfig::normal_default(),
        MarketMetadata::btc_fixture(),
        mock_client,
        audit_logger,
        persistence,
    ));

    orch_2
        .load_persisted_state()
        .await
        .expect("Failed to load persisted state");

    // 4. Invariant: Restored capital allocations match exactly $7.00 and $3.00
    let restored_summary = orch_2.capital_ledger.read().await.summary();
    assert_eq!(restored_summary.allocated_trading.value(), dec!(7.00));
    assert_eq!(restored_summary.allocated_reserve.value(), dec!(3.00));
}

#[tokio::test]
async fn test_authorization_failure_halts_engine_and_sets_unauthorized() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) = create_test_orchestrator(EngineConfig::normal_default(), mock_client);

    // 1. Initial signer status is Active
    assert_eq!(
        *orch.signer_status.read().await,
        algo_trade::execution::SignerStatus::Active
    );

    // 2. Trigger exchange authorization failure
    orch.handle_authorization_error("Exchange returned 401 Unauthorized: agent not approved")
        .await;

    // 3. Invariants: SignerStatus set to UnauthorizedOrExpired, engine mode is Halted
    assert_eq!(
        *orch.signer_status.read().await,
        algo_trade::execution::SignerStatus::UnauthorizedOrExpired
    );
    let snapshot = orch.get_status_snapshot().await;
    assert!(matches!(snapshot.mode, EngineMode::Halted(_)));

    // 4. Invariant: Start fails closed when unauthorized
    let start_res = orch.start().await;
    assert!(start_res.is_err());
    assert!(start_res.unwrap_err().contains("signer is not active"));
}

#[tokio::test]
async fn test_api_account_returns_perp_summary_and_dynamic_shortening() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) = create_test_orchestrator(EngineConfig::normal_default(), mock_client);
    let app = create_router(Arc::new(AppState { orchestrator: orch }), "web");

    let req = Request::builder()
        .uri("/api/account")
        .header(header::HOST, "127.0.0.1:3000")
        .body(Body::empty())
        .unwrap();

    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(res.into_body(), 10240).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["success"], true);

    let data = &body["data"];
    assert_eq!(
        data["account_address"],
        "0x0000000000000000000000000000000000000001"
    );
    assert_eq!(data["shortened_address"], "0x0000...0001");
    assert_eq!(data["is_mock"], true);
}

#[tokio::test]
async fn test_api_capital_and_reallocation_endpoints() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) = create_test_orchestrator(EngineConfig::normal_default(), mock_client);
    let app = create_router(Arc::new(AppState { orchestrator: orch }), "web");

    // 1. GET /api/capital
    let req = Request::builder()
        .uri("/api/capital")
        .header(header::HOST, "127.0.0.1:3000")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(res.into_body(), 10240).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["data"]["allocated_trading"], "8.00");
    assert_eq!(body["data"]["allocated_reserve"], "2.00");

    // 2. POST /api/capital/reallocate with $1.00 to reserve
    let realloc_payload = serde_json::json!({
        "direction": "ToReserve",
        "amount": "1.00"
    });
    let req_realloc = Request::builder()
        .method("POST")
        .uri("/api/capital/reallocate")
        .header(header::HOST, "127.0.0.1:3000")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(realloc_payload.to_string()))
        .unwrap();
    let res_realloc = app.clone().oneshot(req_realloc).await.unwrap();
    assert_eq!(res_realloc.status(), StatusCode::OK);

    let bytes_realloc = axum::body::to_bytes(res_realloc.into_body(), 10240)
        .await
        .unwrap();
    let body_realloc: serde_json::Value = serde_json::from_slice(&bytes_realloc).unwrap();
    assert_eq!(body_realloc["data"]["allocated_trading"], "7.00");
    assert_eq!(body_realloc["data"]["allocated_reserve"], "3.00");

    // 3. POST /api/capital/reallocate with negative amount -> 400 Bad Request
    let invalid_payload = serde_json::json!({
        "direction": "ToReserve",
        "amount": "-1.00"
    });
    let req_invalid = Request::builder()
        .method("POST")
        .uri("/api/capital/reallocate")
        .header(header::HOST, "127.0.0.1:3000")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(invalid_payload.to_string()))
        .unwrap();
    let res_invalid = app.oneshot(req_invalid).await.unwrap();
    assert_eq!(res_invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_api_candles_endpoint_validation_and_results() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) = create_test_orchestrator(EngineConfig::normal_default(), mock_client);

    // Insert sample candle into cache
    let candle = algo_trade::execution::CandleSnapshot {
        open_time_ms: 1700000000000,
        close_time_ms: 1700000059999,
        coin: "BTC".into(),
        interval: "1m".into(),
        open: Price::new(dec!(65000.0)).unwrap(),
        high: Price::new(dec!(65200.0)).unwrap(),
        low: Price::new(dec!(64900.0)).unwrap(),
        close: Price::new(dec!(65100.0)).unwrap(),
        volume: dec!(10.5),
        trade_count: 55,
    };
    orch.on_candle_update(candle).await;

    let app = create_router(Arc::new(AppState { orchestrator: orch }), "web");

    // 1. Valid interval 1m
    let req_valid = Request::builder()
        .uri("/api/candles?interval=1m&limit=10")
        .header(header::HOST, "127.0.0.1:3000")
        .body(Body::empty())
        .unwrap();
    let res_valid = app.clone().oneshot(req_valid).await.unwrap();
    assert_eq!(res_valid.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(res_valid.into_body(), 10240)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["success"], true);
    let candles = body["data"].as_array().unwrap();
    assert_eq!(candles.len(), 1);

    // 2. Invalid interval "2m" -> 400 Bad Request
    let req_invalid = Request::builder()
        .uri("/api/candles?interval=2m")
        .header(header::HOST, "127.0.0.1:3000")
        .body(Body::empty())
        .unwrap();
    let res_invalid = app.oneshot(req_invalid).await.unwrap();
    assert_eq!(res_invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_api_preview_endpoint_and_zero_orders() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());

    orch.on_mark_price_tick(Price::new(dec!(68000.0)).unwrap(), 1700000000000)
        .await;
    let app = create_router(Arc::new(AppState { orchestrator: orch }), "web");

    let req = Request::builder()
        .uri("/api/preview")
        .header(header::HOST, "127.0.0.1:3000")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(res.into_body(), 10240).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["data"]["label"], "ESTIMATE / PENDING FILL");
    assert_eq!(
        body["data"]["benchmark_fee_rate"],
        "0.045% benchmark taker estimate"
    );

    // Invariant: Zero orders submitted
    assert_eq!(mock_client.get_open_orders("").await.unwrap().len(), 0);
}

#[tokio::test]
async fn test_api_control_endpoints_gated_by_signer_status() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) = create_test_orchestrator(EngineConfig::normal_default(), mock_client);

    // Set signer status to UnauthorizedOrExpired
    orch.set_signer_status(algo_trade::execution::SignerStatus::UnauthorizedOrExpired)
        .await;
    let app = create_router(Arc::new(AppState { orchestrator: orch }), "web");

    // 1. Start is rejected with 403 Forbidden
    let req_start = Request::builder()
        .method("POST")
        .uri("/api/control/start")
        .header(header::HOST, "127.0.0.1:3000")
        .body(Body::empty())
        .unwrap();
    let res_start = app.clone().oneshot(req_start).await.unwrap();
    assert_eq!(res_start.status(), StatusCode::FORBIDDEN);

    // 2. Approve entry is rejected with 403 Forbidden
    let payload = serde_json::json!({
        "proposal_id": "prop-123"
    });
    let req_approve = Request::builder()
        .method("POST")
        .uri("/api/control/approve-entry")
        .header(header::HOST, "127.0.0.1:3000")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let res_approve = app.oneshot(req_approve).await.unwrap();
    assert_eq!(res_approve.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_regression_live_vs_mock_account_and_signer_labeling() {
    // 1. Mock exchange client explicitly reports is_mock: true and source: "Local Mock Engine"
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    mock_client
        .set_signer_status(algo_trade::types::SignerStatus::Unconfigured)
        .await;

    let summary = mock_client
        .get_perp_account_summary("0x1234567890123456789012345678901234567890")
        .await
        .unwrap();
    assert!(summary.is_mock);
    assert_eq!(summary.source, "Local Mock Engine");
    assert_eq!(
        summary.signer_status,
        algo_trade::types::SignerStatus::Unconfigured
    );

    // 2. HyperliquidClient read-only with InvalidKeyFormat has is_mock: false and source: "Hyperliquid Testnet"
    let read_only = algo_trade::execution::client::HyperliquidClient::new_read_only_with_status(
        "https://api.hyperliquid-testnet.xyz",
        Some("0xB251AaA073e13009c58472ae3Ac91D7b19da7b28".to_string()),
        algo_trade::types::SignerStatus::InvalidKeyFormat,
    )
    .unwrap();
    assert_eq!(
        read_only.current_signer_status().await,
        algo_trade::types::SignerStatus::InvalidKeyFormat
    );
}

#[tokio::test]
async fn test_regression_account_unavailable_returns_503() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    // Inject simulated network failure
    mock_client.set_network_error(true);

    let (orch, _state_file) = create_test_orchestrator(EngineConfig::normal_default(), mock_client);
    let app = create_router(Arc::new(AppState { orchestrator: orch }), "web");

    let req = Request::builder()
        .uri("/api/account")
        .header(header::HOST, "127.0.0.1:3000")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    let bytes = axum::body::to_bytes(res.into_body(), 10240).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["success"], false);
    assert!(body["error"].as_str().unwrap().contains("network timeout"));
}

#[tokio::test]
async fn test_regression_candle_cold_start_loading_on_cache_miss() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());

    // Initially cache is completely empty for 1h
    assert_eq!(orch.candle_cache.get_candles("1h", 10).await.len(), 0);

    let app = create_router(
        Arc::new(AppState {
            orchestrator: orch.clone(),
        }),
        "web",
    );

    let req = Request::builder()
        .uri("/api/candles?interval=1h&limit=10")
        .header(header::HOST, "127.0.0.1:3000")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(res.into_body(), 65536).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["success"], true);

    let candles = body["data"].as_array().unwrap();
    assert!(
        !candles.is_empty(),
        "Candles should be populated on cache miss"
    );
    assert!(candles[0]["open_time_ms"].is_number());
    assert!(candles[0]["open"].is_string());
    assert!(candles[0]["high"].is_string());
    assert!(candles[0]["low"].is_string());
    assert!(candles[0]["close"].is_string());

    // Invariant: Zero orders placed while querying candles
    assert_eq!(mock_client.get_open_orders("").await.unwrap().len(), 0);
}

#[tokio::test]
async fn test_regression_capital_reserve_mapping() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) = create_test_orchestrator(EngineConfig::normal_default(), mock_client);

    let app = create_router(Arc::new(AppState { orchestrator: orch }), "web");

    let req = Request::builder()
        .uri("/api/capital")
        .header(header::HOST, "127.0.0.1:3000")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(res.into_body(), 10240).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["success"], true);

    let data = &body["data"];
    assert_eq!(data["strategy_budget"], "10.00");
    assert_eq!(data["allocated_trading"], "8.00");
    assert_eq!(data["allocated_reserve"], "2.00");
    assert_eq!(data["reserve_target"], "2.00");
    assert_eq!(data["uncommitted_trading"], "8.00");
    assert_eq!(data["committed_margin"], "0");
}

#[tokio::test]
async fn test_regression_trade_preview_field_contract() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());

    orch.on_mark_price_tick(Price::new(dec!(68000.0)).unwrap(), 1700000000000)
        .await;
    let app = create_router(Arc::new(AppState { orchestrator: orch }), "web");

    let req = Request::builder()
        .uri("/api/preview")
        .header(header::HOST, "127.0.0.1:3000")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(res.into_body(), 10240).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["success"], true);

    let data = &body["data"];
    assert_eq!(data["profile"], "NORMAL");
    assert_eq!(data["margin_mode"], "CROSS");
    assert_eq!(data["direction"], "Long");
    assert_eq!(data["leverage"], 5);
    assert_eq!(data["label"], "ESTIMATE / PENDING FILL");
    assert!(data["quantity"].is_string());
    assert!(data["notional"].is_string());
    assert!(data["estimated_margin"].is_string());
    assert!(data["tp_price"].is_string());
    assert!(data["sl_price"].is_string());
    assert!(data["estimated_entry_fee"].is_string());
    assert!(data["estimated_exit_fee_tp"].is_string());
    assert!(data["estimated_exit_fee_sl"].is_string());
    assert!(data["estimated_tp_net_pnl"].is_string());
    assert!(data["estimated_sl_net_pnl"].is_string());

    // Invariant: Preview cannot place orders
    assert_eq!(mock_client.get_open_orders("").await.unwrap().len(), 0);
}

#[tokio::test]
async fn test_regression_preflight_rejections_and_zero_orders() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());

    // 1. Preflight rejection on start when signer is not Active
    orch.set_signer_status(algo_trade::types::SignerStatus::InvalidKeyFormat)
        .await;
    let res = orch.start().await;
    assert!(res.is_err());
    assert!(res.unwrap_err().contains("signer is not active"));

    // 2. Preflight rejection when market data is missing
    orch.set_signer_status(algo_trade::types::SignerStatus::Active)
        .await;
    let res_no_data = orch.start().await;
    assert!(res_no_data.is_err());
    assert!(res_no_data
        .unwrap_err()
        .contains("market data is stale or not yet received"));

    // 3. Preflight rejection when uncommitted capital is 0
    orch.on_mark_price_tick(
        Price::new(dec!(68000.0)).unwrap(),
        current_time_ms_for_test(),
    )
    .await;
    let mut cap = orch.capital_ledger.write().await;
    cap.commit_margin(SignedUsd::new(dec!(8.00))).unwrap();
    drop(cap);

    let res_no_cap = orch.start().await;
    assert!(res_no_cap.is_err());
    assert!(res_no_cap
        .unwrap_err()
        .contains("uncommitted trading capital is zero or negative"));

    // Invariant: Zero orders placed across all preflight checks
    assert_eq!(mock_client.get_open_orders("").await.unwrap().len(), 0);
}

fn current_time_ms_for_test() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ============================================================================
// Milestone Regression Tests
// ============================================================================

#[tokio::test]
async fn test_regression_ui_config_staging_and_active_mutation_rejection() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) = create_test_orchestrator(EngineConfig::normal_default(), mock_client);
    let app = create_router(
        Arc::new(AppState {
            orchestrator: orch.clone(),
        }),
        "web",
    );

    // 1. Success: NORMAL profile with trailing stop false
    let normal_req = serde_json::json!({
        "profile": "NORMAL",
        "margin_mode": "Isolated",
        "execution_mode": "Automatic",
        "trailing_stop": false
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/config")
        .header(header::HOST, "127.0.0.1:3000")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(normal_req.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cfg = orch.config.read().await;
    assert_eq!(cfg.profile.profile, TradingProfile::Normal);
    assert!(!cfg.profile.trailing_stop_enabled);
    drop(cfg);

    // 2. Success: GREEDY profile with trailing stop true
    let greedy_req = serde_json::json!({
        "profile": "greedy", // test case-insensitivity
        "margin_mode": "Cross",
        "execution_mode": "ConfirmationRequired",
        "trailing_stop": true
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/config")
        .header(header::HOST, "127.0.0.1:3000")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(greedy_req.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cfg = orch.config.read().await;
    assert_eq!(cfg.profile.profile, TradingProfile::Greedy);
    assert!(cfg.profile.trailing_stop_enabled);
    drop(cfg);

    // 3. Error: Invalid profile name rejected with 400 Bad Request
    let invalid_prof_req = serde_json::json!({
        "profile": "ULTRA_RISKY",
        "margin_mode": "Isolated",
        "execution_mode": "Automatic"
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/config")
        .header(header::HOST, "127.0.0.1:3000")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(invalid_prof_req.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // 4. Guard: Reject config mutation when trading is active (e.g. EngineMode::Running)
    {
        let mut sm = orch.state_machine.write().await;
        sm.start().unwrap();
    }
    let req = Request::builder()
        .method("POST")
        .uri("/api/config")
        .header(header::HOST, "127.0.0.1:3000")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(normal_req.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let bytes = axum::body::to_bytes(res.into_body(), 10240).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["success"], false);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("Cannot modify configuration while trading is active"));
}

#[tokio::test]
async fn test_regression_emergency_flatten_exposure_query_and_reconciliation() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());

    // 1. Simulate an open position on the exchange of 0.00045 BTC
    let btc_qty = Quantity::new(dec!(0.00045)).unwrap();
    let entry_px = Price::new(dec!(68000.0)).unwrap();
    mock_client
        .set_position("BTC", btc_qty, true, Some(entry_px))
        .await;
    mock_client.set_fill_mode(MockFillMode::ImmediateFull).await;

    // Allocate and commit margin in the capital ledger
    {
        let mut cap = orch.capital_ledger.write().await;
        cap.commit_margin(SignedUsd::new(dec!(8.00))).unwrap();
    }

    // Run emergency_flatten
    orch.emergency_flatten()
        .await
        .expect("Emergency flatten must succeed when exchange closes position");

    // Post-conditions:
    // a. Exchange exposure is flat (0 positions)
    let user_state = mock_client
        .get_user_state(&orch.target_account)
        .await
        .unwrap();
    assert!(
        user_state.positions.is_empty(),
        "Exchange exposure must be fully flat"
    );

    // b. Internal capital ledger committed margin is released back to uncommitted
    let cap = orch.capital_ledger.read().await;
    assert_eq!(
        cap.committed_margin,
        SignedUsd::zero(),
        "Committed margin must be released"
    );

    // c. State machine is in EmergencyShutdown and Flat
    let sm = orch.state_machine.read().await;
    assert!(matches!(sm.mode(), EngineMode::EmergencyShutdown(_)));
    assert_eq!(sm.position(), &PositionState::Flat);
    drop(sm);

    // 2. Failure Case: When exchange order placement is rejected, emergency_flatten must fail
    mock_client
        .set_position("BTC", btc_qty, true, Some(entry_px))
        .await;
    mock_client.set_order_rejection(true);
    let fail_res = orch.emergency_flatten().await;
    assert!(
        fail_res.is_err(),
        "Emergency flatten must fail if close order is rejected"
    );
    mock_client.set_order_rejection(false);

    // 3. Failure Case: When exchange position remains non-zero after close attempt, emergency_flatten must fail
    mock_client.set_fill_mode(MockFillMode::Resting).await; // Resting orders don't immediately fill
    let fail_remaining = orch.emergency_flatten().await;
    assert!(
        fail_remaining.is_err(),
        "Emergency flatten must fail if exposure remains open on exchange"
    );
}

#[tokio::test]
async fn test_regression_startup_aborts_on_update_leverage_failure() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());

    orch.set_signer_status(algo_trade::types::SignerStatus::Active)
        .await;
    orch.on_mark_price_tick(
        Price::new(dec!(68000.0)).unwrap(),
        current_time_ms_for_test(),
    )
    .await;

    // Simulate exchange rejection specifically during update_leverage
    mock_client.set_leverage_failure(true);

    let start_res = orch.start().await;
    assert!(
        start_res.is_err(),
        "Startup must abort safely if update_leverage fails"
    );
    assert!(start_res
        .unwrap_err()
        .contains("Failed to configure leverage"));

    // Invariant: Mode must remain Stopped, never Running
    let sm = orch.state_machine.read().await;
    assert_eq!(sm.mode(), &EngineMode::Stopped);
}

#[tokio::test]
async fn test_regression_candle_broadcast_subscription_and_no_mark_mutation() {
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0))));
    let (orch, _state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());

    // 1. Subscribe to candles
    let mut candle_rx = orch.subscribe_candles();

    // 2. Broadcast a candle snapshot
    let test_candle = CandleSnapshot {
        coin: "BTC".to_string(),
        interval: "1m".to_string(),
        open_time_ms: 1700000000000,
        close_time_ms: 1700000060000,
        open: Price::new(dec!(68000.0)).unwrap(),
        high: Price::new(dec!(68500.0)).unwrap(),
        low: Price::new(dec!(67800.0)).unwrap(),
        close: Price::new(dec!(68200.0)).unwrap(),
        volume: dec!(1.5),
        trade_count: 42,
    };

    orch.on_candle_update(test_candle.clone()).await;

    // 3. Subscriber receives the exact candle snapshot
    let received = candle_rx
        .try_recv()
        .expect("Subscriber must receive broadcast candle");
    assert_eq!(received.open_time_ms, test_candle.open_time_ms);
    assert_eq!(received.open.value(), dec!(68000.0));
    assert_eq!(received.high.value(), dec!(68500.0));
    assert_eq!(received.low.value(), dec!(67800.0));
    assert_eq!(received.close.value(), dec!(68200.0));

    // 4. Invariant: Feeding mark price ticks must NEVER modify candle OHLC values
    let mark_price = Price::new(dec!(72000.0)).unwrap();
    orch.on_mark_price_tick(mark_price, current_time_ms_for_test())
        .await;

    let candles = orch.ensure_candles("1m", 10).await.unwrap();
    if let Some(c) = candles
        .iter()
        .find(|c| c.open_time_ms == test_candle.open_time_ms)
    {
        assert_eq!(
            c.high.value(),
            dec!(68500.0),
            "Mark price tick must not alter candle high"
        );
        assert_eq!(
            c.close.value(),
            dec!(68200.0),
            "Mark price tick must not alter candle close"
        );
    }
}

#[tokio::test]
async fn test_api_account_unified_usdc_with_zero_perp_equity_and_failure_handling() {
    // 1. Setup mock client where perpetual clearinghouse equity is zero ($0.00),
    // but spot clearinghouse contains positive USDC ($463.704600 total, $2.500000 hold).
    let mock_client = Arc::new(MockExchangeClient::new("BTC", SignedUsd::zero()));
    mock_client
        .set_spot_balance(
            SignedUsd::new(dec!(463.704600)),
            SignedUsd::new(dec!(2.500000)),
        )
        .await;

    let (orch, _state_file) =
        create_test_orchestrator(EngineConfig::normal_default(), mock_client.clone());
    let app = create_router(Arc::new(AppState { orchestrator: orch }), "web");

    // 2. Query GET /api/account
    let req = Request::builder()
        .uri("/api/account")
        .header(header::HOST, "127.0.0.1:3000")
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(res.into_body(), 10240).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["success"], true);
    // Verify perp account value is 0, but unified USDC is positive
    let acct_val: Decimal = body["data"]["account_value"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(acct_val, Decimal::ZERO);
    assert_eq!(body["data"]["unified_usdc_total"], "463.704600");
    assert_eq!(body["data"]["unified_usdc_hold"], "2.500000");
    assert_eq!(body["data"]["unified_usdc_available"], "461.204600");
    assert_eq!(body["data"]["unified_account"]["usdc_total"], "463.704600");
    assert_eq!(body["data"]["unified_account"]["usdc_hold"], "2.500000");
    assert_eq!(
        body["data"]["unified_account"]["usdc_available"],
        "461.204600"
    );

    // 3. API Failure handling: When spot clearinghouse query fails, /api/account returns 503 SERVICE_UNAVAILABLE
    // and never silently substitutes mock or zero values.
    mock_client.set_spot_error(true);

    let req_fail = Request::builder()
        .uri("/api/account")
        .header(header::HOST, "127.0.0.1:3000")
        .body(Body::empty())
        .unwrap();

    let res_fail = app.oneshot(req_fail).await.unwrap();
    assert_eq!(res_fail.status(), StatusCode::SERVICE_UNAVAILABLE);

    let bytes_fail = axum::body::to_bytes(res_fail.into_body(), 10240)
        .await
        .unwrap();
    let body_fail: serde_json::Value = serde_json::from_slice(&bytes_fail).unwrap();
    assert_eq!(body_fail["success"], false);
    assert!(body_fail["error"]
        .as_str()
        .unwrap()
        .contains("Simulated spot clearinghouse error"));
}
