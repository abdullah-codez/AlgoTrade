//! Integration test suite verifying hand-calculated financial arithmetic, risk thresholds,
//! direction-aware P&L, funding integration, and halt-reason priorities.

use algo_trade::config::{MarketMetadata, ProfileConfig};
use algo_trade::ledger::{FillLedger, FillRecord, FundingLedger, FundingRecord, NewFillParams};
use algo_trade::risk::{
    calculate_mtm_session_equity_change, calculate_order_size, calculate_unrealized_gross_pnl,
    ConsecutiveLossTracker,
};
use algo_trade::state::{EngineMode, HaltReason, TradingStateMachine};
use algo_trade::strategy::{calculate_tp_sl_prices, determine_next_direction};
use algo_trade::types::{Direction, Price, Quantity, Side, SignedUsd};
use chrono::Utc;
use rust_decimal_macros::dec;

#[test]
fn test_hand_calculated_normal_two_win_halt() {
    let cfg = ProfileConfig::normal();
    cfg.validate().unwrap();
    let metadata = MarketMetadata::btc_fixture();

    let entry_px = Price::new(dec!(70000.0)).unwrap();
    let sizing = calculate_order_size(
        cfg.initial_margin_cap,
        cfg.leverage,
        entry_px,
        &metadata,
        cfg.min_order_notional,
    )
    .unwrap();

    // Sizing assertions:
    // Max notional = $8 * 5 = $40.00. 40 / 70000 = 0.0005714... -> rounded down to 0.00057 BTC
    assert_eq!(sizing.quantity.value(), dec!(0.00057));
    assert_eq!(sizing.notional.value(), dec!(39.90));
    assert_eq!(sizing.deployed_margin.value(), dec!(7.98));

    let mut ledger = FillLedger::new();
    let mut state_machine = TradingStateMachine::new();
    let now = Utc::now();

    // --- TRADE 1: Long Entry & 1.0% TP Exit ---
    let tp_levels = calculate_tp_sl_prices(
        entry_px,
        Direction::Long,
        cfg.take_profit_pct,
        cfg.stop_loss_pct,
    );
    assert_eq!(tp_levels.take_profit_price.value(), dec!(70700.0));

    let entry_fill_1 = FillRecord::new(NewFillParams {
        fill_id: "fill-1".into(),
        cloid: Some("cloid-e1".into()),
        timestamp: now,
        direction: Direction::Long,
        side: Side::Buy,
        price: entry_px,
        quantity: sizing.quantity,
        fee_paid: SignedUsd::new(sizing.notional.value() * cfg.base_taker_fee_rate), // 39.90 * 0.00045 = 0.017955
        fee_token: "USDC".into(),
        is_entry: true,
    })
    .unwrap();
    ledger.add_entry_fill(entry_fill_1);

    let exit_notional_1 = tp_levels.take_profit_price.value() * sizing.quantity.value(); // 70700 * 0.00057 = 40.299
    let exit_fill_1 = FillRecord::new(NewFillParams {
        fill_id: "fill-2".into(),
        cloid: Some("cloid-x1".into()),
        timestamp: now,
        direction: Direction::Long,
        side: Side::Sell,
        price: tp_levels.take_profit_price,
        quantity: sizing.quantity,
        fee_paid: SignedUsd::new(exit_notional_1 * cfg.base_taker_fee_rate), // 40.299 * 0.00045 = 0.01813455
        fee_token: "USDC".into(),
        is_entry: false,
    })
    .unwrap();

    let matched_1 = ledger
        .match_exit_fill(&exit_fill_1, SignedUsd::zero())
        .unwrap();
    assert_eq!(matched_1.len(), 1);
    let trade_1 = &matched_1[0];

    // Gross P&L = (70700 - 70000) * 0.00057 = 700 * 0.00057 = $0.3990
    assert_eq!(trade_1.gross_pnl.value(), dec!(0.3990));
    // Fees = 0.017955 + 0.01813455 = 0.03608955
    // Net P&L = 0.3990 - 0.03608955 = $0.36291045
    assert_eq!(trade_1.net_pnl.value(), dec!(0.36291045));
    assert_eq!(ledger.total_realized_net_pnl().value(), dec!(0.36291045));

    // Evaluate halt condition after Trade 1:
    // Profit target = $0.70. Cumulative = $0.3629 < $0.70. TP count = 1 < 2. Continue!
    assert!(ledger.total_realized_net_pnl().value() < cfg.net_session_profit_target.value());

    // --- TRADE 2: Same-direction re-entry following TP ---
    let (next_dir, _) = determine_next_direction(true, Direction::Long);
    assert_eq!(next_dir, Direction::Long);

    let entry_fill_2 = FillRecord::new(NewFillParams {
        fill_id: "fill-3".into(),
        cloid: Some("cloid-e2".into()),
        timestamp: now,
        direction: Direction::Long,
        side: Side::Buy,
        price: entry_px,
        quantity: sizing.quantity,
        fee_paid: SignedUsd::new(sizing.notional.value() * cfg.base_taker_fee_rate),
        fee_token: "USDC".into(),
        is_entry: true,
    })
    .unwrap();
    ledger.add_entry_fill(entry_fill_2);

    let exit_fill_2 = FillRecord::new(NewFillParams {
        fill_id: "fill-4".into(),
        cloid: Some("cloid-x2".into()),
        timestamp: now,
        direction: Direction::Long,
        side: Side::Sell,
        price: tp_levels.take_profit_price,
        quantity: sizing.quantity,
        fee_paid: SignedUsd::new(exit_notional_1 * cfg.base_taker_fee_rate),
        fee_token: "USDC".into(),
        is_entry: false,
    })
    .unwrap();

    let matched_2 = ledger
        .match_exit_fill(&exit_fill_2, SignedUsd::zero())
        .unwrap();
    let trade_2 = &matched_2[0];
    assert_eq!(trade_2.net_pnl.value(), dec!(0.36291045));

    // Total session net P&L = 2 * 0.36291045 = $0.72582090
    assert_eq!(ledger.total_realized_net_pnl().value(), dec!(0.72582090));

    // Evaluate halt condition:
    // $0.72582090 >= $0.70 (target reached!) AND count = 2.
    // Strict priority check: ProfitTargetReached must be selected because profit target check precedes count.
    if ledger.total_realized_net_pnl().value() >= cfg.net_session_profit_target.value() {
        state_machine.halt(HaltReason::ProfitTargetReached);
    } else if ledger.closed_trades().len() as u32 >= cfg.take_profit_target_count {
        state_machine.halt(HaltReason::TakeProfitCountReached);
    }

    assert_eq!(
        *state_machine.mode(),
        EngineMode::Halted(HaltReason::ProfitTargetReached)
    );
}

#[test]
fn test_hand_calculated_short_stop_loss_reversal_and_loss_streak_halt() {
    let cfg = ProfileConfig::normal();
    let metadata = MarketMetadata::btc_fixture();
    let entry_px = Price::new(dec!(70000.0)).unwrap();
    let sizing = calculate_order_size(
        cfg.initial_margin_cap,
        cfg.leverage,
        entry_px,
        &metadata,
        cfg.min_order_notional,
    )
    .unwrap();

    let mut ledger = FillLedger::new();
    let mut loss_tracker = ConsecutiveLossTracker::new();
    let mut state_machine = TradingStateMachine::new();
    let now = Utc::now();

    // Short TP & SL levels
    let levels = calculate_tp_sl_prices(
        entry_px,
        Direction::Short,
        cfg.take_profit_pct,
        cfg.stop_loss_pct,
    );
    assert_eq!(levels.take_profit_price.value(), dec!(69300.0));
    assert_eq!(levels.stop_loss_price.value(), dec!(70350.0)); // 0.5% adverse price move

    // --- TRADE 1: Short Entry & SL Exit ---
    let entry_fill_1 = FillRecord::new(NewFillParams {
        fill_id: "fill-s1".into(),
        cloid: Some("cloid-se1".into()),
        timestamp: now,
        direction: Direction::Short,
        side: Side::Sell,
        price: entry_px,
        quantity: sizing.quantity,
        fee_paid: SignedUsd::new(sizing.notional.value() * cfg.base_taker_fee_rate), // 39.90 * 0.00045 = 0.017955
        fee_token: "USDC".into(),
        is_entry: true,
    })
    .unwrap();
    ledger.add_entry_fill(entry_fill_1);

    let exit_notional_1 = levels.stop_loss_price.value() * sizing.quantity.value(); // 70350 * 0.00057 = 40.0995
    let exit_fill_1 = FillRecord::new(NewFillParams {
        fill_id: "fill-s2".into(),
        cloid: Some("cloid-sx1".into()),
        timestamp: now,
        direction: Direction::Short,
        side: Side::Buy,
        price: levels.stop_loss_price,
        quantity: sizing.quantity,
        fee_paid: SignedUsd::new(exit_notional_1 * cfg.base_taker_fee_rate), // 40.0995 * 0.00045 = 0.018044775
        fee_token: "USDC".into(),
        is_entry: false,
    })
    .unwrap();

    let matched_1 = ledger
        .match_exit_fill(&exit_fill_1, SignedUsd::zero())
        .unwrap();
    let trade_1 = &matched_1[0];

    // Gross P&L = (70350 - 70000) * 0.00057 * (-1) = 350 * 0.00057 * (-1) = -$0.1995
    assert_eq!(trade_1.gross_pnl.value(), dec!(-0.1995));
    // Fees = 0.017955 + 0.018044775 = 0.035999775
    // Net P&L = -0.1995 - 0.035999775 = -$0.235499775
    assert_eq!(trade_1.net_pnl.value(), dec!(-0.235499775));

    let streak_1 = loss_tracker.record_closed_trade(trade_1.net_pnl);
    assert_eq!(streak_1, 1);
    assert!(!loss_tracker.has_breached_limit(cfg.consecutive_losses_limit));

    // Confirm reversal direction after SL
    let (next_dir, _) = determine_next_direction(false, Direction::Short);
    assert_eq!(next_dir, Direction::Long);

    // --- TRADE 2: Reversed Long Entry & SL Exit ---
    let long_levels = calculate_tp_sl_prices(
        entry_px,
        Direction::Long,
        cfg.take_profit_pct,
        cfg.stop_loss_pct,
    );
    assert_eq!(long_levels.stop_loss_price.value(), dec!(69650.0)); // 0.5% adverse price move

    let entry_fill_2 = FillRecord::new(NewFillParams {
        fill_id: "fill-l1".into(),
        cloid: Some("cloid-le1".into()),
        timestamp: now,
        direction: Direction::Long,
        side: Side::Buy,
        price: entry_px,
        quantity: sizing.quantity,
        fee_paid: SignedUsd::new(sizing.notional.value() * cfg.base_taker_fee_rate),
        fee_token: "USDC".into(),
        is_entry: true,
    })
    .unwrap();
    ledger.add_entry_fill(entry_fill_2);

    let exit_notional_2 = long_levels.stop_loss_price.value() * sizing.quantity.value(); // 69650 * 0.00057 = 39.7005
    let exit_fill_2 = FillRecord::new(NewFillParams {
        fill_id: "fill-l2".into(),
        cloid: Some("cloid-lx1".into()),
        timestamp: now,
        direction: Direction::Long,
        side: Side::Sell,
        price: long_levels.stop_loss_price,
        quantity: sizing.quantity,
        fee_paid: SignedUsd::new(exit_notional_2 * cfg.base_taker_fee_rate), // 39.7005 * 0.00045 = 0.017865225
        fee_token: "USDC".into(),
        is_entry: false,
    })
    .unwrap();

    let matched_2 = ledger
        .match_exit_fill(&exit_fill_2, SignedUsd::zero())
        .unwrap();
    let trade_2 = &matched_2[0];
    // Gross P&L = (69650 - 70000) * 0.00057 * (+1) = -350 * 0.00057 = -$0.1995
    assert_eq!(trade_2.gross_pnl.value(), dec!(-0.1995));

    let streak_2 = loss_tracker.record_closed_trade(trade_2.net_pnl);
    assert_eq!(streak_2, 2);
    assert!(loss_tracker.has_breached_limit(cfg.consecutive_losses_limit));

    // Evaluation: Streak limit reached -> Halt!
    state_machine.halt(HaltReason::ConsecutiveLossesReached);
    assert_eq!(
        *state_machine.mode(),
        EngineMode::Halted(HaltReason::ConsecutiveLossesReached)
    );
}

#[test]
fn test_mark_to_market_hard_loss_breach_detection() {
    let cfg = ProfileConfig::normal(); // Hard loss limit = -$1.00
    let realized_net_pnl = SignedUsd::new(dec!(-0.47)); // Previous closed loss
    let entry_px = Price::new(dec!(70000.0)).unwrap();
    let qty = Quantity::new(dec!(0.00057)).unwrap();

    // Sudden adverse price wick to $68,900 (-1.57% drop)
    let current_mark = Price::new(dec!(68900.0)).unwrap();
    let unrealized_gross =
        calculate_unrealized_gross_pnl(entry_px, current_mark, qty, Direction::Long);
    // (68900 - 70000) * 0.00057 = -1100 * 0.00057 = -$0.6270
    assert_eq!(unrealized_gross.value(), dec!(-0.6270));

    let mtm_session = calculate_mtm_session_equity_change(
        realized_net_pnl,
        unrealized_gross,
        qty,
        current_mark,
        cfg.base_taker_fee_rate, // 0.00045 exit fee
        dec!(0.0002),            // 0.02% estimated exit slippage
    );

    // MTM = -0.47 + (-0.6270) - (68900 * 0.00057 * 0.00065 = 39.273 * 0.00065 = 0.02552745)
    // MTM = -1.0970 - 0.0255 = -$1.1225 <= -$1.00
    assert!(mtm_session.value() <= cfg.hard_session_loss_limit.value());

    let mut state_machine = TradingStateMachine::new();
    state_machine.halt(HaltReason::HardLossExceeded);
    assert_eq!(
        *state_machine.mode(),
        EngineMode::Halted(HaltReason::HardLossExceeded)
    );
}

#[test]
fn test_funding_ledger_integration() {
    let mut funding_ledger = FundingLedger::new();
    let now = Utc::now();

    // 1. Long position pays funding: cost = -$0.0050 cash outflow
    funding_ledger.add_funding_record(FundingRecord {
        timestamp: now,
        coin: "BTC".into(),
        rate: dec!(0.0001),
        position_size: Quantity::new(dec!(0.00057)).unwrap(),
        payment: SignedUsd::new(dec!(-0.0050)),
    });

    // 2. Short position receives funding: rebate = +$0.0020 cash inflow
    funding_ledger.add_funding_record(FundingRecord {
        timestamp: now,
        coin: "BTC".into(),
        rate: dec!(0.0001),
        position_size: Quantity::new(dec!(0.00057)).unwrap(),
        payment: SignedUsd::new(dec!(0.0020)),
    });

    // Net signed funding cash flow = -0.0050 + 0.0020 = -$0.0030 (net outflow)
    assert_eq!(
        funding_ledger.total_signed_funding_cash_flow().value(),
        dec!(-0.0030)
    );
    // Net funding cost = -(-0.0030) = +$0.0030
    assert_eq!(
        funding_ledger.total_net_funding_cost().value(),
        dec!(0.0030)
    );
}

#[test]
fn test_state_machine_prevents_unflat_direction_flipping() {
    let mut sm = TradingStateMachine::new_running();

    // Open a Long position
    sm.transition_to_opening(
        "cloid-1".into(),
        Quantity::new(dec!(0.00057)).unwrap(),
        Side::Buy,
        Direction::Long,
    )
    .unwrap();
    sm.transition_to_open_protected(
        Direction::Long,
        Quantity::new(dec!(0.00057)).unwrap(),
        Price::new(dec!(70000.0)).unwrap(),
        "tp-1".into(),
        "sl-1".into(),
    )
    .unwrap();

    // Attempting to open a Short order directly while in OpenProtected state must fail
    let res = sm.transition_to_opening(
        "cloid-2".into(),
        Quantity::new(dec!(0.00057)).unwrap(),
        Side::Sell,
        Direction::Short,
    );
    assert!(res.is_err());

    // Transition properly: OpenProtected -> Closing -> Flat
    sm.transition_to_closing(
        "cloid-close".into(),
        Direction::Long,
        Quantity::new(dec!(0.00057)).unwrap(),
        "SL".into(),
    )
    .unwrap();
    sm.transition_to_flat().unwrap();

    // Now opening opposite Short direction succeeds
    assert!(sm
        .transition_to_opening(
            "cloid-2".into(),
            Quantity::new(dec!(0.00057)).unwrap(),
            Side::Sell,
            Direction::Short
        )
        .is_ok());
}
